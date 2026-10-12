#!/usr/bin/env python3
"""Spike B: drive reference-macos through its accessibility tree with the ax driver.

Reads `AXIdentifier` `add-session`, AXPresses it and its `New Session` menu item (a SwiftUI Menu opens a menu
rather than acting), checks the new session over the socket, then posts synthesized keystrokes (CGEvent) and
reads them back with `session.text`. Each keystroke route is tried in turn and reported: posted to the pid, the
same after AXFrontmost, then through the HID tap. Prints a JSON report; exits 0 when the identifier was read, the
press made a session and some route's keystrokes reached the pane.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

from agterm_conformance.drivers.ax import AX_SUCCESS, AxDriver, trust
from agterm_conformance.drivers.base import DriverError, ElementNotFound
from agterm_conformance.impls.base import Instance, LaunchError, StopError
from agterm_conformance.impls.swift_macos import LAUNCH_MODES, SwiftMacosAdapter
from agterm_conformance.match import assert_ok, session_ids
from agterm_conformance.wait import WaitTimeout, wait_mounted, wait_text, wait_tree

# Letters only: one keystroke each, no layout-dependent shifted characters.
ROUTES = (("pid", False, "zqxjpid"), ("pid", True, "zqxjact"), ("hid", True, "zqxjhid"))


def ancestry() -> list[str]:
    """This process and its parents, `pid comm`: TCC answers for the responsible one among them."""
    out = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,comm="], capture_output=True, text=True, check=False).stdout
    table = {}
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3:
            table[int(parts[0])] = (int(parts[1]), parts[2])
    chain, pid = [], os.getpid()
    while pid in table and pid not in (0, 1) and len(chain) < 32:
        ppid, comm = table[pid]
        chain.append(f"{pid} {comm}")
        pid = ppid
    return chain


def press_new_session(driver: AxDriver, instance: Instance, report: dict[str, Any], timeout: float) -> str | None:
    before = set(session_ids(assert_ok(instance.request("tree"))))
    add = driver.find("add-session", timeout=timeout)
    report["addSession"] = {
        "uiId": add.ui_id,
        "role": add.role,
        "axRole": driver.attribute(add.handle, "AXRole"),
        "name": add.name,
        "actions": driver.actions(add),
    }
    report["addSessionPress"] = driver.perform(add, "AXPress")
    item = driver.menu_item("New Session", timeout=timeout)
    report["menuItem"] = {"role": item.role, "name": item.name, "enabled": item.enabled}
    report["menuItemPress"] = driver.perform(item, "AXPress")
    tree = wait_tree(instance, lambda t: set(session_ids(t)) - before, timeout)
    (created,) = set(session_ids(tree)) - before
    wait_mounted(instance, created, "left", timeout=timeout)
    return created


def keystrokes(driver: AxDriver, instance: Instance, session: str, timeout: float) -> list[dict[str, Any]]:
    attempts = []
    for post, activate, marker in ROUTES:
        attempt: dict[str, Any] = {"post": post, "activate": activate}
        if activate:
            driver.activate()
        driver.post = post
        driver.press(*marker)
        try:
            wait_text(instance, session, marker, timeout)
            attempt["landed"] = True
        except WaitTimeout as error:
            attempt.update(landed=False, error=str(error)[-400:])
        attempts.append(attempt)
    return attempts


def run(adapter: SwiftMacosAdapter, timeout: float) -> dict[str, Any]:
    report: dict[str, Any] = {"ancestry": ancestry()}
    try:
        report["trust"] = trust()
    except DriverError as error:
        report["driverError"] = str(error)
        return report
    try:
        instance = adapter.launch()
    except LaunchError as error:
        report["launchError"] = str(error)
        return report
    report.update(pid=instance.pid, version=instance.version)
    try:
        driver = AxDriver(instance.pid, timeout=timeout)
        session = press_new_session(driver, instance, report, timeout)
        report["newSession"] = session
        report["keystrokes"] = keystrokes(driver, instance, session, timeout)
    except (AssertionError, DriverError, ElementNotFound) as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        report["logTail"] = instance.log_tail(40)
        try:
            adapter.stop(instance)
        except StopError as error:
            report["stopError"] = str(error)
    return report


def passed(report: dict[str, Any]) -> bool:
    add = report.get("addSession") or {}
    return (
        add.get("uiId") == "add-session"
        and report.get("menuItemPress") == AX_SUCCESS
        and bool(report.get("newSession"))
        and any(attempt.get("landed") for attempt in report.get("keystrokes", ()))
        and "stopError" not in report
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--launch-mode", choices=LAUNCH_MODES, default="uitest")
    parser.add_argument("--timeout", type=float, default=15.0)
    parser.add_argument("--out", type=Path, help="also write the JSON report here")
    args = parser.parse_args(argv)
    report = run(SwiftMacosAdapter(args.bin, launch_mode=args.launch_mode), args.timeout)
    text = json.dumps(report, indent=2, sort_keys=True, default=repr)
    print(text)
    if args.out is not None:
        args.out.write_text(text + "\n")
    return 0 if passed(report) else 1


if __name__ == "__main__":
    sys.exit(main())
