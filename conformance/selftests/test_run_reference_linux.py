import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "tools" / "run-reference-linux.sh"

VERSION_TEST = """
from agterm_conformance.match import assert_ok

def test_reports_version(ctl):
    assert assert_ok(ctl.request("version"))["app"]["version"]
"""

OTHER_TEST = """
def test_other(ctl):
    pass
"""

LEAKING_TEST = """
from agterm_conformance.impls.base import make_root

def test_leaks_a_root(ctl):
    make_root()
"""


@pytest.fixture
def layers(tmp_path: Path) -> Path:
    tests = tmp_path / "tests"
    (tests / "protocol").mkdir(parents=True)
    (tests / "protocol" / "test_probe.py").write_text(VERSION_TEST)
    return tests


@pytest.fixture
def install(fake_bin: Path) -> Path:
    share = fake_bin.parent.parent / "share" / "agterm"
    share.mkdir(parents=True)
    (share / "VERSION").write_text("9.9.9\n")
    (share / "COMMIT").write_text("abc1234\n")
    return fake_bin


@pytest.fixture(autouse=True)
def weston_on_path(
    tmp_path: Path, fake_compositor_bin: Path, fake_session_bus_bin: Path, monkeypatch: pytest.MonkeyPatch
) -> Path:
    """The script's pytest is a subprocess the conftest patch cannot reach: it finds the fake weston and bus on PATH."""
    directory = tmp_path / "weston-bin"
    directory.mkdir()
    (directory / "weston").symlink_to(fake_compositor_bin)
    (directory / "dbus-daemon").symlink_to(fake_session_bus_bin)
    monkeypatch.setenv("PATH", f"{directory}{os.pathsep}{os.environ['PATH']}")
    return directory


def run(tmp_path: Path, *args: str, script: Path = SCRIPT, **env: str) -> tuple[subprocess.CompletedProcess[str], str]:
    out = tmp_path / "report"
    completed = subprocess.run(
        [str(script), "--out", str(out), *args],
        env={**os.environ, "PYTHON": sys.executable, **env},
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    report = out / "report.md"
    return completed, report.read_text() if report.exists() else ""


def test_a_green_run_writes_the_report(tmp_path: Path, install: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(OTHER_TEST)
    (tmp_path / "report").mkdir()
    (tmp_path / "report" / "keep.txt").write_text("mine")
    (tmp_path / "report" / "report.md").write_text("stale")
    completed, report = run(
        tmp_path, "--bin", str(install), "--tests", str(layers), "--", "-k", "version", AGTERM_SOCKET="/nowhere"
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "fork: 9.9.9 (abc1234)" in report
    assert "layers: protocol" in report
    assert "display: headless" in report
    assert "pytest args: -k version" in report
    assert "swift-linux: 1 pass, 0 fail" in report
    assert "Left behind" not in report
    assert (tmp_path / "report" / "junit.xml").is_file()
    assert "scrubbed from env" not in (tmp_path / "report" / "pytest.log").read_text()
    assert (tmp_path / "report" / "keep.txt").read_text() == "mine"


def test_pytest_args_reach_pytest(tmp_path: Path, install: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(OTHER_TEST)
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers))
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "swift-linux: 2 pass, 0 fail" in report


def test_a_failing_test_fails_the_run(tmp_path: Path, install: Path, layers: Path) -> None:
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers), FAKE_VERSION='{"app": {}}')
    assert completed.returncode == 1
    assert "swift-linux: 0 pass, 1 fail" in report
    assert "FAILED" in report


def test_a_root_left_behind_fails_a_green_run(tmp_path: Path, install: Path, layers: Path) -> None:
    (layers / "protocol" / "test_probe.py").write_text(LEAKING_TEST)
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers))
    left = [Path(root) for root in re.findall(r"`(/tmp/agt-[^`]+)`", report)]
    ours = [root for root in left if root.parent.name.startswith("agt-run-")]
    for root in ours:
        shutil.rmtree(root, ignore_errors=True)
        root.parent.rmdir()
    assert completed.returncode == 1
    assert "swift-linux: 1 pass, 0 fail" in report
    assert len(ours) == len(left) == 1


@pytest.mark.parametrize("untracked", [False, True])
def test_an_untracked_suite_file_marks_the_suite_dirty(
    tmp_path: Path, install: Path, layers: Path, untracked: bool
) -> None:
    repo = tmp_path / "repo"
    script = repo / "conformance" / "tools" / SCRIPT.name
    script.parent.mkdir(parents=True)
    shutil.copy2(SCRIPT, script)
    git = ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
    subprocess.run([*git, "init", "-q"], check=True)
    subprocess.run([*git, "add", "."], check=True)
    subprocess.run([*git, "commit", "-qm", "suite"], check=True)
    if untracked:
        (repo / "conformance" / "test_new.py").write_text("")
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers), script=script)
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert ("(dirty)" in report) == untracked


@pytest.mark.parametrize("case", ["missing bin", "no layer"])
def test_bad_arguments_exit_2(tmp_path: Path, install: Path, case: str) -> None:
    args = ["--bin", str(tmp_path / "absent")] if case == "missing bin" else ["--bin", str(install), "--tests", "/"]
    completed, _ = run(tmp_path, *args)
    assert completed.returncode == 2


@pytest.mark.parametrize("missing", ["weston", "dbus-daemon"])
def test_a_headless_run_without_weston_or_a_bus_names_the_opt_out(
    tmp_path: Path, install: Path, layers: Path, weston_on_path: Path, monkeypatch: pytest.MonkeyPatch, missing: str
) -> None:
    bare = tmp_path / "bare-bin"
    bare.mkdir()
    for tool in ("bash", "dirname", "readlink", "cat", "git", "grep", "mktemp", "rmdir", "rm", "mkdir", "env"):
        if found := shutil.which(tool):
            (bare / tool).symlink_to(found)
    for helper in {"weston", "dbus-daemon"} - {missing}:
        (bare / helper).symlink_to(weston_on_path / helper)
    monkeypatch.setenv("PATH", str(bare))
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers))
    assert completed.returncode == 2
    assert f"no {missing} on PATH" in completed.stderr and "CONFORMANCE_LINUX_DISPLAY=session" in completed.stderr
    assert report == ""


def test_the_script_runs_its_own_checkouts_harness(tmp_path: Path, install: Path, layers: Path) -> None:
    (layers / "protocol" / "test_where.py").write_text(
        "import agterm_conformance\n\ndef test_where(ctl):\n"
        f"    assert agterm_conformance.__file__.startswith({str(SCRIPT.parents[1])!r})\n"
    )
    completed, _ = run(tmp_path, "--bin", str(install), "--tests", str(layers), "--", "-k", "where", PYTHONPATH="")
    assert completed.returncode == 0, completed.stdout + completed.stderr
