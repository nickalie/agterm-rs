#!/usr/bin/env python3
"""Open a WebKitGTK 6 view under a private headless weston and session bus, the isolation of the Linux runs, and
report whether its web process came up: Ubuntu 24.04's AppArmor restriction on unprivileged user namespaces can stop
WebKit's bubblewrap sandbox on a CI runner, and the Rust build's HTML overlays need that process.

  python conformance/tools/webkit_probe.py [--timeout SECONDS] [--view-python PATH]

This process starts weston and dbus-daemon through the harness; the view runs under `--view-python` (default
/usr/bin/python3), which needs PyGObject and the GTK 4.0 and WebKit 6.0 typelibs. Prints a JSON report; exits 0 when
the page loaded and its script answered.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
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


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--timeout", type=int, default=60, help="seconds the page may take to answer")
    parser.add_argument("--view-python", default="/usr/bin/python3", help="a python with PyGObject and WebKit 6.0")
    parser.add_argument("--view", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.view:
        return view(args.timeout)
    report = probe(args.view_python, args.timeout)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
