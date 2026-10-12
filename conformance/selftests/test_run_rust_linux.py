import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "tools" / "run-rust-linux.sh"
# What a Rust build's `version` answers; without the conformance block every test skips as not-yet-at-level.
AT_LEVEL = json.dumps({"app": {"version": "0.1.0"}, "conformance": {"parityLevel": "0.36.0", "capabilities": []}})
PROBE_ID = "tests/protocol/test_probe.py::test_reports_version"
OTHER_ID = "tests/protocol/test_other.py::test_other"

VERSION_TEST = """
from agterm_conformance.match import assert_ok

def test_reports_version(ctl):
    assert assert_ok(ctl.request("version"))["app"]["version"]
"""

OTHER_TEST = """
def test_other(ctl):
    pass
"""

FAILING_TEST = """
def test_other(ctl):
    assert False, "red by design"
"""

LEAKING_TEST = """
from agterm_conformance.impls.base import make_root

def test_reports_version(ctl):
    make_root()
"""


@pytest.fixture
def layers(tmp_path: Path) -> Path:
    tests = tmp_path / "tests"
    (tests / "protocol").mkdir(parents=True)
    (tests / "protocol" / "test_probe.py").write_text(VERSION_TEST)
    return tests


@pytest.fixture
def stage(tmp_path: Path, fake_bin: Path) -> Path:
    """A staged tree whose bin/agterm is the fake app: no Rust binary needs to exist."""
    app = tmp_path / "stage" / "bin" / "agterm"
    app.parent.mkdir(parents=True)
    shutil.copy2(fake_bin, app)
    return app.parent.parent


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
    monkeypatch.delenv("GITHUB_STEP_SUMMARY", raising=False)
    return directory


def ratchet(tmp_path: Path, *ids: str) -> Path:
    path = tmp_path / "rust-passing.txt"
    path.write_text("# ids the build passes\n" + "".join(f"{nodeid}\n" for nodeid in sorted(ids)))
    return path


def run(
    tmp_path: Path, stage: Path, layers: Path, listed: Path, *args: str, **env: str
) -> tuple[subprocess.CompletedProcess[str], str]:
    out = tmp_path / "report"
    completed = subprocess.run(
        [
            str(SCRIPT),
            "--stage",
            str(stage),
            "--tests",
            str(layers),
            "--ratchet",
            str(listed),
            "--out",
            str(out),
            *args,
        ],
        env={**os.environ, "PYTHON": sys.executable, "FAKE_VERSION": AT_LEVEL, **env},
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    report = out / "report.md"
    return completed, report.read_text() if report.exists() else ""


def test_a_holding_run_writes_the_report(tmp_path: Path, stage: Path, layers: Path) -> None:
    (tmp_path / "report").mkdir()
    (tmp_path / "report" / "keep.txt").write_text("mine")
    (tmp_path / "report" / "report.md").write_text("stale")
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path, PROBE_ID), AGTERM_SOCKET="/nowhere")
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert f"agterm: `{stage}/bin/agterm`" in report
    assert "layers: protocol" in report
    assert "display: headless" in report
    assert "rust: 1 pass, 0 fail" in report
    assert "### ratchet" in report
    assert "ratchet holds" in report and "not collected" not in completed.stdout
    assert "Left behind" not in report
    out = tmp_path / "report"
    assert (out / "junit.xml").is_file()
    records = [json.loads(line) for line in (out / "nodeids.jsonl").read_text().splitlines()]
    assert {"kind": "test", "nodeid": PROBE_ID, "outcome": "passed"}.items() <= records[1].items()
    assert "scrubbed from env" not in (out / "pytest.log").read_text()
    assert (out / "keep.txt").read_text() == "mine"


def test_a_red_unlisted_test_does_not_fail_the_run(tmp_path: Path, stage: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(FAILING_TEST)
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path, PROBE_ID))
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "rust: 1 pass, 1 fail" in report
    assert "pytest status: 1" in report
    assert "exit status: 0" in report


def test_a_red_listed_test_breaks_the_ratchet(tmp_path: Path, stage: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(FAILING_TEST)
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path, OTHER_ID, PROBE_ID))
    assert completed.returncode == 1
    assert OTHER_ID in report
    assert "exit status: 1" in report


def test_a_newly_passing_id_is_printed(tmp_path: Path, stage: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(OTHER_TEST)
    listed = ratchet(tmp_path, PROBE_ID)
    completed, report = run(tmp_path, stage, layers, listed)
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert re.search(rf"^{re.escape(OTHER_ID)}$", completed.stdout, re.MULTILINE)
    assert OTHER_ID in report
    assert OTHER_ID not in listed.read_text()


def test_without_an_agterm_binary_every_test_skips(tmp_path: Path, layers: Path) -> None:
    """Before the app crate exists: a stage without bin/agterm runs, skips everything and holds the empty ratchet."""
    empty = tmp_path / "stage"
    (empty / "bin").mkdir(parents=True)
    completed, report = run(tmp_path, empty, layers, ratchet(tmp_path))
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "agterm: not staged" in report
    assert "rust: 0 pass, 0 fail, 0 skip(unsupported), 1 skip(not-yet-at-level)" in report


def test_a_root_left_behind_fails_a_holding_run(tmp_path: Path, stage: Path, layers: Path) -> None:
    (layers / "protocol" / "test_probe.py").write_text(LEAKING_TEST)
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path, PROBE_ID))
    left = [Path(root) for root in re.findall(r"`(/tmp/agt-[^`]+)`", report)]
    ours = [root for root in left if root.parent.name.startswith("agt-run-")]
    for root in ours:
        shutil.rmtree(root, ignore_errors=True)
        root.parent.rmdir()
    assert completed.returncode == 1
    assert "rust: 1 pass, 0 fail" in report
    assert len(ours) == len(left) == 1


@pytest.mark.parametrize("case", ["missing stage", "no layer", "missing ratchet"])
def test_bad_arguments_exit_2(tmp_path: Path, stage: Path, layers: Path, case: str) -> None:
    listed = ratchet(tmp_path)
    if case == "missing stage":
        stage = tmp_path / "absent"
    elif case == "no layer":
        layers = Path("/")
    else:
        listed = tmp_path / "absent.txt"
    completed, report = run(tmp_path, stage, layers, listed)
    assert completed.returncode == 2
    assert report == ""


@pytest.mark.parametrize("missing", ["weston", "dbus-daemon"])
def test_a_headless_run_without_weston_or_a_bus_names_the_opt_out(
    tmp_path: Path,
    stage: Path,
    layers: Path,
    weston_on_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    missing: str,
) -> None:
    bare = tmp_path / "bare-bin"
    bare.mkdir()
    for tool in ("bash", "dirname", "readlink", "cat", "git", "grep", "mktemp", "rmdir", "rm", "mkdir", "env", "sed"):
        if found := shutil.which(tool):
            (bare / tool).symlink_to(found)
    for helper in {"weston", "dbus-daemon"} - {missing}:
        (bare / helper).symlink_to(weston_on_path / helper)
    monkeypatch.setenv("PATH", str(bare))
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path))
    assert completed.returncode == 2
    assert f"no {missing} on PATH" in completed.stderr and "CONFORMANCE_LINUX_DISPLAY=session" in completed.stderr
    assert report == ""


def test_pytest_args_reach_pytest(tmp_path: Path, stage: Path, layers: Path) -> None:
    (layers / "protocol" / "test_other.py").write_text(FAILING_TEST)
    completed, report = run(tmp_path, stage, layers, ratchet(tmp_path, PROBE_ID), "--", "-k", "version")
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "pytest args: -k version" in report
    assert "rust: 1 pass, 0 fail" in report
