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
