import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
from agterm_conformance.impls.base import LAVAPIPE_ICD_VARIABLE, OWN_VARIABLES, scrubbed_environ

spec = importlib.util.spec_from_file_location(
    "ghostty_probe", Path(__file__).resolve().parent.parent / "tools" / "ghostty_probe.py"
)
assert spec is not None and spec.loader is not None
ghostty_probe = importlib.util.module_from_spec(spec)
sys.modules["ghostty_probe"] = ghostty_probe
spec.loader.exec_module(ghostty_probe)

CREATED = "2026-10-15T00:00:00.000000Z DEBUG agterm::surface: terminal surface created key=1 scale=2"
DREW = "2026-10-15T00:00:00.016000Z DEBUG agterm::surface: terminal drew its first frame key=1"


def test_a_drawn_frame_is_seen() -> None:
    log = f"INFO agterm_gpui::app: serving\n{CREATED}\n{DREW}\n"
    assert ghostty_probe.read_log(log) == {"created": True, "drew": True, "error": None}


def test_a_serving_app_whose_surface_cannot_be_created_is_a_failure() -> None:
    # The case `version` misses: the app logs the failure and keeps answering on its socket.
    line = "2026-10-15T00:00:00Z ERROR agterm::surface: could not create the terminal surface key=1 err=pty: boom"
    assert ghostty_probe.read_log(f"INFO agterm_gpui::app: serving\n{CREATED}\n{line}\n") == {
        "created": True,
        "drew": False,
        "error": line,
    }


def test_colour_codes_are_dropped_from_the_log() -> None:
    # tracing's writer colours a file too; the message itself is never split by a code
    coloured = (
        "\x1b[2m2026-10-15T00:00:00Z\x1b[0m \x1b[31mERROR\x1b[0m \x1b[2magterm::surface\x1b[0m\x1b[2m:\x1b[0m "
        "could not create the terminal surface \x1b[3merr\x1b[0m\x1b[2m=\x1b[0mboom"
    )
    seen = ghostty_probe.read_log(coloured)
    assert seen["error"] == "2026-10-15T00:00:00Z ERROR agterm::surface: could not create the terminal surface err=boom"
    coloured = (
        "\x1b[2m2026-10-15T00:00:00Z\x1b[0m \x1b[34mDEBUG\x1b[0m \x1b[2magterm::surface\x1b[0m\x1b[2m:\x1b[0m "
        "terminal drew its first frame \x1b[3mkey\x1b[0m\x1b[2m=\x1b[0m1"
    )
    assert ghostty_probe.read_log(coloured)["drew"] is True


def test_an_empty_log_is_neither_drawn_nor_failed() -> None:
    assert ghostty_probe.read_log("") == {"created": False, "drew": False, "error": None}


def test_the_log_filter_is_harness_owned_but_never_inherited() -> None:
    assert "AGTERM_LOG" in OWN_VARIABLES
    assert "AGTERM_LOG" not in scrubbed_environ({"AGTERM_LOG": "trace", "PATH": "/bin"})


def test_a_missing_binary_fails_without_launching(tmp_path: Path) -> None:
    report = ghostty_probe.probe(tmp_path / "agterm", 1.0)
    assert report["ok"] is False and "no agterm at" in report["error"]


def recording_probe(monkeypatch: pytest.MonkeyPatch) -> dict:
    seen: dict = {}

    def fake_probe(bin: Path, timeout: float, args=()) -> dict:
        seen.update(bin=bin, timeout=timeout, args=list(args))
        return {"ok": True}

    monkeypatch.setattr(ghostty_probe, "probe", fake_probe)
    return seen


def test_extra_arguments_reach_the_probe_in_order(monkeypatch, tmp_path: Path) -> None:
    seen = recording_probe(monkeypatch)
    status = ghostty_probe.main(["--bin", str(tmp_path / "agterm"), "--arg=--no-such-option", "--arg", "file.txt"])
    assert status == 0
    assert seen["args"] == ["--no-such-option", "file.txt"]


def test_the_probe_defaults_to_the_runners_stage(monkeypatch) -> None:
    seen = recording_probe(monkeypatch)
    assert ghostty_probe.main([]) == 0
    assert seen["bin"] == (ghostty_probe.REPO / "target/conformance-stage/bin/agterm").resolve()


def test_the_probe_spells_the_harness_variable() -> None:
    assert ghostty_probe.LAVAPIPE_ICD_VARIABLE == LAVAPIPE_ICD_VARIABLE


def test_lavapipe_comes_from_the_script_without_fetching(tmp_path: Path) -> None:
    script = tmp_path / "fetch-lavapipe.sh"
    script.write_text('#!/bin/sh\n[ "$1" = --print-icd ] || exit 9\necho /vendor/lavapipe/lvp_icd.json\n')
    script.chmod(0o755)
    assert ghostty_probe.lavapipe_from_script(script) == ("/vendor/lavapipe/lvp_icd.json", "")


def test_the_scripts_reason_is_kept(tmp_path: Path) -> None:
    script = tmp_path / "fetch-lavapipe.sh"
    script.write_text("#!/bin/sh\necho 'no lavapipe ICD found: install mesa-vulkan-drivers' >&2\nexit 1\n")
    script.chmod(0o755)
    assert ghostty_probe.lavapipe_from_script(script) == (None, "no lavapipe ICD found: install mesa-vulkan-drivers")
    icd, reason = ghostty_probe.lavapipe_from_script(tmp_path / "absent.sh")
    assert icd is None and "absent.sh" in reason


def test_a_probe_without_lavapipe_fails_naming_why(
    monkeypatch: pytest.MonkeyPatch, roots: list[Path], fake_bin: Path
) -> None:
    monkeypatch.setenv(LAVAPIPE_ICD_VARIABLE, "unset by the test")
    monkeypatch.delenv(LAVAPIPE_ICD_VARIABLE)
    monkeypatch.setattr(ghostty_probe, "lavapipe_from_script", lambda: (None, "no lavapipe ICD found: because"))
    report = ghostty_probe.probe(fake_bin, 1.0)
    assert report["ok"] is False and "frontend" not in report
    assert report["lavapipe"] == "no lavapipe ICD found: because"
    assert "no lavapipe" in report["error"]
    assert report["error"].endswith("(fetch-lavapipe.sh: no lavapipe ICD found: because)")
    assert all(not root.exists() for root in roots)


def test_a_probe_takes_the_scripts_lavapipe(
    monkeypatch: pytest.MonkeyPatch, roots: list[Path], fake_bin: Path, make_icd
) -> None:
    icd = make_icd()
    monkeypatch.setenv(LAVAPIPE_ICD_VARIABLE, "unset by the test")
    monkeypatch.delenv(LAVAPIPE_ICD_VARIABLE)
    monkeypatch.setattr(ghostty_probe, "lavapipe_from_script", lambda: (str(icd), ""))
    report = ghostty_probe.probe(fake_bin, 0.3)
    # the fake app serves but draws nothing: launched on lavapipe, then no first frame
    assert report["ok"] is False and report["error"] == "no first frame within 0.3s" and "lavapipe" not in report
    assert os.environ[LAVAPIPE_ICD_VARIABLE] == str(icd)


def test_the_cli_finds_lavapipe_before_the_harness_scrubs_agterm_variables(
    tmp_path: Path, fake_bin: Path, fake_compositor_bin: Path, fake_session_bus_bin: Path, make_icd
) -> None:
    """Run as a program: AGTERM_LAVAPIPE_ICD reaches scripts/fetch-lavapipe.sh, though the harness's import drops it."""
    helpers = tmp_path / "helpers"
    helpers.mkdir()
    (helpers / "weston").symlink_to(fake_compositor_bin)
    (helpers / "dbus-daemon").symlink_to(fake_session_bus_bin)
    env = {name: value for name, value in os.environ.items() if name != LAVAPIPE_ICD_VARIABLE}
    env.update(
        AGTERM_LAVAPIPE_ICD=str(make_icd()),
        # no vendored lavapipe to fall back on, should the override be lost
        AGTERM_VENDOR=str(tmp_path / "vendor"),
        HOME=str(tmp_path / "home"),
        PATH=f"{helpers}{os.pathsep}{os.environ['PATH']}",
        PYTHONPATH=str(Path(__file__).resolve().parents[1]),
        CONFORMANCE_TMP=str(tmp_path),
    )
    env.pop("CONFORMANCE_LINUX_DISPLAY", None)
    completed = subprocess.run(
        [sys.executable, str(ghostty_probe.__file__), "--bin", str(fake_bin), "--timeout", "0.5"],
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    report = json.loads(completed.stdout)
    assert report["error"] == "no first frame within 0.5s", report
    assert "lavapipe" not in report and "cleanup" not in report


