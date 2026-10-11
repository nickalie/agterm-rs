"""Implementation adapter contract: isolated launch, socket wait, own-pid stop, leftover-process check.

Safety is an allowlist: every path the instance is told about lives under a fresh `mkdtemp` root, and the
guards refuse a launch env that points anywhere else before anything is spawned.
"""

from __future__ import annotations

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar

from agterm_conformance.client import (
    Client,
    ClientError,
    ClientTimeout,
    ConnectFailed,
    NoResponse,
    Response,
    check_under_root,
)
from agterm_conformance.plugin import SCRUB_PREFIXES

# Both references refuse to bind at `path.utf8.count >= 104` (Darwin's sun_path, mirrored on Linux).
SOCKET_PATH_CAP = 104
# `/tmp`, not `$TMPDIR`: a macOS runner's `/var/folders/...` temp dir eats most of the socket cap.
# `CONFORMANCE_TMP` (not `AGTERM_`-prefixed, which the plugin scrubs) gives run-reference-linux.sh its own parent.
TEMP_PARENT = os.environ.get("CONFORMANCE_TMP") or "/tmp"
MINIMAL_PATH = "/usr/bin:/bin"
DEFAULT_APP_ID = "io.github.melonamin.agterm"
SESSION_HOST = "agterm-session-host"
LEFTOVER_NAMES = frozenset({"zmx", SESSION_HOST})
# The macOS host outlives the app by design, takes no stop a harness can send, and is not ours to signal.
SESSION_HOST_NOTE = (
    f"; {SESSION_HOST} is kept alive by the reference itself and the harness may not signal it,"
    " see docs/backlog/harness-stop-for-macos-session-host.md"
)
# With SHELL=/bin/sh this is belt and braces: either alone gives a reference-linux pane /bin/sh
# (conformance/docs/spike-macos.md).
GHOSTTY_SHELL_CONFIG = "command = /bin/sh\n"
LAUNCH_TIMEOUT = 30.0
STOP_TIMEOUT = 10.0
POLL_INTERVAL = 0.05

# The capability vocabulary `requires(...)` may name. A reason for each absence lives on the test marker.
CAPABILITIES: Mapping[str, str] = {
    "posix-shell": "panes run /bin/sh, so `session type` + `session.text` round-trips are deterministic",
    "live-sessions": "restoreMode live keeps panes in zmx daemons that outlive the app",
    "window-geometry": "window x/y position is settable and restored",
    "status-shape": "`session.status` validates `shape` and reads it back as `statusShape`",
    "type-rejects-nul": "`session.type` refuses text carrying a NUL rather than typing the run before it",
    "resize-echoes-ratio": "`session.resize` answers with the applied ratio in `result.ratio`",
    "drop-placement": "`session.move --after/--before` places like a sidebar drop, also above its own anchor",
}


class LaunchError(Exception):
    pass


class LaunchRefused(LaunchError):
    """A guard rejected the launch env before anything was spawned."""


class StopError(Exception):
    pass


class LeftoverProcesses(StopError):
    pass


@dataclass(frozen=True)
class Profile:
    """What an implementation claims: `parity_level` None means it is below every `since` version."""

    parity_level: str | None
    capabilities: frozenset[str]

    def at_level(self, since: str) -> bool:
        return self.parity_level is not None and version_tuple(since) <= version_tuple(self.parity_level)


NO_PROFILE = Profile(parity_level=None, capabilities=frozenset())


def version_tuple(version: str) -> tuple[int, ...]:
    match = re.fullmatch(r"v?(\d+(?:\.\d+)*)", version.strip())
    if not match:
        raise ValueError(f"not a dotted version: {version!r}")
    parts = [int(part) for part in match.group(1).split(".")]
    while len(parts) > 1 and parts[-1] == 0:
        parts.pop()
    return tuple(parts)


def zmx_socket_directory(state_dir: str | os.PathLike[str]) -> str:
    """`ZmxSupport.socketDirectory`: FNV-1a 64 of the canonical state dir, under `/tmp`."""
    return f"/tmp/agterm-zmx-{fnv1a64(_foundation_resolved(os.path.realpath(state_dir)))}"


def fnv1a64(text: str) -> str:
    value = 14_695_981_039_346_656_037
    for byte in text.encode():
        value = ((value ^ byte) * 1_099_511_628_211) & 0xFFFF_FFFF_FFFF_FFFF
    return f"{value:016x}"


def _foundation_resolved(path: str) -> str:
    # Darwin Foundation's resolvingSymlinksInPath drops a leading /private when the shorter path exists.
    if sys.platform == "darwin" and path.startswith("/private/") and os.path.exists(path[len("/private") :]):
        return path[len("/private") :]
    return path


def scrubbed_environ(environ: Mapping[str, str] | None = None) -> dict[str, str]:
    source = os.environ if environ is None else environ
    return {name: value for name, value in source.items() if not name.startswith(SCRUB_PREFIXES)}


_APP_ID_ELEMENT = re.compile(r"[A-Za-z_-][A-Za-z0-9_-]*")
_app_ids_in_use: set[str] = set()
_app_ids_lock = threading.Lock()


def check_app_id(app_id: str | None) -> str:
    """Missing, default or malformed ids reach the user's running instance or abort GApplication."""
    if not app_id:
        raise LaunchRefused("AGTERM_APP_ID is required: without it the launch is forwarded to the running app")
    if app_id == DEFAULT_APP_ID:
        raise LaunchRefused(f"AGTERM_APP_ID {app_id} is the installed app's id")
    elements = app_id.split(".")
    if len(app_id) > 255 or len(elements) < 2 or not all(_APP_ID_ELEMENT.fullmatch(e) for e in elements):
        raise LaunchRefused(f"AGTERM_APP_ID {app_id!r} is not a valid GApplication id")
    return app_id


def unique_app_id() -> str:
    return f"{DEFAULT_APP_ID}.conformance.i{uuid.uuid4().hex[:12]}"


# Harness-owned variables; everything else under AGTERM_*/ZMX_* is inherited and refused.
OWN_VARIABLES = frozenset(
    {
        "AGTERM_STATE_DIR",
        "AGTERM_CONTROL_SOCKET",
        "AGTERM_APP_ID",
        "AGTERM_RESOURCE_ROOT",
        "AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE",
        "AGTERM_UITEST_ENABLE_ZMX",
    }
)


def check_launch_env(env: Mapping[str, str], root: str | os.PathLike[str], needs_app_id: bool) -> None:
    for name in sorted(env):
        if name.startswith(SCRUB_PREFIXES) and name not in OWN_VARIABLES:
            raise LaunchRefused(f"{name} would leak into the instance: only harness-set AGTERM_* are allowed")
    for name in ("AGTERM_STATE_DIR", "AGTERM_CONTROL_SOCKET", "HOME"):
        if not env.get(name):
            raise LaunchRefused(f"{name} must be set")
        try:
            check_under_root(env[name], root)
        except ClientError as error:
            raise LaunchRefused(f"{name}: {error}") from None
    socket_bytes = len(os.fsencode(env["AGTERM_CONTROL_SOCKET"]))
    if socket_bytes >= SOCKET_PATH_CAP:
        raise LaunchRefused(f"socket path is {socket_bytes} bytes, cap {SOCKET_PATH_CAP - 1}: the app would not bind")
    if needs_app_id:
        check_app_id(env.get("AGTERM_APP_ID"))


@dataclass
class Instance:
    process: subprocess.Popen[bytes]
    root: Path
    state_dir: Path
    home: Path
    socket: Path
    env: dict[str, str]
    log: Path
    app_id: str | None
    zmx_dir: str
    version: dict[str, Any] = field(default_factory=dict)

    @property
    def pid(self) -> int:
        return self.process.pid

    def client(self, timeout: float | None = None) -> Client:
        if timeout is None:
            return Client(self.socket, self.root)
        return Client(self.socket, self.root, timeout=timeout)

    def request(self, cmd: str, target: str | None = None, **args: Any) -> Response:
        return self.client().request(cmd, target, **args)

    def log_tail(self, lines: int = 20) -> str:
        try:
            return "\n".join(self.log.read_text(errors="replace").splitlines()[-lines:])
        except OSError:
            return ""


class Adapter:
    """One implementation under test. Subclasses supply the executable and platform env."""

    name: ClassVar[str] = ""
    needs_app_id: ClassVar[bool] = False

    def __init__(self, bin: Path | None, ctl: Path | None = None) -> None:
        # Absolute now: the child starts in the isolated HOME, where a relative path would not resolve.
        self.bin = _absolute(bin)
        self._ctl = _absolute(ctl)

    # -- what the implementation claims

    @property
    def profile(self) -> Profile:
        raise NotImplementedError

    @property
    def capabilities(self) -> frozenset[str]:
        return self.profile.capabilities

    @property
    def parity_level(self) -> str | None:
        return self.profile.parity_level

    @property
    def available(self) -> bool:
        return self.bin is not None

    @property
    def ctl_path(self) -> Path | None:
        return self._ctl if self._ctl is not None else self.default_ctl()

    def default_ctl(self) -> Path | None:
        return None

    def executable(self) -> list[str]:
        if self.bin is None:
            raise LaunchError(f"{self.name}: no binary to launch")
        return [os.fspath(self.bin)]

    def platform_env(self, env: dict[str, str]) -> None:
        """Hook for adapter-specific variables; `env` is already isolated."""

    def ghostty_config(self) -> str:
        """The agterm-scoped `<state>/config/ghostty.conf` the instance starts with."""
        return GHOSTTY_SHELL_CONFIG

    # -- launch

    def build_env(self, root: Path, app_id: str | None = None) -> dict[str, str]:
        home, state = root / "home", root / "state"
        env = scrubbed_environ()
        env.update(
            HOME=os.fspath(home),
            XDG_CONFIG_HOME=os.fspath(home / ".config"),
            XDG_DATA_HOME=os.fspath(home / ".local/share"),
            XDG_STATE_HOME=os.fspath(home / ".local/state"),
            XDG_CACHE_HOME=os.fspath(home / ".cache"),
            PATH=MINIMAL_PATH,
            SHELL="/bin/sh",
            AGTERM_STATE_DIR=os.fspath(state),
            AGTERM_CONTROL_SOCKET=os.fspath(state / "agterm.sock"),
        )
        if self.needs_app_id:
            env["AGTERM_APP_ID"] = app_id or unique_app_id()
        self.platform_env(env)
        return env

    def launch(self, timeout: float = LAUNCH_TIMEOUT, app_id: str | None = None) -> Instance:
        command = self.executable()
        root = make_root()
        try:
            env = self.build_env(root, app_id)
            check_launch_env(env, root, self.needs_app_id)
            app_id = env.get("AGTERM_APP_ID")
            _prepare_tree(root, Path(env["HOME"]), Path(env["AGTERM_STATE_DIR"]), self.ghostty_config())
            if app_id is not None:
                _claim_app_id(app_id)
        except BaseException:
            shutil.rmtree(root, ignore_errors=True)
            raise
        log = root / "app.log"
        try:
            with open(log, "wb") as sink:
                process = subprocess.Popen(
                    command,
                    env=env,
                    cwd=env["HOME"],
                    stdin=subprocess.DEVNULL,
                    stdout=sink,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
        except BaseException:
            _release_app_id(app_id)
            shutil.rmtree(root, ignore_errors=True)
            raise
        instance = Instance(
            process=process,
            root=root,
            state_dir=Path(env["AGTERM_STATE_DIR"]),
            home=Path(env["HOME"]),
            socket=Path(env["AGTERM_CONTROL_SOCKET"]),
            env=env,
            log=log,
            app_id=app_id,
            zmx_dir=zmx_socket_directory(env["AGTERM_STATE_DIR"]),
        )
        try:
            instance.version = wait_for_socket(instance, timeout)
        except BaseException:
            self.stop(instance, cleanup=False)
            raise
        return instance

    # -- stop

    def stop(self, instance: Instance, cleanup: bool = True) -> None:
        """Reap daemons, SIGTERM the launched pid only, then require no zmx/session-host left behind."""
        try:
            try:
                if instance.process.poll() is None and find_leftovers(instance.zmx_dir):
                    reap_live_sessions(instance)
            finally:
                killed = _terminate(instance.process)
            leftovers = find_leftovers(instance.zmx_dir)
        finally:
            _release_app_id(instance.app_id)
        if leftovers:
            described = ", ".join(f"{pid} {command}" for pid, _, command in leftovers)
            note = SESSION_HOST_NOTE if any(name == SESSION_HOST for _, name, _ in leftovers) else ""
            raise LeftoverProcesses(f"processes left under ZMX_DIR {instance.zmx_dir}: {described}{note}")
        if killed:
            raise StopError(f"pid {instance.pid} ignored SIGTERM for {STOP_TIMEOUT}s and was killed")
        if cleanup:
            shutil.rmtree(instance.root, ignore_errors=True)
            _remove_empty_dir(instance.zmx_dir)


def _absolute(path: Path | None) -> Path | None:
    return Path(os.path.abspath(path)) if path is not None else None


def make_root() -> Path:
    return Path(tempfile.mkdtemp(prefix="agt-", dir=TEMP_PARENT))


def _prepare_tree(root: Path, home: Path, state: Path, ghostty_config: str) -> None:
    for directory in (home / ".claude", home / ".codex", state / "config"):
        directory.mkdir(parents=True)
    # `FirstRunWelcome.hasPriorState` reads this directory: its presence skips the welcome dialog.
    (state / "windows").mkdir()
    (state / "config" / "ghostty.conf").write_text(ghostty_config)


def _claim_app_id(app_id: str) -> None:
    with _app_ids_lock:
        if app_id in _app_ids_in_use:
            raise LaunchRefused(f"AGTERM_APP_ID {app_id} is already used by a running instance")
        _app_ids_in_use.add(app_id)


def _release_app_id(app_id: str | None) -> None:
    if app_id is not None:
        with _app_ids_lock:
            _app_ids_in_use.discard(app_id)


def wait_for_socket(instance: Instance, timeout: float) -> dict[str, Any]:
    """Poll `version` until it answers; the process wait doubles as the poll interval and an exit check."""
    deadline = time.monotonic() + timeout
    last: Exception | None = None
    while True:
        try:
            per_request = max(POLL_INTERVAL, min(2.0, deadline - time.monotonic()))
            response = instance.client(timeout=per_request).request("version")
        except (ConnectFailed, NoResponse, ClientTimeout) as error:
            last = error
        else:
            if response.ok:
                return response.result or {}
            raise LaunchError(f"version refused during launch: {response.error!r}")
        if time.monotonic() >= deadline:
            raise LaunchError(
                f"socket {instance.socket} not serving after {timeout}s ({last}); app log:\n{instance.log_tail()}"
            )
        try:
            code = instance.process.wait(timeout=POLL_INTERVAL)
        except subprocess.TimeoutExpired:
            continue
        raise LaunchError(f"app exited with {code} before serving {instance.socket}; app log:\n{instance.log_tail()}")


def reap_live_sessions(instance: Instance) -> None:
    """Best effort over the protocol; whatever survives is reported by the leftover check.

    Re-lists after every kill: killing a left pane promotes its split survivor into the left slot, so a
    selector read before the kill no longer names that survivor.
    """
    try:
        tried: set[str] = set()
        while (entry := _next_claimed(instance, tried)) is not None:
            tried.add(_daemon_key(entry))
            args: dict[str, Any] = {"pane": entry["pane"], "force": True}
            if entry.get("windowID"):
                args["window"] = entry["windowID"]
            instance.request("zmx.kill", entry["sessionID"], **args)
        instance.request("zmx.prune")
    except ClientError:
        pass


def _next_claimed(instance: Instance, tried: set[str]) -> dict[str, Any] | None:
    listing = instance.request("zmx.list")
    inventory = (listing.result or {}).get("zmx") if listing.ok else None
    entries = inventory.get("entries") if isinstance(inventory, dict) else None
    for entry in entries if isinstance(entries, list) else []:
        if (
            isinstance(entry, dict)
            and entry.get("state") == "claimed"
            and entry.get("sessionID")
            and entry.get("pane")
            and _daemon_key(entry) not in tried
        ):
            return entry
    return None


def _daemon_key(entry: dict[str, Any]) -> str:
    return str(entry.get("daemon") or (entry["sessionID"], entry["pane"], entry.get("windowID")))


def _terminate(process: subprocess.Popen[bytes]) -> bool:
    if process.poll() is not None:
        return False
    process.send_signal(signal.SIGTERM)
    try:
        process.wait(timeout=STOP_TIMEOUT)
        return False
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=STOP_TIMEOUT)
        return True


def find_leftovers(zmx_dir: str) -> list[tuple[int, str, str]]:
    """(pid, name, command) of zmx / agterm-session-host processes carrying this instance's `ZMX_DIR`."""
    marker = f"ZMX_DIR={zmx_dir}"
    return [
        (pid, name, command)
        for pid, name, command, environ in _processes()
        if marker in environ and name in LEFTOVER_NAMES
    ]


def _processes() -> Iterable[tuple[int, str, str, list[str]]]:
    """(pid, program name, command, environ) for every readable process."""
    if os.path.isdir("/proc/self"):
        yield from _proc_processes()
    else:
        yield from _ps_processes()


def _proc_processes() -> Iterable[tuple[int, str, str, list[str]]]:
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            with open(f"/proc/{entry.name}/environ", "rb") as source:
                environ = [item.decode(errors="replace") for item in source.read().split(b"\0") if item]
            executable = os.readlink(f"/proc/{entry.name}/exe")
        except OSError:
            continue  # gone, or another user's process
        executable = executable.removesuffix(" (deleted)")
        yield int(entry.name), os.path.basename(executable), executable, environ


def _ps_processes() -> Iterable[tuple[int, str, str, list[str]]]:
    # [Unverified until Task 19] macOS `ps -E` appends the environment to the command for own processes.
    output = subprocess.run(
        ["ps", "-E", "-ww", "-o", "pid=,command=", "-u", str(os.getuid())],
        capture_output=True,
        text=True,
        check=False,
    ).stdout
    for line in output.splitlines():
        pid, _, command = line.strip().partition(" ")
        if pid.isdigit():
            # A command line cannot tell a space inside the executable path from an argument separator.
            yield int(pid), os.path.basename(command.split(" ", 1)[0]), command, command.split(" ")


def _remove_empty_dir(path: str) -> None:
    try:
        os.rmdir(path)
    except OSError:
        pass
