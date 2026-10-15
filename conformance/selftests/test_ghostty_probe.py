import importlib.util
import json
import os
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

CREATED = "2026-10-13T00:00:00Z DEBUG agterm::surface: terminal surface created key=1 scale=1.0"
DREW = "2026-10-13T00:00:00Z DEBUG agterm::surface: terminal drew its first frame key=1"
NO_GL = "2026-10-13T00:00:00Z ERROR agterm::surface: the terminal's GtkGLArea has no OpenGL context err=no backend"


def test_a_drawn_frame_is_seen() -> None:
    assert ghostty_probe.read_log(f"{CREATED}\n{DREW}\n") == {"created": True, "drew": True, "error": None}


def test_a_serving_app_without_a_gl_context_is_a_failure() -> None:
    # The case `version` misses: the app logs the GL error and keeps answering on its socket.
    seen = ghostty_probe.read_log(f"INFO agterm: serving\n{NO_GL}\n")
    assert seen == {"created": False, "drew": False, "error": NO_GL}


def test_colour_codes_are_dropped_from_the_log() -> None:
    # tracing's writer colours a file too; the message itself is never split by a code
    coloured = (
        "\x1b[2m2026-10-13T00:00:00Z\x1b[0m \x1b[31mERROR\x1b[0m \x1b[2magterm::surface\x1b[0m\x1b[2m:\x1b[0m "
        "the terminal's GtkGLArea has no OpenGL context \x1b[3merr\x1b[0m\x1b[2m=\x1b[0mOpenGL support disabled"
    )
    seen = ghostty_probe.read_log(coloured)
    assert seen["error"] == (
        "2026-10-13T00:00:00Z ERROR agterm::surface: the terminal's GtkGLArea has no OpenGL context "
        "err=OpenGL support disabled"
    )


def test_a_failed_surface_creation_is_a_failure() -> None:
    line = "ERROR agterm::surface: could not create the terminal surface err=boom"
    assert ghostty_probe.read_log(line)["error"] == line


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

    def fake_probe(bin: Path, timeout: float, args=(), frontend: str = "gtk") -> dict:
        seen.update(bin=bin, timeout=timeout, args=list(args), frontend=frontend)
        return {"ok": True}

    monkeypatch.setattr(ghostty_probe, "probe", fake_probe)
    return seen


def test_extra_arguments_reach_the_probe_in_order(monkeypatch, tmp_path: Path) -> None:
    seen = recording_probe(monkeypatch)
    status = ghostty_probe.main(["--bin", str(tmp_path / "agterm"), "--arg=--no-such-option", "--arg", "file.txt"])
    assert status == 0
    assert seen["args"] == ["--no-such-option", "file.txt"]
    assert seen["frontend"] == "gtk"


# -- the GPUI build (Task 13 of the GPUI plan logs these lines under the explicit target `agterm::surface`)

GPUI_CREATED = "2026-10-15T00:00:00.000000Z DEBUG agterm::surface: terminal surface created key=1 scale=2"
GPUI_DREW = "2026-10-15T00:00:00.016000Z DEBUG agterm::surface: terminal drew its first frame key=1"


def test_the_gpui_builds_first_frame_is_seen() -> None:
    log = f"INFO agterm_gpui::app: serving\n{GPUI_CREATED}\n{GPUI_DREW}\n"
    assert ghostty_probe.read_log(log) == {"created": True, "drew": True, "error": None}


def test_the_gpui_builds_coloured_first_frame_is_seen() -> None:
    coloured = (
        "\x1b[2m2026-10-15T00:00:00Z\x1b[0m \x1b[34mDEBUG\x1b[0m \x1b[2magterm::surface\x1b[0m\x1b[2m:\x1b[0m "
        "terminal drew its first frame \x1b[3mkey\x1b[0m\x1b[2m=\x1b[0m1"
    )
    assert ghostty_probe.read_log(coloured)["drew"] is True


def test_a_gpui_surface_that_cannot_be_created_is_a_failure() -> None:
    line = "2026-10-15T00:00:00Z ERROR agterm::surface: could not create the terminal surface key=1 err=pty: boom"
    assert ghostty_probe.read_log(f"{GPUI_CREATED}\n{line}\n")["error"] == line


def test_the_gpui_frontend_defaults_to_its_own_stage(monkeypatch) -> None:
    seen = recording_probe(monkeypatch)
    assert ghostty_probe.main(["--frontend", "gpui"]) == 0
    assert seen["frontend"] == "gpui"
    assert seen["bin"] == (ghostty_probe.REPO / "target/conformance-stage-gpui/bin/agterm-gpui").resolve()
    seen.clear()
    assert ghostty_probe.main([]) == 0
    assert seen["bin"] == (ghostty_probe.REPO / "target/conformance-stage/bin/agterm").resolve()


def test_lavapipe_comes_from_the_script_without_fetching(tmp_path: Path) -> None:
    script = tmp_path / "fetch-lavapipe.sh"
    script.write_text('#!/bin/sh\n[ "$1" = --print-icd ] || exit 9\necho /vendor/lavapipe/lvp_icd.json\n')
    script.chmod(0o755)
    assert ghostty_probe.lavapipe_from_script(script) == "/vendor/lavapipe/lvp_icd.json"
    script.write_text("#!/bin/sh\necho 'no lavapipe ICD found' >&2\nexit 1\n")
    assert ghostty_probe.lavapipe_from_script(script) is None
    assert ghostty_probe.lavapipe_from_script(tmp_path / "absent.sh") is None


def test_a_gpui_probe_without_lavapipe_fails_before_launching(
    monkeypatch: pytest.MonkeyPatch, roots: list[Path], fake_bin: Path
) -> None:
    monkeypatch.setenv(LAVAPIPE_ICD_VARIABLE, "unset by the test")
    monkeypatch.delenv(LAVAPIPE_ICD_VARIABLE)
    monkeypatch.setattr(ghostty_probe, "lavapipe_from_script", lambda: None)
    report = ghostty_probe.probe(fake_bin, 1.0, frontend="gpui")
    assert report["ok"] is False and report["frontend"] == "gpui"
    assert "no lavapipe" in report["error"] and "fetch-lavapipe.sh" in report["error"]
    assert all(not root.exists() for root in roots)


def test_a_gpui_probe_takes_the_scripts_lavapipe(
    monkeypatch: pytest.MonkeyPatch, roots: list[Path], fake_bin: Path, tmp_path: Path
) -> None:
    icd = tmp_path / "lvp_icd.json"
    icd.write_text(json.dumps({"ICD": {"library_path": "/vendor/lib/libvulkan_lvp.so"}}))
    monkeypatch.setenv(LAVAPIPE_ICD_VARIABLE, "unset by the test")
    monkeypatch.delenv(LAVAPIPE_ICD_VARIABLE)
    monkeypatch.setattr(ghostty_probe, "lavapipe_from_script", lambda: str(icd))
    report = ghostty_probe.probe(fake_bin, 0.3, frontend="gpui")
    # the fake app serves but draws nothing: launched on lavapipe, then no first frame
    assert report["ok"] is False and report["error"] == "no first frame within 0.3s"
    assert os.environ[LAVAPIPE_ICD_VARIABLE] == str(icd)
