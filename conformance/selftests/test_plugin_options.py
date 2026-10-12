from pathlib import Path

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


def test_must_pass_refuses_an_unknown_capability(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--must-pass", "live-session")
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines(["*--must-pass: unknown capability live-session*"])
