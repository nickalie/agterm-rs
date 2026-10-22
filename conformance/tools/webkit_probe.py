#!/usr/bin/env python3
"""Open a page in the staged Rust build under a private headless weston and session bus, the isolation of the Linux
runs, and report whether WPE WebKit's web process came up: Ubuntu 24.04's AppArmor restriction on unprivileged user
namespaces can stop WebKit's bubblewrap sandbox on a CI runner, and the HTML overlays need that process.

  python conformance/tools/webkit_probe.py [--bin PATH] [--timeout SECONDS]

It launches the staged build (--bin, default run-rust-linux.sh's stage) through the harness on lavapipe, opens a page
from a file in the instance's root over a fresh session and waits for it to read back loaded with its title; the
report adds the host's sandbox facts and the app's web engine lines from its log (the frame mode, a start that failed,
a fall-back to shared memory). Prints a JSON report; exits 0 when the page loaded.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path
from typing import Any

TITLE = "ngterm-webkit-probe"
SYSCTLS = (
    "kernel/apparmor_restrict_unprivileged_userns",
    "kernel/unprivileged_userns_clone",
    "user/max_user_namespaces",
)


def host_facts() -> dict[str, Any]:
    facts: dict[str, Any] = {"bwrap": shutil.which("bwrap")}
    for name in SYSCTLS:
        try:
            facts[name.replace("/", ".")] = Path("/proc/sys", name).read_text().strip()
        except OSError:
            facts[name.replace("/", ".")] = None
    return facts


STAGE_BIN = Path(__file__).resolve().parents[2] / "target" / "conformance-stage" / "bin" / "ngterm"
# the app's own lines about its web engine (`ngterm_gpui::html_overlay`, `external_texture`), kept in the report
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


def probe(bin: Path, timeout: int) -> dict[str, Any]:
    """WPE WebKit as the staged build runs it (module docs)."""
    from ghostty_probe import LAVAPIPE_ICD_VARIABLE, lavapipe_from_script

    report: dict[str, Any] = {"engine": "wpe", "bin": str(bin), "host": host_facts(), "ok": False}
    if not os.environ.get(LAVAPIPE_ICD_VARIABLE):
        icd, problem = lavapipe_from_script()
        if icd is None:
            return {**report, "error": f"no lavapipe: {problem}"}
        os.environ[LAVAPIPE_ICD_VARIABLE] = icd
    # only now: importing the harness scrubs NGTERM_* and AGTERM_* from this process
    from agterm_conformance.impls.rust import RustAdapter
    from agterm_conformance.match import assert_ok

    if not bin.is_file():
        return {**report, "error": f"no ngterm at {bin}"}
    adapter = RustAdapter(bin)
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
    parser.add_argument("--timeout", type=int, default=60, help="seconds the page may take to answer")
    parser.add_argument("--bin", type=Path, default=STAGE_BIN, help="the staged app binary (default: the stage's)")
    args = parser.parse_args(argv)
    report = probe(args.bin.resolve(), args.timeout)
    print(json.dumps(report, indent=2))
    return 0 if report.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
