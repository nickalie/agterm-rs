#!/usr/bin/env python3
"""Open a web view under a private headless weston and session bus, the isolation of the Linux runs, and report
whether its web process came up: Ubuntu 24.04's AppArmor restriction on unprivileged user namespaces can stop
WebKit's bubblewrap sandbox on a CI runner, and the Rust build's HTML overlays need that process.

  python conformance/tools/webkit_probe.py [--timeout SECONDS] [--view-python PATH]
  python conformance/tools/webkit_probe.py --engine wpe [--bin PATH] [--timeout SECONDS]

The default engine is WebKitGTK, the GTK build's: this process starts weston and dbus-daemon through the harness; the
view runs under `--view-python` (default /usr/bin/python3), which needs PyGObject and the GTK 4.0 and WebKit 6.0
typelibs. `--engine wpe` probes WPE WebKit the way the GPUI build runs it: it launches the staged GPUI build (--bin,
default run-rust-linux.sh --frontend gpui's stage) through the harness's GPUI frontend on lavapipe, opens a page from
a file in the instance's root over a fresh session and waits for it to read back loaded with its title; the report
adds the app's web engine lines from its log (the frame mode, a start that failed, a fall-back to shared memory).
Prints a JSON report; exits 0 when the page loaded (and, for WebKitGTK, its script answered).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

TITLE = "agterm-webkit-probe"
SYSCTLS = (
    "kernel/apparmor_restrict_unprivileged_userns",
    "kernel/unprivileged_userns_clone",
    "user/max_user_namespaces",
)


def view(timeout: int) -> int:
    """The child: one window with one WebView; prints its own JSON report line."""
    import gi

    gi.require_version("Gtk", "4.0")
    gi.require_version("WebKit", "6.0")
    from gi.repository import Gio, GLib, Gtk, WebKit

    report: dict[str, Any] = {
        "webkit": f"{WebKit.get_major_version()}.{WebKit.get_minor_version()}.{WebKit.get_micro_version()}",
        "ok": False,
    }
    app = Gtk.Application(
        application_id="io.github.melonamin.agterm.webkitprobe", flags=Gio.ApplicationFlags.NON_UNIQUE
    )

    def finish(**fields: Any) -> None:
        if "done" not in report:
            report.update(fields, done=True)
            app.quit()

    def on_script(webview: WebKit.WebView, result: Gio.AsyncResult) -> None:
        try:
            title = webview.evaluate_javascript_finish(result).to_string()
        except GLib.Error as error:
            finish(error=f"script failed: {error.message}")
            return
        finish(ok=title == TITLE, title=title)

    def on_load(webview: WebKit.WebView, event: WebKit.LoadEvent) -> None:
        if event == WebKit.LoadEvent.FINISHED:
            webview.evaluate_javascript("document.title", -1, None, None, None, on_script)

    def on_activate(app: Gtk.Application) -> None:
        window = Gtk.ApplicationWindow(application=app, default_width=480, default_height=320)
        webview = WebKit.WebView()
        window.set_child(webview)
        webview.connect("load-changed", on_load)
        webview.connect("load-failed", lambda _v, _e, _uri, err: finish(error=f"load failed: {err.message}") or True)
        webview.connect(
            "web-process-terminated", lambda _v, reason: finish(error=f"web process ended: {reason.value_nick}")
        )
        webview.load_html(f"<title>{TITLE}</title><p>probe</p>", "about:blank")
        window.present()
        GLib.timeout_add_seconds(timeout, lambda: finish(error=f"no answer within {timeout}s") or False)

    app.connect("activate", on_activate)
    app.run([])
    report.pop("done", None)
    print(json.dumps(report))
    return 0 if report["ok"] else 1


def host_facts() -> dict[str, Any]:
    facts: dict[str, Any] = {"bwrap": shutil.which("bwrap")}
    for name in SYSCTLS:
        try:
            facts[name.replace("/", ".")] = Path("/proc/sys", name).read_text().strip()
        except OSError:
            facts[name.replace("/", ".")] = None
    return facts


def child_report(completed: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    lines = completed.stdout.strip().splitlines()
    try:
        report = json.loads(lines[-1]) if lines else {}
    except json.JSONDecodeError:
        report = {}
    if not isinstance(report, dict) or not report:
        report = {"ok": False, "error": f"the view printed no report (exit {completed.returncode})"}
    report["exit"] = completed.returncode
    if not report.get("ok"):
        report["stderr"] = completed.stderr.strip().splitlines()[-30:]
    return report


def probe(view_python: str, timeout: int) -> dict[str, Any]:
    from agterm_conformance.impls.base import (
        RUNTIME_DIR_NAME,
        isolated_runtime_env,
        scrubbed_environ,
        start_compositor,
        start_session_bus,
        stop_headless,
    )

    root = Path(tempfile.mkdtemp(prefix="agt-webkit-", dir="/tmp"))
    runtime = root / RUNTIME_DIR_NAME
    runtime.mkdir(mode=0o700)
    env = scrubbed_environ()
    isolated_runtime_env(env, os.fspath(runtime), os.environ, headless=True)
    env["GIO_USE_PORTALS"] = "0"
    report: dict[str, Any] = {"host": host_facts()}
    compositor = session_bus = None
    try:
        compositor = start_compositor(root, env)
        session_bus = start_session_bus(root, env)
        command = [view_python, os.path.abspath(__file__), "--view", "--timeout", str(timeout)]
        completed = subprocess.run(command, env=env, capture_output=True, text=True, timeout=timeout + 30, check=False)
        report.update(child_report(completed))
    except Exception as error:  # noqa: BLE001 - the report names whatever stopped the probe
        report.update(ok=False, error=f"{type(error).__name__}: {error}")
    finally:
        if problem := stop_headless(compositor, session_bus, root):
            report["cleanup"] = problem
        else:
            shutil.rmtree(root, ignore_errors=True)
    return report


GPUI_STAGE_BIN = Path(__file__).resolve().parents[2] / "target" / "conformance-stage-gpui" / "bin" / "agterm-gpui"
# the app's own lines about its web engine (`agterm_gpui::html_overlay`, `external_texture`), kept in the report
ENGINE_LINES = ("web engine", "shared memory", "WPE", "WebKit")


def engine_lines(log: str) -> list[str]:
    """The lines of the app's log about its web engine, without the colour codes the log carries."""
    from ghostty_probe import plain

    lines = plain(log).splitlines()
    return [line.strip() for line in lines if any(word in line for word in ENGINE_LINES)][-20:]


def page_state(tree: dict[str, Any], session: str) -> dict[str, Any] | None:
    """The first page `session` shows in `tree`, else None."""
    from agterm_conformance.match import find_session

    pages = find_session(tree, session).get("htmlOverlays") or []
    return pages[0] if pages else None


def probe_wpe(bin: Path, timeout: int) -> dict[str, Any]:
    """WPE WebKit as the staged GPUI build runs it (module docs)."""
    from ghostty_probe import LAVAPIPE_ICD_VARIABLE, lavapipe_from_script

    report: dict[str, Any] = {"engine": "wpe", "bin": str(bin), "host": host_facts(), "ok": False}
    if not os.environ.get(LAVAPIPE_ICD_VARIABLE):
        icd, problem = lavapipe_from_script()
        if icd is None:
            return {**report, "error": f"no lavapipe: {problem}"}
        os.environ[LAVAPIPE_ICD_VARIABLE] = icd
    # only now: importing the harness scrubs AGTERM_* from this process
    from agterm_conformance.impls.rust import RustAdapter
    from agterm_conformance.match import assert_ok

    if not bin.is_file():
        return {**report, "error": f"no agterm-gpui at {bin}"}
    adapter = RustAdapter(bin, frontend="gpui")
    try:
        instance = adapter.launch()
    except Exception as error:  # noqa: BLE001 - the report names whatever stopped the probe
        return {**report, "error": f"launch: {type(error).__name__}: {error}"}
    try:
        page = instance.root / "webkit-probe.html"
        page.write_text(f"<!doctype html><title>{TITLE}</title><p>probe</p>")
        session = assert_ok(instance.request("session.new"))["id"]
        assert_ok(instance.request("session.overlay.open", session, html=str(page)))
        deadline = time.monotonic() + timeout
        state = None
        while time.monotonic() < deadline:
            state = page_state(assert_ok(instance.request("tree")), session)
            if state and state.get("state") in ("loaded", "failed"):
                break
            time.sleep(0.2)
        report["page"] = state
        loaded = bool(state) and state.get("state") == "loaded" and state.get("title") == TITLE
        report["ok"] = loaded
        if not loaded:
            report["error"] = (state or {}).get("error") or f"the page did not load within {timeout}s"
    except Exception as error:  # noqa: BLE001 - the report names whatever stopped the probe
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        report["log"] = engine_lines(instance.log.read_text(errors="replace"))
        try:
            adapter.stop(instance)
        except Exception as error:  # noqa: BLE001 - a failed stop fails the probe too
            report.update(ok=False, cleanup=f"{type(error).__name__}: {error}")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--engine", choices=("webkitgtk", "wpe"), default="webkitgtk", help="the web engine to probe")
    parser.add_argument("--timeout", type=int, default=60, help="seconds the page may take to answer")
    parser.add_argument("--view-python", default="/usr/bin/python3", help="a python with PyGObject and WebKit 6.0")
    parser.add_argument("--bin", type=Path, default=GPUI_STAGE_BIN, help="the staged GPUI build (--engine wpe)")
    parser.add_argument("--view", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.view:
        return view(args.timeout)
    if args.engine == "wpe":
        report = probe_wpe(args.bin.resolve(), args.timeout)
    else:
        report = probe(args.view_python, args.timeout)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
