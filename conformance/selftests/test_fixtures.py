import json
import os
import sys
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace

import pytest
from agterm_conformance.client import Response, SocketOutsideRoot
from agterm_conformance.impls.base import Instance, Profile
from upstream_adapter import UpstreamVocabularyAdapter
from agterm_conformance.match import session_ids
from agterm_conformance.plugin import Cli, Ctl, skip_reason
from agterm_conformance.wait import (
    Cursor,
    WaitTimeout,
    wait_event,
    wait_frame,
    wait_response,
    wait_text,
    wait_until,
)

TWO_SESSIONS = {
    "workspaces": [
        {
            "id": "w1",
            "active": True,
            "sessions": [{"id": "s1", "name": "one", "active": True}, {"id": "s2", "name": "two", "active": False}],
        }
    ]
}


def run_suite(pytester: pytest.Pytester, *args: str) -> pytest.RunResult:
    return pytester.runpytest("-p", "no:cacheprovider", "-rs", *args)


def upstream(fake_bin: Path, *extra: str) -> tuple[str, ...]:
    return ("--impl", UpstreamVocabularyAdapter.name, "--bin", str(fake_bin), *extra)


@pytest.fixture(autouse=True)
def _upstream_impl(upstream_impl: str) -> str:
    return upstream_impl


@pytest.fixture
def ctl(roots: list[Path], fake_bin: Path) -> Iterator[Ctl]:
    adapter = UpstreamVocabularyAdapter(fake_bin)
    instance = adapter.launch()
    try:
        yield Ctl(instance)
    finally:
        adapter.stop(instance)


@pytest.fixture
def fake_ctl(tmp_path: Path) -> Path:
    script = tmp_path / "ctl" / "agtermctl"
    script.parent.mkdir()
    script.write_text(
        f"#!{sys.executable}\n"
        "import json, os, sys\n"
        "keys = ('HOME', 'AGTERM_STATE_DIR', 'PATH')\n"
        "print(json.dumps({'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': {k: os.environ.get(k) for k in keys},"
        " 'stdin': sys.stdin.read()}))\n"
    )
    script.chmod(0o755)
    return script


# -- skips and the summary


def test_skip_reasons_and_summary(pytester: pytest.Pytester, roots: list[Path], fake_bin: Path) -> None:
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.since("0.36.0")
        def test_at_level(ctl):
            assert ctl.request("version").ok

        @pytest.mark.since("0.37.0")
        def test_above_level(ctl):
            raise AssertionError("must not run")

        @pytest.mark.requires("window-geometry", reason="no window position here")
        def test_unsupported(ctl):
            raise AssertionError("must not run")

        @pytest.mark.requires("posix-shell")
        def test_supported(ctl):
            pass

        def test_fails(ctl):
            assert False

        @pytest.mark.skip(reason="plain")
        def test_plain_skip(ctl):
            pass

        def test_needs_no_instance():
            pass
        """
    )
    result = run_suite(pytester, *upstream(fake_bin))
    result.assert_outcomes(passed=3, failed=1, skipped=3)
    result.stdout.fnmatch_lines_random(
        [
            "SKIPPED [[]1[]] test_skip_reasons_and_summary.py:7: not-yet-at-level: since 0.37.0, upstream is at 0.36.0",
            "*unsupported: upstream lacks window-geometry (no window position here)",
            "upstream: 2 pass, 1 fail, 1 skip(unsupported), 1 skip(not-yet-at-level), 1 skip(other), 0 reruns, "
            "parity 50.0%",
        ]
    )
    assert len(roots) == 1 and not roots[0].exists()


def test_launch_fixture_starts_seeded_instances_and_stops_them(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(
        """
        import json

        def test_extra(launch):
            first, second = launch(settings={"restoreMode": "live"}), launch()
            assert first.request("version").ok and second.request("version").ok
            assert json.loads((first.instance.state_dir / "settings.json").read_text()) == {"restoreMode": "live"}
            assert not (second.instance.state_dir / "settings.json").exists()
        """
    )
    run_suite(pytester, *upstream(fake_bin)).assert_outcomes(passed=1)
    assert len(roots) == 2 and not any(root.exists() for root in roots)


def test_launch_fixture_relaunches_on_the_same_state_dir(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(
        """
        def test_relaunch(launch):
            first = launch(settings={"restoreMode": "rerun"})
            (first.instance.state_dir / "kept").write_text("state")
            second = launch.relaunch(first)
            assert second.instance.pid != first.instance.pid
            assert first.instance.process.returncode is not None
            assert second.instance.state_dir == first.instance.state_dir
            assert (second.instance.state_dir / "kept").read_text() == "state"
            assert second.request("version").ok
            assert launch.launched == [second.instance]
        """
    )
    run_suite(pytester, *upstream(fake_bin)).assert_outcomes(passed=1)
    assert len(roots) == 1 and not roots[0].exists()


def test_launch_fixture_waits_for_a_first_run_launch(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(
        """
        def test_first_run(launch):
            waiting = launch(first_run=True)
            ctl = launch.serving(waiting)
            assert ctl.instance is waiting.instance and ctl.request("version").ok
            assert launch.launched == [ctl.instance]
        """
    )
    run_suite(pytester, *upstream(fake_bin)).assert_outcomes(passed=1)
    assert len(roots) == 1 and not roots[0].exists()


def test_a_failed_first_run_wait_keeps_its_root_through_teardown(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "exit")
    pytester.makepyfile(
        """
        import pytest
        from agterm_conformance.impls.base import LaunchError

        def test_first_run(launch):
            waiting = launch(first_run=True)
            with pytest.raises(LaunchError, match="exited with 3"):
                launch.serving(waiting)
            assert launch.launched == []
        """
    )
    run_suite(pytester, *upstream(fake_bin)).assert_outcomes(passed=1)
    assert len(roots) == 1 and (roots[0] / "app.log").exists()


def test_skip_reason_says_why_this_host_lacks_a_capability(pytester: pytest.Pytester) -> None:
    item = pytester.getitem(
        """
        import pytest

        @pytest.mark.requires("live-sessions", reason="needs zmx")
        def test_live():
            pass
        """,
        "test_live",
    )
    why = "the password-database login shell is /bin/fish, not zsh"
    adapter = SimpleNamespace(
        available=True, name="upstream", profile=Profile("0.36.0", frozenset(), {"live-sessions": why})
    )
    assert skip_reason(item, adapter) == f"unsupported: upstream lacks live-sessions on this host: {why} (needs zmx)"


def test_level_is_checked_before_capabilities(pytester: pytest.Pytester, roots: list[Path], fake_bin: Path) -> None:
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.since("1.0")
        @pytest.mark.requires("window-geometry", reason="no window position here")
        def test_both(ctl):
            pass
        """
    )
    result = run_suite(pytester, *upstream(fake_bin))
    result.stdout.fnmatch_lines(["*0 skip(unsupported), 1 skip(not-yet-at-level)*", "*not-yet-at-level: since 1.0*"])
    assert roots == []


def test_rust_without_a_binary_skips_everything_as_not_yet_at_level(
    pytester: pytest.Pytester, roots: list[Path]
) -> None:
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.since("0.1.0")
        def test_old(ctl):
            pass

        @pytest.mark.requires("posix-shell")
        def test_shell(cli):
            pass

        def test_unmarked(app):
            pass

        def test_needs_no_instance():
            pass
        """
    )
    result = run_suite(pytester, "--impl", "rust")
    result.assert_outcomes(passed=1, skipped=3)
    result.stdout.fnmatch_lines(
        [
            "rust: 0 pass, 0 fail, 0 skip(unsupported), 3 skip(not-yet-at-level), 0 skip(other), 0 reruns, parity 0.0%",
            "*not-yet-at-level: rust has no binary",
        ]
    )
    assert roots == []


RUST_SUITE = """
import pytest

@pytest.mark.since("0.36.0")
def test_at_level(ctl):
    pass

@pytest.mark.since("0.37.0")
def test_above(ctl):
    pass

@pytest.mark.requires("window-geometry", reason="not yet")
def test_geometry(ctl):
    pass
"""


def test_rust_profile_comes_from_version(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    profile = {"parityLevel": "0.36.0", "capabilities": ["posix-shell"]}
    monkeypatch.setenv("FAKE_VERSION", json.dumps({"conformance": profile}))
    pytester.makepyfile(RUST_SUITE)
    result = run_suite(pytester, "--impl", "rust", "--bin", str(fake_bin))
    result.assert_outcomes(passed=1, skipped=2)
    result.stdout.fnmatch_lines(
        ["rust: 1 pass, 0 fail, 1 skip(unsupported), 1 skip(not-yet-at-level), 0 skip(other), 0 reruns, parity 50.0%"]
    )


def test_rust_without_a_profile_is_below_every_level(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path
) -> None:
    pytester.makepyfile(RUST_SUITE)
    result = run_suite(pytester, "--impl", "rust", "--bin", str(fake_bin))
    result.assert_outcomes(skipped=3)
    result.stdout.fnmatch_lines(["*not-yet-at-level: rust reports no parity level"])
    assert len(roots) == 1, "one probe launch, no test instance"


def test_explicit_skips_win_before_the_rust_probe(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FAKE_APP_MODE", "exit")
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.skip(reason="plain")
        def test_skip(ctl):
            pass

        @pytest.mark.skipif(True, reason="never here")
        def test_skipif(cli):
            pass
        """
    )
    result = run_suite(pytester, "--impl", "rust", "--bin", str(fake_bin))
    result.assert_outcomes(skipped=2)
    assert roots == []


@pytest.mark.parametrize(
    ("marker", "message"),
    [
        ('requires("windo-geometry")', "*unknown capability 'windo-geometry'*"),
        ("requires()", "*requires names no capability*"),
        ('since("latest")', "*since: not a dotted version*"),
        ('since("0.1", "0.2")', "*since takes one version string*"),
    ],
)
def test_malformed_markers_are_usage_errors(pytester: pytest.Pytester, marker: str, message: str) -> None:
    pytester.makepyfile(f"import pytest\n\n@pytest.mark.{marker}\ndef test_x():\n    pass\n")
    result = run_suite(pytester, "--impl", "rust")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([message])


def test_reruns_are_counted(pytester: pytest.Pytester, roots: list[Path], fake_bin: Path) -> None:
    pytester.makepyfile(
        """
        calls = []

        def test_flaky(ctl):
            calls.append(1)
            assert len(calls) > 1
        """
    )
    result = run_suite(pytester, *upstream(fake_bin, "--reruns", "1"))
    assert result.parseoutcomes() == {"passed": 1, "rerun": 1}
    result.stdout.fnmatch_lines(["upstream: 1 pass, 0 fail, * 1 reruns, parity 100.0%"])


def test_summary_is_absent_without_conformance_tests(pytester: pytest.Pytester) -> None:
    pytester.makepyfile("def test_plain():\n    pass\n")
    result = run_suite(pytester, "--impl", "rust")
    result.assert_outcomes(passed=1)
    assert "agterm conformance" not in result.stdout.str()


# -- instance fixtures


def test_app_is_per_test_and_shared_app_per_module(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, tmp_path: Path
) -> None:
    pids_file = tmp_path / "pids.txt"
    body = f"""
        import os
        PIDS = {str(pids_file)!r}
        seen = []

        def record(instance):
            with open(PIDS, "a") as sink:
                sink.write(f"{{instance.pid}}\\n")

        def test_shared(ctl, shared_app):
            assert ctl.instance is shared_app
            seen.append(shared_app.pid)
            record(shared_app)

        def test_shared_again(ctl):
            assert ctl.instance.pid == seen[0]

        def test_own(app, ctl):
            assert ctl.instance is app and app.pid not in seen
            seen.append(app.pid)
            record(app)

        def test_own_again(app):
            assert app.pid not in seen
            record(app)
        """
    pytester.makepyfile(test_one=body, test_two=body)
    result = run_suite(pytester, *upstream(fake_bin))
    result.assert_outcomes(passed=8)
    pids = [int(line) for line in pids_file.read_text().split()]
    assert len(pids) == len(set(pids)) == 6
    assert len(roots) == 6 and not any(root.exists() for root in roots)
    for pid in pids:
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)


def test_ctl_waits_through_the_fixture(pytester: pytest.Pytester, roots: list[Path], fake_bin: Path) -> None:
    pytester.makepyfile(
        """
        from agterm_conformance.match import session_ids

        def test_waits(ctl):
            cursor = ctl.events_baseline()
            assert ctl.request("fake.emit", kind="session.created", session="s2").ok
            assert ctl.wait_event(cursor, "session.created")["session"] == "s2"
            assert session_ids(ctl.tree()) == ["s1"]
            assert ctl.wait_tree(lambda tree: session_ids(tree) == ["s1"])
        """
    )
    run_suite(pytester, *upstream(fake_bin)).assert_outcomes(passed=1)


def test_cli_passes_the_instance_socket(
    pytester: pytest.Pytester, roots: list[Path], fake_bin: Path, fake_ctl: Path
) -> None:
    pytester.makepyfile(
        """
        import os

        def test_cli(cli, shared_app):
            out = cli("session", "type", "s1", "--", "echo", "--socket", input="x").json()
            sock = os.fspath(shared_app.socket)
            assert out["argv"] == ["session", "type", "s1", "--socket", sock, "--", "echo", "--socket"]
            assert out["stdin"] == "x"
            assert out["cwd"] == os.fspath(shared_app.home)
            assert out["env"] == {
                "HOME": os.fspath(shared_app.home),
                "AGTERM_STATE_DIR": os.fspath(shared_app.state_dir),
                "PATH": "/usr/bin:/bin",
            }
            plain = cli("tree")
            assert plain.returncode == 0 and plain.json()["argv"] == ["tree", "--socket", sock]

        def test_cli_follows_app(app, cli):
            assert cli("tree").json()["argv"][-1] == os.fspath(app.socket)
        """
    )
    run_suite(pytester, *upstream(fake_bin, "--ctl", str(fake_ctl))).assert_outcomes(passed=2)


def test_cli_local_runs_without_a_socket_in_the_instance_env(ctl: Ctl, fake_ctl: Path) -> None:
    cli = Cli(fake_ctl, ctl.instance)
    out = cli.local("clipboard", "set", "x", input="in").json()
    assert out["argv"] == ["clipboard", "set", "x"] and out["stdin"] == "in"
    assert out["env"]["AGTERM_STATE_DIR"] == os.fspath(ctl.instance.state_dir)
    assert out["cwd"] == os.fspath(ctl.instance.home)


def test_cli_local_refuses_a_default_it_cannot_place_under_the_root(ctl: Ctl, fake_ctl: Path, tmp_path: Path) -> None:
    cli = Cli(fake_ctl, ctl.instance)
    env = dict(ctl.instance.env)
    with pytest.raises(SocketOutsideRoot):
        cli.local("version", env={**env, "AGTERM_STATE_DIR": os.fspath(tmp_path)})
    without = {name: value for name, value in env.items() if name != "AGTERM_STATE_DIR"}
    with pytest.raises(SocketOutsideRoot, match="no AGTERM_STATE_DIR"):
        cli.local("version", env=without)
    with pytest.raises(SocketOutsideRoot):
        cli.local("version", env=without, default_socket=tmp_path / "agterm.sock")
    default = ctl.instance.home / ".local/share/ngterm/ngterm.sock"
    assert cli.local("version", env=without, default_socket=default).json()["env"]["AGTERM_STATE_DIR"] is None


@pytest.fixture
def stream_ctl(tmp_path: Path) -> Path:
    """Prints `ready`, echoes each stdin line back as `got:<line>`, then `bye` at end of input, exiting 3; with
    `hold` as its argument it waits forever once its input ends."""
    script = tmp_path / "stream" / "agtermctl"
    script.parent.mkdir()
    script.write_text(
        f"#!{sys.executable}\n"
        "import sys, time\n"
        "print('ready', flush=True)\n"
        "for line in sys.stdin:\n"
        "    print('got:' + line.rstrip(), flush=True)\n"
        "if 'hold' in sys.argv:\n"
        "    time.sleep(600)\n"
        "print('bye', flush=True)\n"
        "sys.exit(3)\n"
    )
    script.chmod(0o755)
    return script


def test_cli_spawn_reads_lines_as_they_come_and_finishes(ctl: Ctl, stream_ctl: Path) -> None:
    cli = Cli(stream_ctl, ctl.instance)
    with cli.spawn("events", interactive=True) as running:
        assert running.argv[-2:] == ["--socket", os.fspath(ctl.instance.socket)]
        assert running.line() == "ready"
        running.send("one\n")
        assert running.line() == "got:one"
        assert running.process.stdin is not None
        running.process.stdin.close()
        finished = running.finish()
    assert (finished.returncode, finished.stdout, finished.stderr) == (3, "bye\n", "")
    with pytest.raises(AssertionError, match="ended"):
        running.line(timeout=1)


def test_cli_spawn_close_stops_a_command_that_keeps_running(ctl: Ctl, stream_ctl: Path) -> None:
    cli = Cli(stream_ctl, ctl.instance)
    with cli.spawn("events", "hold", input="x\n") as running:
        assert [running.line(), running.line()] == ["ready", "got:x"]
        with pytest.raises(AssertionError, match="no output line"):
            running.line(timeout=0.2)
    assert running.process.returncode == -15


def test_cli_without_agtermctl_fails_clearly(pytester: pytest.Pytester, roots: list[Path], fake_bin: Path) -> None:
    pytester.makepyfile("def test_cli(cli):\n    pass\n")
    result = run_suite(pytester, *upstream(fake_bin))
    result.assert_outcomes(errors=1)
    result.stdout.fnmatch_lines(["*upstream has no agtermctl: pass --ctl*"])


# -- waits


def test_wait_tree_polls_until_the_predicate_holds(ctl: Ctl) -> None:
    assert ctl.request("fake.set_tree", tree=TWO_SESSIONS, delay=0.3).ok
    started = time.monotonic()
    result = ctl.wait_tree(lambda tree: session_ids(tree) == ["s1", "s2"], timeout=5)
    assert session_ids(result) == ["s1", "s2"]
    assert time.monotonic() - started >= 0.25


def test_wait_tree_timeout_names_the_predicate_and_the_last_tree(ctl: Ctl) -> None:
    def has_s9(tree: dict) -> bool:
        return "s9" in session_ids(tree)

    with pytest.raises(WaitTimeout, match=r"tree did not satisfy .*has_s9 within 0.3s; last tree: .*\"s1\""):
        ctl.wait_tree(has_s9, timeout=0.3)


def test_wait_tree_reports_the_last_failed_assertion(ctl: Ctl) -> None:
    def two_sessions(tree: dict) -> bool:
        assert session_ids(tree) == ["s1", "s2"], f"sessions {session_ids(tree)}"
        return True

    with pytest.raises(WaitTimeout, match=r"last check: sessions \['s1'\]"):
        ctl.wait_tree(two_sessions, timeout=0.3)


def _one_session(**fields: object) -> dict:
    return {"workspaces": [{"id": "w1", "active": True, "sessions": [{"id": "s1", "active": True, **fields}]}]}


def test_wait_mounted_polls_until_every_named_pane_is_up(ctl: Ctl) -> None:
    assert ctl.request("fake.set_tree", tree=_one_session(realized=False, hasSplit=True)).ok
    up = _one_session(realized=True, hasSplit=True, splitFontSize=13.0)
    assert ctl.request("fake.set_tree", tree=up, delay=0.3).ok
    started = time.monotonic()
    tree = ctl.wait_mounted("s1", "left", "right", timeout=5)
    assert tree["tree"]["workspaces"][0]["sessions"][0]["splitFontSize"] == 13.0
    assert time.monotonic() - started >= 0.25


def test_wait_mounted_timeout_names_the_pending_pane(ctl: Ctl) -> None:
    assert ctl.request("fake.set_tree", tree=_one_session(realized=True, scratch=True)).ok
    with pytest.raises(WaitTimeout, match=r"last check: not mounted: \['scratch'\]"):
        ctl.wait_mounted("s1", "left", "scratch", timeout=0.3)


def test_wait_mounted_refuses_an_unknown_pane(ctl: Ctl) -> None:
    with pytest.raises(ValueError, match="no mount field for pane 'split'"):
        ctl.wait_mounted("s1", "split")


def test_wait_tree_fails_fast_when_tree_fails(ctl: Ctl) -> None:
    assert ctl.request("fake.set_tree", error="boom").ok
    started = time.monotonic()
    with pytest.raises(AssertionError, match="tree failed while waiting: 'boom'") as caught:
        ctl.wait_tree(lambda tree: False, timeout=5)
    assert not isinstance(caught.value, WaitTimeout)
    assert time.monotonic() - started < 2


def test_event_emitted_between_action_and_wait_is_observed(ctl: Ctl) -> None:
    cursor = ctl.events_baseline()
    assert ctl.request("fake.emit", kind="session.created", session="s2").ok
    # A subscription made only when the wait starts sees nothing: the event is already history.
    late = ctl.events_baseline()
    assert ctl.request("events.read", run=late.run, after=str(late.after)).result["events"]["items"] == []
    event = ctl.wait_event(cursor, "session.created", timeout=1)
    assert event["session"] == "s2" and event["seq"] == cursor.after + 1


def test_event_before_the_baseline_is_not_matched(ctl: Ctl) -> None:
    assert ctl.request("fake.emit", kind="session.created", session="old").ok
    cursor = ctl.events_baseline()
    assert ctl.request("fake.emit", kind="session.created", session="new").ok
    assert ctl.wait_event(cursor, "session.created", timeout=1)["session"] == "new"


def test_wait_event_polls_for_a_late_event(ctl: Ctl) -> None:
    cursor = ctl.events_baseline()
    assert ctl.request("fake.emit", kind="pane.split", session="s1", delay=0.3).ok
    assert ctl.wait_event(cursor, "pane.split", timeout=5)["session"] == "s1"


def test_wait_event_filters_by_kind_and_match(ctl: Ctl) -> None:
    cursor = ctl.events_baseline()
    for kind, status in (("notify", "busy"), ("status", "idle"), ("status", "busy")):
        assert ctl.request("fake.emit", kind=kind, session="s1", payload={"status": status}).ok
    busy = ctl.wait_event(cursor, "status", {"session": "s1", "payload": {"status": "busy"}}, timeout=1)
    assert busy["kind"] == "status" and busy["seq"] == cursor.after + 3
    idle = ctl.wait_event(cursor, "status", lambda event: event["payload"]["status"] == "idle", timeout=1)
    assert idle["seq"] == cursor.after + 2
    after_idle = cursor.past(idle)
    assert after_idle == Cursor(cursor.run, idle["seq"])
    assert ctl.wait_event(after_idle, "status", timeout=1)["seq"] == busy["seq"]


def test_wait_event_timeout_lists_what_it_saw(ctl: Ctl) -> None:
    cursor = ctl.events_baseline()
    for status in ("idle", "idle"):
        assert ctl.request("fake.emit", kind="status", session="s1", payload={"status": status}).ok
    with pytest.raises(WaitTimeout, match=r"no status event matching .*busy.* within 0.3s .*saw 2 non-matching.*idle"):
        ctl.wait_event(cursor, "status", {"payload": {"status": "busy"}}, timeout=0.3)
    with pytest.raises(WaitTimeout, match="no session.closed event within 0.3s .*saw none of that kind"):
        ctl.wait_event(cursor, "session.closed", timeout=0.3)


def test_wait_event_rejects_an_event_of_another_kind() -> None:
    class IgnoresKinds:
        def request(self, cmd: str, target: str | None = None, **args: object) -> Response:
            batch = {"run": "r", "next": 1, "items": [{"seq": 1, "kind": "notify", "session": "s1"}]}
            return Response(ok=True, result={"events": batch}, error=None, raw={})

    with pytest.raises(
        AssertionError,
        match=r"events.read run r after seq 0 kinds \['session.created'\] returned an event of another kind: .*notify",
    ) as caught:
        wait_event(IgnoresKinds(), Cursor("r", 0), "session.created", timeout=5)
    assert not isinstance(caught.value, WaitTimeout)


@pytest.mark.parametrize("seq", [9, 10, "11", None])
def test_wait_event_rejects_an_event_at_or_before_the_cursor(seq: object) -> None:
    class IgnoresCursor:
        def request(self, cmd: str, target: str | None = None, **args: object) -> Response:
            batch = {"run": "r", "next": 11, "items": [{"seq": seq, "kind": "status", "session": "s1"}]}
            return Response(ok=True, result={"events": batch}, error=None, raw={})

    with pytest.raises(
        AssertionError, match="events.read run r after seq 10 .*returned next 11 with an event outside it: .*status"
    ) as caught:
        wait_event(IgnoresCursor(), Cursor("r", 10), "status", timeout=5)
    assert not isinstance(caught.value, WaitTimeout)


class ScriptedEvents:
    def __init__(self, *pages: dict[str, object]) -> None:
        self.pages = pages
        self.afters: list[str] = []

    def request(self, cmd: str, target: str | None = None, **args: object) -> Response:
        self.afters.append(str(args["after"]))
        page = self.pages[min(len(self.afters), len(self.pages)) - 1]
        return Response(ok=True, result={"events": {"run": "r", **page}}, error=None, raw={})


def _status(seq: object, kind: str = "status") -> dict[str, object]:
    return {"seq": seq, "kind": kind, "session": "s1"}


@pytest.mark.parametrize(
    ("pages", "error", "requests"),
    [
        pytest.param([{"run": "other", "next": 10, "items": []}], "returned run .*other", 1, id="run-mismatch"),
        pytest.param([{"next": True, "items": []}], "returned next true", 1, id="bool-next"),
        pytest.param([{"next": "11", "items": []}], 'returned next "11"', 1, id="string-next"),
        pytest.param(
            [{"next": 0, "items": []}, {"next": 5, "items": [_status(5)]}],
            "after seq 10 .*returned next 0",
            1,
            id="next-below-baseline",
        ),
        pytest.param(
            [{"next": 12, "items": []}, {"next": 11, "items": []}], "after seq 12 .*returned next 11", 2, id="next-back"
        ),
        pytest.param(
            [{"next": 11, "items": [_status(12)]}], "returned next 11 with an event outside it", 1, id="seq-ahead"
        ),
        pytest.param(
            [{"next": 12, "items": [_status(11), _status(12, "notify")]}],
            "another kind: .*notify",
            1,
            id="bad-later-kind",
        ),
        pytest.param(
            [{"next": 12, "items": [_status(11), _status(9)]}], "event outside it: .*9", 1, id="bad-later-seq"
        ),
    ],
)
def test_wait_event_rejects_a_malformed_page(pages: list[dict[str, object]], error: str, requests: int) -> None:
    requester = ScriptedEvents(*pages)
    with pytest.raises(AssertionError, match=f"events.read run r .*{error}") as caught:
        wait_event(requester, Cursor("r", 10), "status", timeout=1)
    assert not isinstance(caught.value, WaitTimeout)
    assert len(requester.afters) == requests


def test_wait_event_accepts_an_unchanged_next_on_an_empty_poll() -> None:
    requester = ScriptedEvents({"next": 10, "items": []}, {"next": 11, "items": [_status(11)]})
    assert wait_event(requester, Cursor("r", 10), "status", timeout=5)["seq"] == 11
    assert requester.afters == ["10", "10"]


def test_wait_event_fails_fast_on_a_cursor_error(ctl: Ctl) -> None:
    started = time.monotonic()
    with pytest.raises(AssertionError, match="event run changed") as caught:
        ctl.wait_event(Cursor(run="00000000-0000-0000-0000-000000000000", after=0), "status", timeout=5)
    assert not isinstance(caught.value, WaitTimeout)
    assert time.monotonic() - started < 2


def test_ctl_tree_returns_the_result(ctl: Ctl) -> None:
    assert isinstance(ctl.instance, Instance)
    assert session_ids(ctl.tree()) == ["s1"]


class ScriptedText:
    def __init__(self, *responses: Response) -> None:
        self.responses = responses
        self.calls: list[tuple[str, str | None, dict[str, object]]] = []

    def request(self, cmd: str, target: str | None = None, **args: object) -> Response:
        self.calls.append((cmd, target, args))
        return self.responses[min(len(self.calls), len(self.responses)) - 1]


def _text(text: object) -> Response:
    return Response(ok=True, result={"text": text}, error=None, raw={"ok": True, "result": {"text": text}})


def test_wait_text_polls_until_the_pattern_appears() -> None:
    pane = ScriptedText(_text("$ printf 'x-%s' 1\n"), _text("$ printf 'x-%s' 1\nx-1\n$ "))
    found = wait_text(pane, "s1", r"(?m)^x-(\d)$", timeout=5, pane="right")
    assert found.group(1) == "1"
    assert pane.calls == [("session.text", "s1", {"pane": "right"})] * 2


def test_wait_text_timeout_shows_the_last_text() -> None:
    with pytest.raises(WaitTimeout, match=r"session.text of s1 did not match 'marker' within 0.2s; last text: '\$ '"):
        wait_text(ScriptedText(_text("$ ")), "s1", "marker", timeout=0.2)


@pytest.mark.parametrize(
    ("response", "message"),
    [
        (
            Response(ok=False, result=None, error="no session nope", raw={}),
            "session.text failed while waiting: 'no session nope'",
        ),
        (_text(None), "session.text returned no text"),
    ],
)
def test_wait_text_fails_fast_on_a_broken_reply(response: Response, message: str) -> None:
    pane = ScriptedText(response)
    with pytest.raises(AssertionError, match=message) as caught:
        wait_text(pane, "s1", "marker", timeout=5)
    assert not isinstance(caught.value, WaitTimeout) and len(pane.calls) == 1


def _overlay_text(ok: bool, text: str | None = None, error: str | None = None) -> Response:
    result = None if text is None else {"text": text}
    return Response(ok=ok, result=result, error=error, raw={"ok": ok, "result": result, "error": error})


def _marked(response: Response) -> bool:
    assert response.ok, response.error
    return "MARK" in response.result["text"]


def test_wait_response_repeats_the_request_until_accepted() -> None:
    overlay = ScriptedText(
        _overlay_text(False, error="overlay not realized"), _overlay_text(True, ""), _overlay_text(True, "MARK")
    )
    response = wait_response(overlay, "session.overlay.text", "s1", _marked, timeout=5, lines=2)
    assert response.result == {"text": "MARK"}
    assert overlay.calls == [("session.overlay.text", "s1", {"lines": 2})] * 3


def test_wait_response_timeout_shows_the_last_check_and_reply() -> None:
    overlay = ScriptedText(_overlay_text(False, error="overlay not realized"))
    expected = (
        r"(?s)did not satisfy _marked within 0.2s; last check: overlay not realized.*; last reply: .*not realized"
    )
    with pytest.raises(WaitTimeout, match=expected):
        wait_response(overlay, "session.overlay.text", "s1", _marked, timeout=0.2)


def test_wait_response_lets_other_errors_through() -> None:
    def broken(response: Response) -> bool:
        raise KeyError("text")

    overlay = ScriptedText(_overlay_text(True, "x"))
    with pytest.raises(KeyError):
        wait_response(overlay, "session.overlay.text", "s1", broken, timeout=5)
    assert len(overlay.calls) == 1


def test_ctl_wait_response_drives_the_instance(ctl: Ctl) -> None:
    response = ctl.wait_response("tree", None, lambda reply: reply.ok and session_ids(reply.result) == ["s1"])
    assert response.ok


def test_wait_until_returns_the_first_truthy_value() -> None:
    values = iter([None, "", "written"])
    assert wait_until(lambda: next(values), timeout=5) == "written"


def test_wait_until_timeout_names_the_check_and_its_last_failure() -> None:
    def file_written() -> bool:
        raise AssertionError("no marker yet")

    with pytest.raises(WaitTimeout, match=r"file_written did not hold within 0.2s; last check: no marker yet"):
        wait_until(file_written, timeout=0.2)


def test_wait_frame_acks_pings_and_skips_other_frames() -> None:
    frames = iter([{"kind": "ping", "gen": 3}, {"kind": "hud"}, {"kind": "status", "status": {}}])
    sent = []
    assert wait_frame(lambda seconds: next(frames), "status", sent.append, timeout=5) == {
        "kind": "status",
        "status": {},
    }
    assert sent == [{"kind": "ack", "gen": 3, "rev": 0}]


def test_wait_frame_has_one_deadline_however_often_pings_arrive() -> None:
    waits = [0.2]

    def read(seconds: float) -> dict:
        assert seconds <= waits[-1]
        waits[-1] = seconds
        return {"kind": "ping", "gen": 1}

    with pytest.raises(WaitTimeout, match=r"no status frame within 0.2s; last skipped: \['ping'"):
        wait_frame(read, "status", lambda frame: None, timeout=0.2)


def test_wait_until_lets_other_errors_through() -> None:
    calls = []

    def broken() -> bool:
        calls.append(1)
        raise KeyError("marker")

    with pytest.raises(KeyError):
        wait_until(broken, timeout=5)
    assert calls == [1]
