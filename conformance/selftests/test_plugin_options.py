import json
from pathlib import Path
from typing import Any

import pytest

from agterm_conformance.plugin import ImplOptions, get_impl_options, scrub_environ

USES_IMPL = """
def test_uses_impl(impl_options):
    pass
"""


@pytest.fixture
def bin_path(tmp_path: Path) -> Path:
    path = tmp_path / "agterm"
    path.write_text("")
    return path


def test_options_parse(pytester: pytest.Pytester, bin_path: Path) -> None:
    config = pytester.parseconfigure(
        "--impl", "swift-linux", "--bin", str(bin_path), "--ctl", str(bin_path), "--coverage-gate"
    )
    assert get_impl_options(config) == ImplOptions(impl="swift-linux", bin=bin_path, ctl=bin_path, coverage_gate=True)


def test_options_default(pytester: pytest.Pytester) -> None:
    config = pytester.parseconfigure()
    assert get_impl_options(config) == ImplOptions(impl=None, bin=None, ctl=None, coverage_gate=False)


def test_macos_launch_mode_option(pytester: pytest.Pytester) -> None:
    assert get_impl_options(pytester.parseconfigure()).macos_launch_mode == "uitest"
    assert get_impl_options(pytester.parseconfigure("--macos-launch-mode", "plain")).macos_launch_mode == "plain"
    result = pytester.runpytest("--macos-launch-mode", "open")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--macos-launch-mode: invalid choice: 'open'*"])


def test_rust_frontend_option(pytester: pytest.Pytester) -> None:
    assert get_impl_options(pytester.parseconfigure()).rust_frontend == "gtk"
    assert get_impl_options(pytester.parseconfigure("--rust-frontend", "gpui")).rust_frontend == "gpui"
    result = pytester.runpytest("--rust-frontend", "qt")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--rust-frontend: invalid choice: 'qt'*"])


def test_options_reach_fixture(pytester: pytest.Pytester, bin_path: Path) -> None:
    pytester.makepyfile(
        f"""
        def test_options(impl_options):
            assert impl_options.impl == "swift-macos"
            assert str(impl_options.bin) == {str(bin_path)!r}
            assert impl_options.ctl is None
        """
    )
    pytester.runpytest("--impl", "swift-macos", "--bin", str(bin_path)).assert_outcomes(passed=1)


def test_invalid_impl(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--impl", "bogus")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--impl*invalid choice*bogus*"])


def test_missing_impl(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(USES_IMPL)
    result = pytester.runpytest()
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--impl is required*swift-macos, swift-linux, rust*"])


def test_missing_impl_ignored_without_impl_fixture(pytester: pytest.Pytester) -> None:
    pytester.makepyfile("def test_plain(): pass")
    pytester.runpytest().assert_outcomes(passed=1)


def test_missing_impl_ignored_when_deselected(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(USES_IMPL + "\ndef test_plain(): pass\n")
    pytester.runpytest("-k", "plain").assert_outcomes(passed=1, deselected=1)


@pytest.mark.parametrize("impl", ["swift-macos", "swift-linux"])
def test_missing_bin_for_swift(pytester: pytest.Pytester, impl: str) -> None:
    pytester.makepyfile(USES_IMPL)
    result = pytester.runpytest("--impl", impl)
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([f"*--bin is required for --impl={impl}*"])


def test_rust_runs_without_bin(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        """
        def test_no_bin(impl_options):
            pass

        def test_options(pytestconfig):
            from agterm_conformance.plugin import get_impl_options

            options = get_impl_options(pytestconfig)
            assert options.impl == "rust" and options.bin is None
        """
    )
    # Not a usage error: the conformance test skips as not-yet-at-level.
    pytester.runpytest("--impl", "rust").assert_outcomes(passed=1, skipped=1)


@pytest.mark.parametrize("flag", ["--bin", "--ctl"])
def test_nonexistent_path(pytester: pytest.Pytester, tmp_path: Path, flag: str) -> None:
    missing = tmp_path / "missing"
    result = pytester.runpytest("--impl", "rust", flag, str(missing))
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([f"*{flag} {missing} does not exist*"])


def test_markers_registered(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--markers")
    result.stdout.fnmatch_lines(["@pytest.mark.since(version):*", "@pytest.mark.requires(capability, reason=...):*"])
    result.stdout.fnmatch_lines(["@pytest.mark.covers(*items, streams=()):*"])


def test_markers_pass_strict(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        """
        import pytest

        @pytest.mark.since("0.36.0")
        @pytest.mark.requires("window-geometry", reason="GTK4 cannot restore x/y")
        @pytest.mark.covers("session.resize", "event:pane.split")
        def test_marked():
            pass
        """
    )
    pytester.runpytest("--strict-markers").assert_outcomes(passed=1)


def test_scrub_environ_prefixes() -> None:
    environ = {"AGTERM_SOCKET": "/s", "ZMX_DIR": "/z", "AGTERMISH": "1", "MY_AGTERM_X": "1", "PATH": "/bin"}
    assert scrub_environ(environ) == ["AGTERM_SOCKET", "ZMX_DIR"]
    assert environ == {"AGTERMISH": "1", "MY_AGTERM_X": "1", "PATH": "/bin"}


def test_env_scrubbed_at_plugin_load(pytester: pytest.Pytester, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AGTERM_SOCKET", "/live/agterm.sock")
    monkeypatch.setenv("AGTERM_SESSION_ID", "live-session")
    monkeypatch.setenv("ZMX_DIR", "/live/zmx")
    monkeypatch.setenv("AGTERMISH", "kept")
    pytester.makepyfile(
        """
        import os, subprocess, sys

        LIVE = {"AGTERM_SOCKET", "AGTERM_SESSION_ID", "ZMX_DIR"}

        def test_env():
            assert LIVE.isdisjoint(os.environ)
            assert os.environ["AGTERMISH"] == "kept"
            child = subprocess.run(
                [sys.executable, "-c", "import os; print(chr(10).join(os.environ))"],
                capture_output=True, text=True, check=True,
            )
            assert LIVE.isdisjoint(child.stdout.splitlines())
        """
    )
    result = pytester.runpytest_subprocess()
    result.assert_outcomes(passed=1)
    result.stdout.fnmatch_lines(["scrubbed from env: AGTERM_SESSION_ID, AGTERM_SOCKET, ZMX_DIR"])


MUST_PASS_TESTS = """
import pytest

@pytest.mark.requires("live-sessions")
def test_live_one():
    pass

@pytest.mark.requires("live-sessions")
def test_live_two():
    {body}

def test_plain():
    pass
"""


@pytest.mark.parametrize(
    ("body", "args", "ret"),
    [
        ("pass", (), pytest.ExitCode.OK),
        ("pytest.skip('unsupported: no zsh')", (), pytest.ExitCode.TESTS_FAILED),
        ("assert False", (), pytest.ExitCode.TESTS_FAILED),
        ("pass", ("-k", "not two"), pytest.ExitCode.TESTS_FAILED),
    ],
)
def test_must_pass_needs_every_required_test_to_pass(
    pytester: pytest.Pytester, body: str, args: tuple[str, ...], ret: pytest.ExitCode
) -> None:
    pytester.makepyfile(MUST_PASS_TESTS.format(body=body))
    result = pytester.runpytest("--must-pass", "live-sessions", *args)
    assert result.ret == ret
    if ret != pytest.ExitCode.OK:
        result.stdout.fnmatch_lines(["*--must-pass live-sessions: not passed*", "*test_live_two*"])


def test_must_pass_with_no_required_test_fails(pytester: pytest.Pytester) -> None:
    pytester.makepyfile("def test_plain():\n    pass\n")
    result = pytester.runpytest("--must-pass", "live-sessions")
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*no collected test requires live-sessions*"])


def test_must_pass_needs_a_collected_test_for_each_capability(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(MUST_PASS_TESTS.format(body="pass"))
    result = pytester.runpytest("--must-pass", "live-sessions", "--must-pass", "posix-shell")
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    result.stdout.fnmatch_lines(["*no collected test requires posix-shell*"])
    result.stdout.no_fnmatch_line("*no collected test requires live-sessions*")


def test_must_pass_refuses_an_unknown_capability(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--must-pass", "live-session")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--must-pass: unknown capability live-session*"])


def run_nodeid_report(
    pytester: pytest.Pytester, *args: str, subprocess: bool = False
) -> tuple[pytest.RunResult, list[dict[str, Any]]]:
    """Runs pytester with a report in a not-yet-existing directory and reads its JSON lines back."""
    run = pytester.runpytest_subprocess if subprocess else pytester.runpytest
    result = run("--nodeid-report", "out/nodeids.jsonl", *args)
    lines = (pytester.path / "out" / "nodeids.jsonl").read_text(encoding="utf-8").splitlines()
    return result, [json.loads(line) for line in lines]


def outcomes(records: list[dict[str, Any]]) -> list[tuple[str, str, str | None, str | None]]:
    return [(r["nodeid"], r["outcome"], r["skip"], r["reason"]) for r in records if r["kind"] == "test"]


def test_nodeid_report_merges_phases(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        test_phases="""
        import pytest

        @pytest.fixture
        def broken_setup():
            raise RuntimeError("setup")

        @pytest.fixture
        def broken_teardown():
            yield
            raise RuntimeError("teardown")

        def test_pass():
            pass

        def test_fail():
            assert False

        def test_setup_error(broken_setup):
            pass

        def test_teardown_error(broken_teardown):
            pass

        def test_fail_then_teardown_error(broken_teardown):
            assert False

        def test_skip_then_teardown_error(broken_teardown):
            pytest.skip("unsupported: rust lacks posix-shell")

        def test_unsupported():
            pytest.skip("unsupported: rust lacks posix-shell on this host: no zsh (needs zsh)")

        def test_not_yet():
            pytest.skip("not-yet-at-level: since 0.40.0, rust is at 0.36.0")

        def test_other():
            pytest.skip("no display")

        @pytest.mark.skip(reason="marker skip")
        def test_marker_skip():
            pass

        @pytest.mark.parametrize("text", ["   "])
        def test_spaces(text):
            pass
        """
    )
    result, records = run_nodeid_report(pytester)
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    assert records[0] == {"kind": "run", "partial": False, "mode": "full"}
    unsupported = "unsupported: rust lacks posix-shell on this host: no zsh (needs zsh)"
    not_yet = "not-yet-at-level: since 0.40.0, rust is at 0.36.0"
    assert outcomes(records) == [
        ("test_phases.py::test_pass", "passed", None, None),
        ("test_phases.py::test_fail", "failed", None, None),
        ("test_phases.py::test_setup_error", "error", None, None),
        ("test_phases.py::test_teardown_error", "error", None, None),
        ("test_phases.py::test_fail_then_teardown_error", "failed", None, None),
        ("test_phases.py::test_skip_then_teardown_error", "error", None, None),
        ("test_phases.py::test_unsupported", "skipped", "unsupported", unsupported),
        ("test_phases.py::test_not_yet", "skipped", "not-yet-at-level", not_yet),
        ("test_phases.py::test_other", "skipped", "other", "no display"),
        ("test_phases.py::test_marker_skip", "skipped", "other", "marker skip"),
        ("test_phases.py::test_spaces[   ]", "passed", None, None),
    ]
    assert len(records) == 12


def test_nodeid_report_module_teardown_error_lands_on_last_test(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        test_module_fixture="""
        import pytest

        @pytest.fixture(scope="module")
        def shared():
            yield
            raise RuntimeError("module teardown")

        def test_first(shared):
            pass

        def test_last(shared):
            pass
        """
    )
    _, records = run_nodeid_report(pytester)
    assert outcomes(records) == [
        ("test_module_fixture.py::test_first", "passed", None, None),
        ("test_module_fixture.py::test_last", "error", None, None),
    ]


def test_nodeid_report_xfail(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        test_xfail="""
        import pytest

        @pytest.mark.xfail(reason="known bug")
        def test_xfailed():
            assert False

        @pytest.mark.xfail(reason="flaky")
        def test_xpassed():
            pass

        @pytest.mark.xfail(reason="fixed", strict=True)
        def test_strict_xpass():
            pass

        def test_imperative():
            pytest.xfail("later")
        """
    )
    _, records = run_nodeid_report(pytester)
    assert outcomes(records) == [
        ("test_xfail.py::test_xfailed", "xfailed", None, "known bug"),
        ("test_xfail.py::test_xpassed", "xpassed", None, "flaky"),
        ("test_xfail.py::test_strict_xpass", "failed", None, None),
        ("test_xfail.py::test_imperative", "xfailed", None, "later"),
    ]


def test_nodeid_report_rerun_discards_earlier_attempts(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(
        test_rerun="""
        def test_flaky(tmp_path_factory):
            tried = tmp_path_factory.getbasetemp() / "tried"
            if not tried.exists():
                tried.write_text("")
                assert False

        def test_broken():
            assert False
        """
    )
    _, records = run_nodeid_report(pytester, "--reruns", "1")
    assert outcomes(records) == [
        ("test_rerun.py::test_flaky", "passed", None, None),
        ("test_rerun.py::test_broken", "failed", None, None),
    ]


def test_nodeid_report_collection_error(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(test_ok="def test_ok():\n    pass\n", test_broken="import no_such_module_xyz\n")
    result, records = run_nodeid_report(pytester)
    assert result.ret == pytest.ExitCode.INTERRUPTED
    assert records[0] == {"kind": "run", "partial": False, "mode": "full"}
    # A collection error stops the session before any test runs.
    assert outcomes(records) == [("test_ok.py::test_ok", "not-run", None, None)]
    (error,) = [r for r in records if r["kind"] == "collection-error"]
    assert error["nodeid"] == "test_broken.py"
    assert "no_such_module_xyz" in error["message"]


@pytest.mark.parametrize(
    ("args", "partial"),
    [((), False), (("-k", "not two"), True), (("test_partial.py::test_one",), True)],
)
def test_nodeid_report_partial(pytester: pytest.Pytester, args: tuple[str, ...], partial: bool) -> None:
    pytester.makepyfile(test_partial="def test_one():\n    pass\n\ndef test_two():\n    pass\n")
    _, records = run_nodeid_report(pytester, *args)
    assert records[0] == {"kind": "run", "partial": partial, "mode": "partial" if partial else "full"}
    expected = ["test_partial.py::test_one"] + ([] if partial else ["test_partial.py::test_two"])
    assert [nodeid for nodeid, *_ in outcomes(records)] == expected


RATCHET_ONLY_TESTS = """
import pytest

def test_one():
    pass

def test_two():
    pass

@pytest.mark.parametrize("name", ["alpha", "beta"])
def test_three(name):
    pass
"""


def ratchet_list(pytester: pytest.Pytester, *ids: str) -> str:
    (pytester.path / "list.txt").write_text("# header\n" + "".join(f"{nodeid}\n" for nodeid in ids))
    return "list.txt"


def ran(records: list[dict[str, Any]]) -> list[str]:
    return [nodeid for nodeid, *_ in outcomes(records)]


def test_ratchet_only_runs_the_listed_ids(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(test_r=RATCHET_ONLY_TESTS)
    listed = ratchet_list(pytester, "test_r.py::test_three[beta]", "test_r.py::test_two", "test_r.py::test_gone")
    result, records = run_nodeid_report(pytester, "--ratchet-only", listed)
    assert result.ret == pytest.ExitCode.OK
    assert records[0] == {"kind": "run", "partial": True, "mode": "ratchet-only"}
    assert ran(records) == ["test_r.py::test_two", "test_r.py::test_three[beta]"]


ONE, TWO = "test_r.py::test_one", "test_r.py::test_two"
ALPHA, BETA = "test_r.py::test_three[alpha]", "test_r.py::test_three[beta]"


@pytest.mark.parametrize(
    ("extra", "added"),
    [
        (("--also", ONE), [ONE]),
        (("--also", "test_r.py::test_three"), [ALPHA, BETA]),
        (("--also", "test_r.py"), [ONE, ALPHA, BETA]),
        (("--also-k", "one or alpha"), [ONE, ALPHA]),
        (("--also", ONE, "--also-k", "three and beta"), [ONE, BETA]),
    ],
)
def test_ratchet_only_adds_the_extra_selections(
    pytester: pytest.Pytester, extra: tuple[str, ...], added: list[str]
) -> None:
    pytester.makepyfile(test_r=RATCHET_ONLY_TESTS)
    listed = ratchet_list(pytester, TWO)
    _, records = run_nodeid_report(pytester, "--ratchet-only", listed, *extra)
    assert records[0]["mode"] == "ratchet-only"
    assert ran(records) == [nodeid for nodeid in (ONE, TWO, ALPHA, BETA) if nodeid in {TWO, *added}]


@pytest.mark.parametrize("narrowing", [("-k", "not two"), ("test_r.py::test_two",)])
def test_ratchet_only_narrowed_further_is_a_partial_run(pytester: pytest.Pytester, narrowing: tuple[str, ...]) -> None:
    """Some listed ids did not run on purpose: the report must not claim every listed id was selected."""
    pytester.makepyfile(test_r=RATCHET_ONLY_TESTS)
    listed = ratchet_list(pytester, "test_r.py::test_one", "test_r.py::test_two")
    _, records = run_nodeid_report(pytester, "--ratchet-only", listed, *narrowing)
    assert records[0] == {"kind": "run", "partial": True, "mode": "partial"}


@pytest.mark.parametrize(
    ("args", "message"),
    [
        (("--also", "test_r.py"), "--also and --also-k need --ratchet-only"),
        (("--also-k", "one"), "--also and --also-k need --ratchet-only"),
        (("--ratchet-only", "absent.txt"), "--ratchet-only: cannot read"),
        (("--ratchet-only", "list.txt", "--also-k", "one and"), "--also-k: bad expression"),
        (("--ratchet-only", "list.txt", "--coverage-gate"), "--ratchet-only cannot be combined with --coverage-gate"),
    ],
)
def test_ratchet_only_usage_errors(pytester: pytest.Pytester, args: tuple[str, ...], message: str) -> None:
    pytester.makepyfile(test_r=RATCHET_ONLY_TESTS)
    ratchet_list(pytester, "test_r.py::test_one")
    result = pytester.runpytest(*args)
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([f"*{message}*"])


def test_nodeid_report_must_pass_problems(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(test_must=MUST_PASS_TESTS.format(body="pytest.skip('unsupported: no zsh')"))
    result, records = run_nodeid_report(pytester, "--must-pass", "live-sessions", "--must-pass", "posix-shell")
    assert result.ret == pytest.ExitCode.TESTS_FAILED
    assert [r for r in records if r["kind"] == "must-pass"] == [
        {"kind": "must-pass", "problem": "no collected test requires posix-shell"},
        {"kind": "must-pass", "problem": "test_must.py::test_live_two"},
    ]


@pytest.mark.parametrize(
    ("body", "stopped"),
    [("assert False", "failed"), ("raise KeyboardInterrupt", "not-run")],
)
def test_nodeid_report_not_run(pytester: pytest.Pytester, body: str, stopped: str) -> None:
    pytester.makepyfile(test_stop=f"def test_stop():\n    {body}\n\ndef test_after():\n    pass\n")
    # A subprocess: in-process pytester re-raises a KeyboardInterrupt into this session.
    _, records = run_nodeid_report(pytester, "-x", subprocess=True)
    assert records[0] == {"kind": "run", "partial": False, "mode": "full"}
    assert outcomes(records) == [
        ("test_stop.py::test_stop", stopped, None, None),
        ("test_stop.py::test_after", "not-run", None, None),
    ]


def test_nodeid_report_refused_under_coverage_gate(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--nodeid-report", "nodeids.jsonl", "--coverage-gate")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--nodeid-report cannot be combined with --coverage-gate*"])
    assert not (pytester.path / "nodeids.jsonl").exists()


@pytest.mark.parametrize("target", ["test_absent.py", "test_here.py::test_absent"])
def test_nodeid_report_not_written_on_a_usage_error(pytester: pytest.Pytester, target: str) -> None:
    """pytest refusing its arguments must leave no report, or the ratchet would read an empty run as holding."""
    pytester.makepyfile(test_here="def test_here():\n    pass\n")
    result = pytester.runpytest("--nodeid-report", "out/nodeids.jsonl", target)
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    assert not (pytester.path / "out" / "nodeids.jsonl").exists()
