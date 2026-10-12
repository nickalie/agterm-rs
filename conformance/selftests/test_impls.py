import json
import os
import pwd
import re
import shutil
import signal
import subprocess
import threading
import traceback
from collections.abc import Iterator
from pathlib import Path

import pytest
from agterm_conformance.impls import ADAPTERS, base, make_adapter, swift_macos
from agterm_conformance.impls.base import (
    CAPABILITIES,
    DEFAULT_APP_ID,
    NO_PROFILE,
    NOOP_DESKTOP_ID,
    NOOP_MIME_TYPES,
    SOCKET_PATH_CAP,
    Adapter,
    Instance,
    LaunchError,
    LaunchRefused,
    LeftoverProcesses,
    ProcessIdentity,
    Profile,
    StopError,
    check_app_id,
    check_launch_env,
    find_leftovers,
    fnv1a64,
    hosts_zmx_dir,
    isolated_runtime_env,
    process_identity,
    version_tuple,
    zmx_socket_directory,
)
from agterm_conformance.impls.rust import ProfileError, RustAdapter, profile_from_version
from agterm_conformance.impls.swift_linux import SwiftLinuxAdapter
from agterm_conformance.impls.swift_macos import SwiftMacosAdapter
from agterm_conformance.plugin import ImplOptions


@pytest.fixture
def daemons(tmp_path: Path) -> Iterator[list[subprocess.Popen[bytes]]]:
    spawned: list[subprocess.Popen[bytes]] = []
    yield spawned
    for process in spawned:
        if process.stdin:
            process.stdin.close()
        process.wait(timeout=5)


def spawn_daemon(
    tmp_path: Path, name: str, zmx_dir: str, spawned: list[subprocess.Popen[bytes]], directory: str = "daemons"
) -> int:
    """A copy of `cat` under `name`, blocked on a stdin pipe the test holds."""
    program = tmp_path / directory / name
    program.parent.mkdir(exist_ok=True)
    if not program.exists():
        shutil.copy2(shutil.which("cat") or "/bin/cat", program)
    process = subprocess.Popen([program], stdin=subprocess.PIPE, env={"ZMX_DIR": zmx_dir})
    spawned.append(process)
    return process.pid


def env_for(root: Path, **overrides: str) -> dict[str, str]:
    env = {
        "HOME": str(root / "home"),
        "AGTERM_STATE_DIR": str(root / "state"),
        "AGTERM_CONTROL_SOCKET": str(root / "state" / "agterm.sock"),
        "AGTERM_APP_ID": f"{DEFAULT_APP_ID}.conformance.test",
    }
    env.update(overrides)
    return env


# -- guards


def test_launch_env_inside_root_is_accepted(tmp_path: Path) -> None:
    check_launch_env(env_for(tmp_path), tmp_path, needs_app_id=True)


@pytest.mark.parametrize("name", ["AGTERM_CONTROL_SOCKET", "AGTERM_STATE_DIR", "HOME"])
def test_path_outside_root_is_refused(tmp_path: Path, name: str) -> None:
    root = tmp_path / "root"
    outside = tmp_path / "elsewhere" / "agterm.sock"
    with pytest.raises(LaunchRefused, match=name):
        check_launch_env(env_for(root, **{name: str(outside)}), root, needs_app_id=True)


def test_socket_symlinked_out_of_root_is_refused(tmp_path: Path) -> None:
    root = tmp_path / "root"
    (root / "state").mkdir(parents=True)
    (root / "state" / "agterm.sock").symlink_to(tmp_path / "live.sock")
    with pytest.raises(LaunchRefused, match="AGTERM_CONTROL_SOCKET"):
        check_launch_env(env_for(root), root, needs_app_id=True)


def test_socket_path_at_cap_is_refused_and_one_under_is_accepted() -> None:
    parent, tail = Path("/tmp"), "/state/agterm.sock"
    padding = SOCKET_PATH_CAP - 1 - len(str(parent)) - 1 - len(tail)
    under = parent / ("r" * padding)
    assert len(f"{under}{tail}") == SOCKET_PATH_CAP - 1
    check_launch_env(env_for(under), under, needs_app_id=True)
    over = parent / ("r" * (padding + 1))
    with pytest.raises(LaunchRefused, match="cap"):
        check_launch_env(env_for(over), over, needs_app_id=True)


@pytest.mark.parametrize(
    ("app_id", "message"),
    [
        (None, "required"),
        ("", "required"),
        (DEFAULT_APP_ID, "installed app"),
        ("agterm", "not a valid"),
        ("io.github.1agterm", "not a valid"),
        ("io..agterm", "not a valid"),
        ("io.github.agterm test", "not a valid"),
    ],
)
def test_bad_app_id_is_refused(app_id: str | None, message: str) -> None:
    with pytest.raises(LaunchRefused, match=message):
        check_app_id(app_id)


def test_missing_app_id_is_refused_only_where_needed(tmp_path: Path) -> None:
    env = env_for(tmp_path)
    del env["AGTERM_APP_ID"]
    check_launch_env(env, tmp_path, needs_app_id=False)
    with pytest.raises(LaunchRefused, match="AGTERM_APP_ID is required"):
        check_launch_env(env, tmp_path, needs_app_id=True)


def test_inherited_agterm_variable_in_launch_env_is_refused(tmp_path: Path) -> None:
    with pytest.raises(LaunchRefused, match="AGTERM_SESSION_ID"):
        check_launch_env(env_for(tmp_path, AGTERM_SESSION_ID="live"), tmp_path, needs_app_id=True)
    with pytest.raises(LaunchRefused, match="ZMX_DIR"):
        check_launch_env(env_for(tmp_path, ZMX_DIR="/tmp/live"), tmp_path, needs_app_id=True)


def test_build_env_drops_inherited_live_session_variables(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    for name in ("AGTERM_SOCKET", "AGTERM_SESSION_ID", "AGTERM_STATE_DIR", "ZMX_DIR", "ZMX_SESSION"):
        monkeypatch.setenv(name, "/live")
    monkeypatch.setenv("CONFORMANCE_KEEP", "1")
    env = SwiftLinuxAdapter(tmp_path / "agterm-linux").build_env(tmp_path)
    assert not {"AGTERM_SOCKET", "AGTERM_SESSION_ID", "ZMX_DIR", "ZMX_SESSION"} & env.keys()
    assert env["AGTERM_STATE_DIR"] == str(tmp_path / "state")
    assert env["CONFORMANCE_KEEP"] == "1"
    assert env["PATH"] == "/usr/bin:/bin"
    assert env["SHELL"] == "/bin/sh"
    assert env["AGTERM_APP_ID"].startswith(DEFAULT_APP_ID + ".")
    check_launch_env(env, tmp_path, needs_app_id=True)


def test_build_env_drops_the_callers_zsh_startup_directory(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ZDOTDIR", "/home/user/.config/zsh")
    monkeypatch.setenv("GHOSTTY_ZSH_ZDOTDIR", "/home/user/.config/zsh")
    for adapter in (SwiftLinuxAdapter(tmp_path / "agterm-linux"), SwiftMacosAdapter(tmp_path / "agterm.app")):
        assert not {"ZDOTDIR", "GHOSTTY_ZSH_ZDOTDIR"} & adapter.build_env(tmp_path).keys()


@pytest.mark.parametrize(
    ("parent", "expected"),
    [
        (
            {"XDG_RUNTIME_DIR": "/run/user/7", "WAYLAND_DISPLAY": "wayland-1"},
            {"WAYLAND_DISPLAY": "/run/user/7/wayland-1", "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/7/bus"},
        ),
        (
            {
                "XDG_RUNTIME_DIR": "/run/user/7",
                "WAYLAND_DISPLAY": "/srv/wl",
                "DBUS_SESSION_BUS_ADDRESS": "unix:path=/b",
            },
            {"WAYLAND_DISPLAY": "/srv/wl", "DBUS_SESSION_BUS_ADDRESS": "unix:path=/b"},
        ),
        ({"WAYLAND_DISPLAY": "wayland-1"}, {}),
        (
            {"XDG_RUNTIME_DIR": "/run/user/7", "DISPLAY": ":0"},
            {
                "DISPLAY": ":0",
                "WAYLAND_DISPLAY": "/run/user/7/wayland-0",
                "DBUS_SESSION_BUS_ADDRESS": "unix:path=/run/user/7/bus",
            },
        ),
        ({"DISPLAY": ":0"}, {"DISPLAY": ":0"}),
    ],
)
def test_runtime_dir_is_private_and_session_sockets_stay_reachable(
    parent: dict[str, str], expected: dict[str, str]
) -> None:
    env = dict(parent)
    isolated_runtime_env(env, "/tmp/agt-x/run", parent)
    assert env == {"XDG_RUNTIME_DIR": "/tmp/agt-x/run", **expected}


def test_runtime_dir_is_refused_outside_the_root_only_where_needed(tmp_path: Path) -> None:
    root = tmp_path / "root"
    check_launch_env(env_for(root, XDG_RUNTIME_DIR="/run/user/1000"), root, needs_app_id=True)
    with pytest.raises(LaunchRefused, match="XDG_RUNTIME_DIR must be set"):
        check_launch_env(env_for(root), root, needs_app_id=True, needs_runtime_dir=True)
    with pytest.raises(LaunchRefused, match="XDG_RUNTIME_DIR"):
        check_launch_env(
            env_for(root, XDG_RUNTIME_DIR="/run/user/1000"), root, needs_app_id=True, needs_runtime_dir=True
        )
    check_launch_env(env_for(root, XDG_RUNTIME_DIR=str(root / "run")), root, needs_app_id=True, needs_runtime_dir=True)


def test_linux_adapters_get_a_private_runtime_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("XDG_RUNTIME_DIR", "/run/user/7")
    monkeypatch.setenv("WAYLAND_DISPLAY", "wayland-1")
    linux = SwiftLinuxAdapter(tmp_path / "agterm-linux").build_env(tmp_path)
    assert linux["XDG_RUNTIME_DIR"] == str(tmp_path / "run")
    assert linux["WAYLAND_DISPLAY"] == "/run/user/7/wayland-1"
    check_launch_env(linux, tmp_path, needs_app_id=True, needs_runtime_dir=True)
    assert SwiftMacosAdapter(tmp_path / "agterm.app").build_env(tmp_path)["XDG_RUNTIME_DIR"] == "/run/user/7"
    assert RustAdapter.needs_runtime_dir is RustAdapter.needs_app_id


def test_each_build_env_gets_a_fresh_app_id(tmp_path: Path) -> None:
    adapter = SwiftLinuxAdapter(tmp_path / "agterm-linux")
    assert adapter.build_env(tmp_path)["AGTERM_APP_ID"] != adapter.build_env(tmp_path)["AGTERM_APP_ID"]


def test_refused_launch_spawns_nothing_and_removes_its_root(roots: list[Path], tmp_path: Path) -> None:
    adapter = SwiftLinuxAdapter(tmp_path / "never-run")
    with pytest.raises(LaunchRefused, match="installed app"):
        adapter.launch(app_id=DEFAULT_APP_ID)
    assert len(roots) == 1 and not roots[0].exists()


# -- launch and stop against the fake app


def test_launch_isolates_and_stop_cleans_up(roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AGTERM_SOCKET", "/live/agterm.sock")
    monkeypatch.setenv("ZMX_DIR", "/tmp/live-zmx")
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    try:
        assert instance.version == {"app": {"version": "0.0.1"}}
        assert instance.request("version").ok
        seen = json.loads((instance.state_dir / "fake-env.json").read_text())
        assert "AGTERM_SOCKET" not in seen and "ZMX_DIR" not in seen
        assert seen["HOME"] == str(instance.home)
        assert seen["AGTERM_CONTROL_SOCKET"] == str(instance.socket)
        assert seen["AGTERM_APP_ID"] == instance.app_id
        assert (instance.home / ".claude").is_dir() and (instance.home / ".codex").is_dir()
        assert (instance.home / ".profile").read_text() == "PS1='$ '\n"
        assert (instance.state_dir / "windows").is_dir()
        assert not (instance.state_dir / "settings.json").exists()
        ghostty = (instance.state_dir / "config" / "ghostty.conf").read_text().splitlines()
        assert ghostty == ["command = /bin/sh", "copy-on-select = false", "clipboard-write = deny"]
        assert seen["XDG_RUNTIME_DIR"] == str(instance.root / "run")
        assert (instance.root / "run").stat().st_mode & 0o777 == 0o700
        assert instance.root.parent == Path(base.TEMP_PARENT)
    finally:
        adapter.stop(instance)
    assert instance.process.returncode is not None
    assert not instance.root.exists()


@pytest.mark.parametrize("adapter_class", [SwiftLinuxAdapter, RustAdapter])
def test_launch_seeds_settings(roots: list[Path], fake_bin: Path, adapter_class: type[Adapter]) -> None:
    adapter = adapter_class(fake_bin)
    instance = adapter.launch(settings={"restoreMode": "live", "autoFollowAttention": "s30"})
    try:
        seeded = json.loads((instance.state_dir / "settings.json").read_text())
        assert seeded == {"restoreMode": "live", "autoFollowAttention": "s30"}
    finally:
        adapter.stop(instance)


@pytest.mark.parametrize("adapter_class", [SwiftLinuxAdapter, RustAdapter])
def test_linux_launch_sends_every_uri_to_a_noop(
    roots: list[Path], fake_bin: Path, adapter_class: type[Adapter], monkeypatch: pytest.MonkeyPatch
) -> None:
    if not adapter_class.needs_runtime_dir:
        pytest.skip("a Linux desktop launch only")
    monkeypatch.setenv("GTK_USE_PORTAL", "1")
    monkeypatch.setenv("GIO_USE_PORTALS", "1")
    adapter = adapter_class(fake_bin)
    instance = adapter.launch()
    try:
        seen = json.loads((instance.state_dir / "fake-env.json").read_text())
        assert seen["BROWSER"] == "true" and seen["GIO_USE_PORTALS"] == "0" and "GTK_USE_PORTAL" not in seen
        desktop = (instance.home / ".local/share/applications" / NOOP_DESKTOP_ID).read_text().splitlines()
        noop = shutil.which("true", path="/usr/bin:/bin")
        assert desktop == [
            "[Desktop Entry]",
            "Type=Application",
            "Name=agterm conformance no-op",
            f"Exec={noop} %u",
            "NoDisplay=true",
            f"MimeType={';'.join(NOOP_MIME_TYPES)};",
        ]
        mimeapps = (instance.home / ".config/mimeapps.list").read_text()
        entries = [f"{mime}={NOOP_DESKTOP_ID};" for mime in NOOP_MIME_TYPES]
        assert mimeapps.splitlines() == ["[Default Applications]", *entries, "", "[Added Associations]", *entries]
        assert {"inode/directory", "x-scheme-handler/https", "x-scheme-handler/file"} <= set(NOOP_MIME_TYPES)
        if gio := shutil.which("gio"):
            env = {name: seen[name] for name in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "PATH")}
            for mime in ("inode/directory", "x-scheme-handler/https"):
                answer = subprocess.run([gio, "mime", mime], env=env, capture_output=True, text=True, check=False)
                # A C locale prints the curly quotes around the type as `?`.
                expected = rf"Default application for .{re.escape(mime)}.: {re.escape(NOOP_DESKTOP_ID)}"
                assert re.fullmatch(expected, answer.stdout.splitlines()[0]), answer
    finally:
        adapter.stop(instance)


@pytest.mark.parametrize("adapter_class", [SwiftLinuxAdapter, RustAdapter])
def test_relaunch_starts_the_binary_again_on_the_same_root(
    roots: list[Path], fake_bin: Path, adapter_class: type[Adapter]
) -> None:
    adapter = adapter_class(fake_bin)
    first = adapter.launch(settings={"restoreMode": "rerun"})
    (first.state_dir / "kept").write_text("state")
    second = adapter.relaunch(first)
    try:
        assert first.process.returncode is not None and second.pid != first.pid
        same = ("root", "state_dir", "home", "socket", "env", "log", "app_id", "zmx_dir")
        assert all(getattr(second, name) == getattr(first, name) for name in same)
        assert (second.state_dir / "kept").read_text() == "state"
        assert second.version == {"app": {"version": "0.0.1"}} and second.request("version").ok
        assert json.loads((second.state_dir / "settings.json").read_text()) == {"restoreMode": "rerun"}
    finally:
        adapter.stop(second)
    assert len(roots) == 1 and not roots[0].exists()


def test_relaunch_of_an_app_ignoring_sigterm_fails_and_frees_its_app_id(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "ignore-term")
    monkeypatch.setattr(base, "STOP_TIMEOUT", 0.5)
    adapter = SwiftLinuxAdapter(fake_bin)
    app_id = f"{DEFAULT_APP_ID}.conformance.relaunch"
    instance = adapter.launch(app_id=app_id)
    with pytest.raises(StopError, match="ignored SIGTERM"):
        adapter.relaunch(instance)
    assert instance.process.returncode == -9
    monkeypatch.delenv("FAKE_APP_MODE")
    adapter.stop(adapter.launch(app_id=app_id))


def test_relaunch_killing_the_app_reports_the_daemons_it_left(
    roots: list[Path],
    fake_bin: Path,
    tmp_path: Path,
    daemons: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "ignore-term")
    monkeypatch.setattr(base, "STOP_TIMEOUT", 0.5)
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    pid = spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)
    with pytest.raises(LeftoverProcesses, match=rf"{pid} ") as raised:
        adapter.relaunch(instance)
    assert isinstance(raised.value.__cause__, StopError)
    assert instance.process.returncode == -9 and instance.root.exists()


def test_relaunch_failing_to_spawn_reports_the_daemons_left_and_frees_its_app_id(
    roots: list[Path],
    fake_bin: Path,
    tmp_path: Path,
    daemons: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    app_id = f"{DEFAULT_APP_ID}.conformance.respawn"
    instance = adapter.launch(app_id=app_id)
    pid = spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)

    def unspawnable(*args: object) -> subprocess.Popen[bytes]:
        raise OSError("no binary")

    with monkeypatch.context() as patched:
        patched.setattr(base, "_spawn", unspawnable)
        with pytest.raises(LeftoverProcesses, match=rf"{pid} ") as raised:
            adapter.relaunch(instance)
    assert isinstance(raised.value.__cause__, OSError)
    adapter.stop(adapter.launch(app_id=app_id))


def test_duplicate_app_id_is_refused_until_the_owner_stops(roots: list[Path], fake_bin: Path) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    app_id = f"{DEFAULT_APP_ID}.conformance.dup"
    first = adapter.launch(app_id=app_id)
    try:
        with pytest.raises(LaunchRefused, match="already used"):
            adapter.launch(app_id=app_id)
    finally:
        adapter.stop(first)
    adapter.stop(adapter.launch(app_id=app_id))


def test_relative_bin_is_resolved_before_the_child_moves_to_its_home(
    roots: list[Path], fake_bin: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    adapter = SwiftLinuxAdapter(fake_bin.relative_to(tmp_path), ctl=Path("install/bin/agtermctl"))
    assert adapter.bin == fake_bin and adapter.ctl_path == tmp_path / "install" / "bin" / "agtermctl"
    adapter.stop(adapter.launch())


def test_unexecutable_bin_fails_the_launch_and_removes_its_root(roots: list[Path], tmp_path: Path) -> None:
    program = tmp_path / "agterm-linux"
    program.write_text("not a program\n")
    adapter = SwiftLinuxAdapter(program)
    with pytest.raises(PermissionError):
        adapter.launch()
    assert len(roots) == 1 and not roots[0].exists()


def test_app_exiting_before_serving_fails_the_launch(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "exit")
    with pytest.raises(LaunchError, match=r"exited with 3(.|\n)*refusing to start"):
        SwiftLinuxAdapter(fake_bin).launch()


def test_app_never_serving_times_out_and_is_stopped(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "silent")
    spawned: list[subprocess.Popen[bytes]] = []
    popen = subprocess.Popen

    def recording(*args, **kwargs):  # type: ignore[no-untyped-def]
        spawned.append(popen(*args, **kwargs))
        return spawned[-1]

    monkeypatch.setattr(base.subprocess, "Popen", recording)
    with pytest.raises(LaunchError, match="not serving after"):
        SwiftLinuxAdapter(fake_bin).launch(timeout=1.0)
    assert spawned[0].returncode is not None


def test_version_refused_during_launch_fails_the_launch(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "refuse")
    with pytest.raises(LaunchError, match="not ready"):
        SwiftLinuxAdapter(fake_bin).launch()


def test_app_ignoring_sigterm_is_killed_and_reported(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "ignore-term")
    monkeypatch.setattr(base, "STOP_TIMEOUT", 0.5)
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    with pytest.raises(StopError, match="ignored SIGTERM"):
        adapter.stop(instance)
    assert instance.process.returncode == -9


def test_stop_reaps_claimed_daemons_over_the_protocol(
    roots: list[Path], fake_bin: Path, tmp_path: Path, daemons: list[subprocess.Popen[bytes]]
) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    pid = spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)
    assert instance.request("fake.adopt", pid=pid).ok
    assert find_leftovers(instance.zmx_dir) != []
    adapter.stop(instance, cleanup=False)
    requests = [json.loads(line) for line in (instance.state_dir / "fake-requests.jsonl").read_text().splitlines()]
    assert {"cmd": "zmx.kill", "target": f"s{pid}", "args": {"pane": "left", "force": True, "window": "w1"}} in requests


def test_stop_reaps_a_split_whose_survivor_is_promoted_by_the_first_kill(
    roots: list[Path], fake_bin: Path, tmp_path: Path, daemons: list[subprocess.Popen[bytes]]
) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    for name, pane in (("a-left", "left"), ("b-right", "right")):
        pid = spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)
        assert instance.request("fake.adopt", pid=pid, session="s1", pane=pane, daemon=name).ok
    adapter.stop(instance)
    assert find_leftovers(instance.zmx_dir) == []


def test_a_refused_kill_is_not_retried(roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    row = {"daemon": "d1", "state": "claimed", "sessionID": "s1", "pane": "left"}
    monkeypatch.setenv("FAKE_ZMX_LIST", json.dumps({"zmx": {"entries": [row]}}))
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    base.reap_live_sessions(instance)
    requests = [json.loads(line) for line in (instance.state_dir / "fake-requests.jsonl").read_text().splitlines()]
    assert [request["cmd"] for request in requests].count("zmx.kill") == 1
    adapter.stop(instance)


@pytest.mark.parametrize("listing", ['{"zmx": {"entries": null}}', '{"zmx": {"entries": [null, 1]}}', '{"zmx": 1}'])
def test_malformed_zmx_list_still_terminates_the_app(
    roots: list[Path],
    fake_bin: Path,
    tmp_path: Path,
    daemons: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
    listing: str,
) -> None:
    monkeypatch.setenv("FAKE_ZMX_LIST", listing)
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)
    with pytest.raises(LeftoverProcesses):
        adapter.stop(instance)
    assert instance.process.returncode is not None


def test_stop_reports_daemons_left_under_the_instance_zmx_dir(
    roots: list[Path], fake_bin: Path, tmp_path: Path, daemons: list[subprocess.Popen[bytes]]
) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    pid = spawn_daemon(tmp_path, "agterm-session-host", instance.zmx_dir, daemons)
    with pytest.raises(LeftoverProcesses, match=rf"{pid} .*agterm-session-host"):
        adapter.stop(instance)
    assert instance.process.returncode is not None
    assert instance.root.exists()


def test_leftover_check_matches_name_and_zmx_dir(tmp_path: Path, daemons: list[subprocess.Popen[bytes]]) -> None:
    mine, other = "/tmp/agterm-zmx-mine", "/tmp/agterm-zmx-other"
    zmx = spawn_daemon(tmp_path, "zmx", mine, daemons)
    spawn_daemon(tmp_path, "zmx", other, daemons)
    spawn_daemon(tmp_path, "unrelated", mine, daemons)
    assert [pid for pid, *_ in find_leftovers(mine)] == [zmx]


def test_leftover_check_names_an_executable_whose_path_has_a_space(
    tmp_path: Path, daemons: list[subprocess.Popen[bytes]]
) -> None:
    zmx_dir = "/tmp/agterm-zmx-spaced"
    pid = spawn_daemon(tmp_path, "zmx", zmx_dir, daemons, directory="Test Install")
    assert [found for found, *_ in find_leftovers(zmx_dir)] == [pid]


def test_leftover_check_finds_a_session_host_by_its_host_argv(monkeypatch: pytest.MonkeyPatch) -> None:
    mine, other = "/tmp/agterm-zmx-mine", "/tmp/agterm-zmx-other"
    rows = [
        (1, "agterm-session-host", "a", ["/x/agterm-session-host", "host", mine, "HOME=/h"]),
        (2, "agterm-session-host", "b", ["/x/agterm-session-host", "host", other]),
        (3, "agterm-session-host", "c", ["/x/agterm-session-host", "client", mine]),
    ]
    monkeypatch.setattr(base, "_processes", lambda: iter(rows))
    assert [pid for pid, *_ in find_leftovers(mine)] == [1]
    assert hosts_zmx_dir(["/a b/agterm-session-host", "host", mine], mine)
    assert not hosts_zmx_dir(["/x/agterm-session-host", "host"], mine)


def test_process_identity_reads_the_image_argv_and_environment(
    tmp_path: Path, daemons: list[subprocess.Popen[bytes]]
) -> None:
    pid = spawn_daemon(tmp_path, "agterm-session-host", "/tmp/agterm-zmx-ident", daemons)
    identity = process_identity(pid)
    assert identity is not None
    assert identity.executable == os.path.realpath(tmp_path / "daemons" / "agterm-session-host")
    assert "ZMX_DIR=/tmp/agterm-zmx-ident" in identity.words
    assert identity.words[0].endswith("agterm-session-host")


# -- the macOS session host the harness may end (docs: Adapter.stop_helpers)


class FakeHosts:
    """`process_identity` answers per pid from a queue (the last answer repeats); `signals` records what was sent."""

    def __init__(self, monkeypatch: pytest.MonkeyPatch, instance: Instance) -> None:
        self.answers: dict[int, list[ProcessIdentity | None]] = {}
        self.signals: list[tuple[int, int]] = []
        self.ignores_sigterm = False
        monkeypatch.setattr(swift_macos, "process_identity", self.identity)
        monkeypatch.setattr(swift_macos, "_send_signal", self.send)
        monkeypatch.setattr(
            swift_macos,
            "find_leftovers",
            lambda zmx_dir: [(pid, "agterm-session-host", "") for pid in self.answers] if zmx_dir else [],
        )
        self.instance = instance

    def identity(self, pid: int) -> ProcessIdentity | None:
        queue = self.answers.get(pid, [None])
        return queue.pop(0) if len(queue) > 1 else queue[0]

    def send(self, pid: int, signum: int) -> None:
        self.signals.append((pid, signum))
        if signum == signal.SIGKILL or not self.ignores_sigterm:
            self.answers[pid] = [None]


def host_identity(bundle: Path, zmx_dir: str) -> ProcessIdentity:
    executable = bundle / "Contents" / "MacOS" / "agterm-session-host"
    return ProcessIdentity(os.path.realpath(executable), (str(executable), "host", zmx_dir, f"ZMX_DIR={zmx_dir}"))


@pytest.fixture
def mac_host(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[SwiftMacosAdapter, Instance, FakeHosts]:
    bundle = tmp_path / "ref" / "agterm.app"
    (bundle / "Contents" / "MacOS").mkdir(parents=True)
    root = tmp_path / "root"
    state = root / "state"
    state.mkdir(parents=True)
    instance = Instance(
        process=None,  # type: ignore[arg-type]
        root=root,
        state_dir=state,
        home=root / "home",
        socket=state / "agterm.sock",
        env={},
        log=root / "app.log",
        app_id=None,
        zmx_dir=zmx_socket_directory(state),
    )
    adapter = SwiftMacosAdapter(bundle)
    adapter.session_host_timeout = 0.2
    return adapter, instance, FakeHosts(monkeypatch, instance)


def test_owned_session_host_gets_sigterm(mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts]) -> None:
    adapter, instance, hosts = mac_host
    hosts.answers[41] = [host_identity(adapter.instance_bundle(instance.root), instance.zmx_dir)]
    assert adapter.stop_helpers(instance) is None
    assert hosts.signals == [(41, signal.SIGTERM)]


@pytest.mark.parametrize("which", ["shared", "other"])
def test_session_host_from_another_bundle_is_not_signalled(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts], tmp_path: Path, which: str
) -> None:
    adapter, instance, hosts = mac_host
    bundle = adapter.bundle() if which == "shared" else tmp_path / "other" / "agterm.app"
    hosts.answers[41] = [host_identity(bundle, instance.zmx_dir)]
    assert adapter.stop_helpers(instance) is None
    assert hosts.signals == []


def test_session_host_for_another_zmx_dir_is_not_signalled(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts],
) -> None:
    adapter, instance, hosts = mac_host
    hosts.answers[41] = [host_identity(adapter.instance_bundle(instance.root), "/tmp/agterm-zmx-0000000000000000")]
    assert adapter.stop_helpers(instance) is None
    assert hosts.signals == []


def test_zmx_dir_not_derived_from_the_instance_state_dir_signals_nothing(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts], tmp_path: Path
) -> None:
    adapter, instance, hosts = mac_host
    outside = tmp_path / "elsewhere"
    outside.mkdir()
    instance.state_dir = outside
    instance.zmx_dir = zmx_socket_directory(outside)
    hosts.answers[41] = [host_identity(adapter.instance_bundle(instance.root), instance.zmx_dir)]
    assert adapter.stop_helpers(instance) is None
    assert hosts.signals == []


def test_pid_reused_before_the_signal_is_not_signalled(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts], tmp_path: Path
) -> None:
    adapter, instance, hosts = mac_host
    stranger = ProcessIdentity("/usr/bin/vim", ("vim",))
    hosts.answers[41] = [host_identity(adapter.instance_bundle(instance.root), instance.zmx_dir), stranger]
    assert adapter.stop_helpers(instance) is None
    assert hosts.signals == []


def test_session_host_ignoring_sigterm_is_killed_after_reverifying(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts],
) -> None:
    adapter, instance, hosts = mac_host
    hosts.ignores_sigterm = True
    hosts.answers[41] = [host_identity(adapter.instance_bundle(instance.root), instance.zmx_dir)]
    assert "41 ignored SIGTERM" in (adapter.stop_helpers(instance) or "")
    assert hosts.signals == [(41, signal.SIGTERM), (41, signal.SIGKILL)]


def test_pid_reused_while_waiting_for_sigterm_gets_no_sigkill(
    mac_host: tuple[SwiftMacosAdapter, Instance, FakeHosts], monkeypatch: pytest.MonkeyPatch
) -> None:
    adapter, instance, hosts = mac_host
    hosts.ignores_sigterm = True

    def timed_out(check: object, timeout: float) -> None:
        raise swift_macos.WaitTimeout("still running")

    monkeypatch.setattr(swift_macos, "wait_until", timed_out)
    owned = host_identity(adapter.instance_bundle(instance.root), instance.zmx_dir)
    hosts.answers[41] = [owned, owned, ProcessIdentity("/usr/bin/vim", ("vim",))]
    adapter.stop_helpers(instance)
    assert hosts.signals == [(41, signal.SIGTERM)]


def test_zmx_dir_is_the_fnv1a_hash_of_the_canonical_state_dir(tmp_path: Path) -> None:
    assert fnv1a64("") == "cbf29ce484222325"
    assert fnv1a64("a") == "af63dc4c8601ec8c"
    assert fnv1a64("foobar") == "85944171f73967e8"
    real = tmp_path / "state"
    real.mkdir()
    (tmp_path / "link").symlink_to(real)
    expected = f"/tmp/agterm-zmx-{fnv1a64(os.path.realpath(real))}"
    assert zmx_socket_directory(real) == zmx_socket_directory(tmp_path / "link") == expected


# -- capabilities and parity level


@pytest.mark.parametrize(
    ("since", "level", "at"),
    [
        ("0.36.0", "0.36.0", True),
        ("0.36", "0.36.0", True),
        ("v0.35.2", "0.36.0", True),
        ("0.36.1", "0.36.0", False),
        ("0.100.0", "0.36.0", False),
        ("0.1.0", None, False),
    ],
)
def test_profile_level_comparison(since: str, level: str | None, at: bool) -> None:
    assert Profile(level, frozenset()).at_level(since) is at


def test_version_tuple_rejects_garbage() -> None:
    with pytest.raises(ValueError):
        version_tuple("0.36.0-beta")


@pytest.fixture
def zsh_login(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Path:
    """An executable `zsh` as this account's password-database shell."""
    shell = tmp_path / "bin" / "zsh"
    shell.parent.mkdir()
    shell.write_text("#!/bin/sh\n")
    shell.chmod(0o755)
    _passwd_shell(monkeypatch, str(shell))
    return shell


def _passwd_shell(monkeypatch: pytest.MonkeyPatch, shell: str) -> None:
    entry = pwd.getpwuid(os.getuid())
    fake = pwd.struct_passwd((*entry[:6], shell))
    monkeypatch.setattr(base.pwd, "getpwuid", lambda uid: fake)


def _staged_zmx(launcher: Path) -> Path:
    zmx = launcher.parent.parent / "libexec" / "zmx"
    zmx.parent.mkdir()
    zmx.write_text("#!/bin/sh\n")
    zmx.chmod(0o755)
    return zmx


def test_swift_linux_profile(zsh_login: Path, fake_bin: Path) -> None:
    _staged_zmx(fake_bin)
    adapter = SwiftLinuxAdapter(fake_bin)
    assert adapter.parity_level == "0.36.0"
    assert adapter.capabilities == {
        "posix-shell",
        "capture-reads-command-panes",
        "hidden-split-holds-focus",
        "live-sessions",
    }
    assert "window-geometry" not in adapter.capabilities
    assert adapter.profile.absent == {}


@pytest.mark.parametrize(
    ("shell", "reason"),
    [
        ("/usr/bin/fish", "the password-database login shell is /usr/bin/fish, not zsh"),
        ("", "the password-database login shell is unset, not zsh"),
        ("/nonexistent/zsh", "the login shell /nonexistent/zsh is not executable"),
    ],
)
def test_live_sessions_need_an_executable_zsh_login_shell(
    monkeypatch: pytest.MonkeyPatch, fake_bin: Path, shell: str, reason: str
) -> None:
    _passwd_shell(monkeypatch, shell)
    _staged_zmx(fake_bin)
    for adapter in (SwiftLinuxAdapter(fake_bin), SwiftMacosAdapter(None, launch_mode="plain")):
        assert "live-sessions" not in adapter.capabilities
        assert adapter.profile.absent == {"live-sessions": reason}


def test_swift_linux_live_sessions_need_the_staged_zmx(zsh_login: Path, fake_bin: Path) -> None:
    adapter = SwiftLinuxAdapter(fake_bin)
    assert "live-sessions" not in adapter.capabilities
    assert adapter.profile.absent["live-sessions"].startswith("no executable zmx at ")


@pytest.mark.parametrize(
    ("mode", "zmx", "live"), [("plain", False, True), ("uitest", False, False), ("uitest", True, True)]
)
def test_swift_macos_profile_follows_launch_mode(zsh_login: Path, mode: str, zmx: bool, live: bool) -> None:
    adapter = SwiftMacosAdapter(None, launch_mode=mode, enable_zmx=zmx)
    assert adapter.parity_level == "0.36.0"
    assert {"posix-shell", "window-geometry"} <= adapter.capabilities
    assert ("live-sessions" in adapter.capabilities) is live


def test_static_capabilities_come_from_the_vocabulary() -> None:
    for adapter in (SwiftLinuxAdapter(None), SwiftMacosAdapter(None), SwiftMacosAdapter(None, launch_mode="uitest")):
        assert adapter.capabilities <= CAPABILITIES.keys()


def test_has_refuses_a_name_outside_the_vocabulary() -> None:
    adapter = SwiftLinuxAdapter(None)
    assert adapter.has("posix-shell") and not adapter.has("drop-placement")
    with pytest.raises(ValueError, match="unknown capability 'drop-placment'"):
        adapter.has("drop-placment")


def test_rust_without_a_binary_reports_nothing() -> None:
    adapter = RustAdapter(None)
    assert adapter.profile == NO_PROFILE
    assert adapter.parity_level is None and adapter.capabilities == frozenset()
    assert not adapter.available
    assert not adapter.profile.at_level("0.1.0")
    with pytest.raises(LaunchError, match="no binary"):
        adapter.launch()


@pytest.mark.parametrize(
    ("result", "profile"),
    [
        (None, NO_PROFILE),
        ({"app": {"version": "0.1.0"}}, NO_PROFILE),
        ({"conformance": {}}, NO_PROFILE),
        (
            {"conformance": {"parityLevel": "0.36.0", "capabilities": ["posix-shell"]}},
            Profile("0.36.0", frozenset({"posix-shell"})),
        ),
        ({"conformance": {"capabilities": ["live-sessions"]}}, Profile(None, frozenset({"live-sessions"}))),
    ],
)
def test_rust_profile_from_version(result: dict | None, profile: Profile) -> None:
    assert profile_from_version(result) == profile


@pytest.mark.parametrize(
    "block",
    [
        "0.36.0",
        {"parityLevel": 36},
        {"parityLevel": "latest"},
        {"capabilities": "posix-shell"},
        {"capabilities": [1]},
    ],
)
def test_rust_malformed_profile_is_an_error(block: object) -> None:
    with pytest.raises(ProfileError):
        profile_from_version({"conformance": block})


def test_rust_profile_is_probed_once_from_version(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    conformance = {"parityLevel": "0.12.0", "capabilities": ["posix-shell"]}
    monkeypatch.setenv("FAKE_VERSION", json.dumps({"app": {"version": "0.1.0"}, "conformance": conformance}))
    adapter = RustAdapter(fake_bin)
    assert adapter.profile == Profile("0.12.0", frozenset({"posix-shell"}))
    assert adapter.capabilities == {"posix-shell"}
    assert len(roots) == 1 and not roots[0].exists()


def test_rust_malformed_profile_fails_the_launch(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_VERSION", json.dumps({"conformance": {"parityLevel": 1}}))
    with pytest.raises(ProfileError):
        RustAdapter(fake_bin).launch()
    assert not roots[0].exists()


def test_rust_failed_probe_is_not_retried(roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "exit")
    adapter = RustAdapter(fake_bin)
    errors = []
    for _ in range(3):
        with pytest.raises(LaunchError, match="exited with 3") as caught:
            adapter.profile
        errors.append((caught.value, len(traceback.extract_tb(caught.value.__traceback__))))
    assert len(roots) == 1
    (first, first_frames), (second, second_frames), (third, third_frames) = errors
    assert second is not third and second.__cause__ is first and third.__cause__ is first
    assert len(traceback.extract_tb(first.__traceback__)) == first_frames
    assert second_frames == third_frames


# -- per-implementation wiring


def test_make_adapter_covers_every_impl(tmp_path: Path) -> None:
    for name, cls in ADAPTERS.items():
        adapter = make_adapter(ImplOptions(impl=name, bin=None, ctl=tmp_path / "ctl", coverage_gate=False))
        assert type(adapter) is cls and adapter.name == name
        assert adapter.ctl_path == tmp_path / "ctl"


def test_swift_linux_wires_the_install_layout(fake_bin: Path) -> None:
    install = fake_bin.parent.parent
    (install / "share" / "agterm").mkdir(parents=True)
    (fake_bin.parent / "agtermctl").touch()
    adapter = SwiftLinuxAdapter(fake_bin)
    assert adapter.ctl_path == fake_bin.parent / "agtermctl"
    env = adapter.build_env(Path("/tmp/agt-x"))
    assert env["AGTERM_RESOURCE_ROOT"] == str(install / "share" / "agterm")


def test_swift_linux_without_share_sets_no_resource_root(fake_bin: Path, tmp_path: Path) -> None:
    assert "AGTERM_RESOURCE_ROOT" not in SwiftLinuxAdapter(fake_bin).build_env(tmp_path)
    explicit = tmp_path / "resources"
    env = SwiftLinuxAdapter(fake_bin, resource_root=explicit).build_env(tmp_path)
    assert env["AGTERM_RESOURCE_ROOT"] == str(explicit)


def test_swift_macos_instance_runs_its_own_copy_of_the_bundle(tmp_path: Path) -> None:
    bundle = tmp_path / "ref" / "agterm.app"
    (bundle / "Contents" / "MacOS").mkdir(parents=True)
    (bundle / "Contents" / "MacOS" / "agterm").write_text("binary")
    adapter = SwiftMacosAdapter(bundle / "Contents" / "MacOS" / "agterm")
    root = tmp_path / "root"
    command = adapter.instance_executable(root)
    copy = root / "app" / "agterm.app"
    assert command == [str(copy / "Contents" / "MacOS" / "agterm")]
    assert (copy / "Contents" / "MacOS" / "agterm").read_text() == "binary"
    assert adapter.instance_executable(root) == command


def test_swift_macos_execs_inside_the_bundle(tmp_path: Path) -> None:
    bundle = tmp_path / "agterm.app"
    adapter = SwiftMacosAdapter(bundle)
    assert adapter.executable() == [str(bundle / "Contents" / "MacOS" / "agterm")]
    assert adapter.ctl_path == bundle / "Contents" / "MacOS" / "agtermctl"
    direct = SwiftMacosAdapter(bundle / "Contents" / "MacOS" / "agterm")
    assert direct.executable() == [str(bundle / "Contents" / "MacOS" / "agterm")]
    assert direct.ctl_path == bundle / "Contents" / "MacOS" / "agtermctl"


@pytest.mark.parametrize(("mode", "zmx"), [("plain", False), ("uitest", False), ("uitest", True)])
def test_swift_macos_launch_env(tmp_path: Path, mode: str, zmx: bool) -> None:
    env = SwiftMacosAdapter(tmp_path / "agterm.app", launch_mode=mode, enable_zmx=zmx).build_env(tmp_path)
    assert env["CFFIXED_USER_HOME"] == env["HOME"]
    assert "AGTERM_APP_ID" not in env
    assert ("AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE" in env) is (mode == "uitest")
    assert ("AGTERM_UITEST_ENABLE_ZMX" in env) is zmx
    check_launch_env(env, tmp_path, needs_app_id=False)


def test_swift_macos_defaults_to_the_sentinel_with_zmx(zsh_login: Path, tmp_path: Path) -> None:
    adapter = SwiftMacosAdapter(tmp_path / "agterm.app")
    env = adapter.build_env(tmp_path)
    assert env["AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"] == "1" and env["AGTERM_UITEST_ENABLE_ZMX"] == "1"
    assert "live-sessions" in adapter.capabilities


@pytest.mark.parametrize("mode", ["uitest", "plain"])
def test_make_adapter_passes_the_macos_launch_mode(mode: str) -> None:
    options = ImplOptions(impl="swift-macos", bin=None, ctl=None, coverage_gate=False, macos_launch_mode=mode)
    adapter = make_adapter(options)
    assert isinstance(adapter, SwiftMacosAdapter) and adapter.launch_mode == mode and adapter.enable_zmx


def test_swift_macos_rejects_an_unknown_launch_mode() -> None:
    with pytest.raises(ValueError, match="launch_mode"):
        SwiftMacosAdapter(None, launch_mode="open")


def test_reaping_waits_for_a_daemon_zmx_does_not_list_yet_and_reports_refusals(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    row = {"daemon": "d1", "state": "claimed", "sessionID": "s1", "pane": "left", "observation": "absent"}
    monkeypatch.setenv("FAKE_ZMX_LIST", json.dumps({"zmx": {"entries": [row]}}))
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    try:
        assert base.reap_live_sessions(instance, settle=0.2) == [
            "listed: d1 claimed/absent",
            "zmx.kill d1: no left pane daemon for session s1",
            "zmx.prune: unknown command: zmx.prune",
            "left listed: d1 claimed/absent",
        ]
        requests = [json.loads(line) for line in (instance.state_dir / "fake-requests.jsonl").read_text().splitlines()]
        commands = [request["cmd"] for request in requests]
        assert commands.count("zmx.list") > 2 and commands.count("zmx.kill") == 1
    finally:
        adapter.stop(instance)


def test_reaping_waits_out_a_closed_session_daemon(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    row = {"daemon": "d1", "state": "pendingClose", "sessionID": "s1", "pane": "left", "observation": "running"}
    monkeypatch.setenv("FAKE_ZMX_LIST", json.dumps({"zmx": {"entries": [row]}}))
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    try:
        assert base.reap_live_sessions(instance, settle=0.2) == [
            "listed: d1 pendingClose/running",
            "zmx.prune: unknown command: zmx.prune",
            "left listed: d1 pendingClose/running",
        ]
        requests = [json.loads(line) for line in (instance.state_dir / "fake-requests.jsonl").read_text().splitlines()]
        commands = [request["cmd"] for request in requests]
        assert commands.count("zmx.list") > 2 and "zmx.kill" not in commands
    finally:
        adapter.stop(instance)


def test_stop_reaps_a_listed_daemon_that_has_no_process_yet(
    roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    row = {"daemon": "d1", "state": "claimed", "sessionID": "s1", "pane": "left", "observation": "running"}
    monkeypatch.setenv("FAKE_ZMX_LIST", json.dumps({"zmx": {"entries": [row]}}))
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    requests_file = instance.state_dir / "fake-requests.jsonl"
    adapter.stop(instance, cleanup=False)
    commands = [json.loads(line)["cmd"] for line in requests_file.read_text().splitlines()]
    assert "zmx.kill" in commands
    shutil.rmtree(instance.root)


def test_a_daemon_exiting_within_the_grace_is_not_a_leftover(
    roots: list[Path],
    fake_bin: Path,
    tmp_path: Path,
    daemons: list[subprocess.Popen[bytes]],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(base, "LEFTOVER_GRACE", 5.0)
    adapter = SwiftLinuxAdapter(fake_bin)
    instance = adapter.launch()
    spawn_daemon(tmp_path, "zmx", instance.zmx_dir, daemons)
    exiting = daemons[-1]
    timer = threading.Timer(0.3, exiting.stdin.close)
    timer.start()
    adapter.stop(instance)
    timer.join()
    assert exiting.wait(timeout=5) == 0
