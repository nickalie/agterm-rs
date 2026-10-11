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


def test_missing_impl_ignored_for_coverage_gate(pytester: pytest.Pytester) -> None:
    pytester.makepyfile(USES_IMPL)
    assert pytester.runpytest("--coverage-gate").ret == pytest.ExitCode.OK


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
            assert impl_options.impl == "rust"
            assert impl_options.bin is None
        """
    )
    pytester.runpytest("--impl", "rust").assert_outcomes(passed=1)


@pytest.mark.parametrize("flag", ["--bin", "--ctl"])
def test_nonexistent_path(pytester: pytest.Pytester, tmp_path: Path, flag: str) -> None:
    missing = tmp_path / "missing"
    result = pytester.runpytest("--impl", "rust", flag, str(missing))
    assert result.ret == pytest.ExitCode.USAGE_ERROR
    result.stderr.fnmatch_lines([f"*{flag} {missing} does not exist*"])


def test_markers_registered(pytester: pytest.Pytester) -> None:
    result = pytester.runpytest("--markers")
    result.stdout.fnmatch_lines(["@pytest.mark.since(version):*", "@pytest.mark.requires(capability, reason=...):*"])
    result.stdout.fnmatch_lines(["@pytest.mark.covers(*items):*"])


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
