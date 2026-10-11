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

LEAKING_TEST = """
import tempfile

def test_leaks_a_root(ctl):
    tempfile.mkdtemp(prefix="agt-", dir="/tmp")
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


def run(tmp_path: Path, *args: str, **env: str) -> tuple[subprocess.CompletedProcess[str], str]:
    out = tmp_path / "report"
    completed = subprocess.run(
        [str(SCRIPT), "--out", str(out), *args],
        env={**os.environ, "PYTHON": sys.executable, **env},
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    report = out / "report.md"
    return completed, report.read_text() if report.exists() else ""


def test_a_green_run_writes_the_report(tmp_path: Path, install: Path, layers: Path) -> None:
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


def test_a_failing_test_fails_the_run(tmp_path: Path, install: Path, layers: Path) -> None:
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers), FAKE_VERSION='{"app": {}}')
    assert completed.returncode == 1
    assert "swift-linux: 0 pass, 1 fail" in report
    assert "FAILED" in report


def test_a_root_left_behind_fails_a_green_run(tmp_path: Path, install: Path, layers: Path) -> None:
    (layers / "protocol" / "test_probe.py").write_text(LEAKING_TEST)
    completed, report = run(tmp_path, "--bin", str(install), "--tests", str(layers))
    left = re.findall(r"`(/tmp/agt-[^`]+)`", report)
    for root in left:
        shutil.rmtree(root, ignore_errors=True)
    assert completed.returncode == 1
    assert "swift-linux: 1 pass, 0 fail" in report
    assert len(left) == 1


@pytest.mark.parametrize("case", ["missing bin", "no layer"])
def test_bad_arguments_exit_2(tmp_path: Path, install: Path, case: str) -> None:
    args = ["--bin", str(tmp_path / "absent")] if case == "missing bin" else ["--bin", str(install), "--tests", "/"]
    completed, _ = run(tmp_path, *args)
    assert completed.returncode == 2
