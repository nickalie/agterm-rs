import importlib.util
import sys
from pathlib import Path

from agterm_conformance.impls.base import OWN_VARIABLES, scrubbed_environ

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
