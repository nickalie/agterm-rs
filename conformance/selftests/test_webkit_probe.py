import importlib.util
import sys
from pathlib import Path

import pytest

TOOLS = Path(__file__).resolve().parent.parent / "tools"


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


# the WPE route reads the GPUI build's launch and log helpers from its sibling
load("ghostty_probe")
webkit_probe = load("webkit_probe")


def test_the_engine_lines_of_the_apps_log_are_kept_without_colour() -> None:
    log = (
        "\x1b[2m2026-10-17T00:00:00Z\x1b[0m \x1b[32m INFO\x1b[0m agterm_gpui::html_overlay::registry: the web engine started\n"
        "INFO agterm_gpui::app: quit: the state is saved\n"
        "WARN agterm_gpui::external_texture: external frames fall back to shared memory: no render node\n"
    )
    lines = webkit_probe.engine_lines(log)
    assert lines == [
        "2026-10-17T00:00:00Z  INFO agterm_gpui::html_overlay::registry: the web engine started",
        "WARN agterm_gpui::external_texture: external frames fall back to shared memory: no render node",
    ]


def test_the_page_read_back_is_the_sessions_first() -> None:
    tree = {
        "tree": {
            "workspaces": [
                {
                    "id": "W",
                    "sessions": [
                        {"id": "A", "htmlOverlays": [{"state": "loaded", "title": "t"}]},
                        {"id": "B"},
                    ],
                }
            ]
        }
    }
    assert webkit_probe.page_state(tree, "A") == {"state": "loaded", "title": "t"}
    assert webkit_probe.page_state(tree, "B") is None


def test_a_missing_gpui_build_fails_without_launching(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("CONFORMANCE_LAVAPIPE_ICD", str(tmp_path / "lvp_icd.json"))
    report = webkit_probe.probe_wpe(tmp_path / "agterm-gpui", 1)
    assert report["ok"] is False and "no agterm-gpui at" in report["error"]
    assert report["engine"] == "wpe"


def test_the_engine_flag_picks_the_route(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    seen: list[str] = []
    monkeypatch.setattr(webkit_probe, "probe_wpe", lambda bin, timeout: seen.append("wpe") or {"ok": True})
    monkeypatch.setattr(webkit_probe, "probe", lambda python, timeout: seen.append("gtk") or {"ok": False})
    assert webkit_probe.main(["--engine", "wpe", "--bin", str(tmp_path / "agterm-gpui")]) == 0
    assert webkit_probe.main([]) == 1
    assert seen == ["wpe", "gtk"], "WebKitGTK stays the default"
