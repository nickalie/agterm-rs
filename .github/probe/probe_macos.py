"""Temporary Task 19 probe on the macOS runner: launch reliability and Live reaping. Removed after Task 19."""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from agterm_conformance.impls.swift_macos import SwiftMacosAdapter
from agterm_conformance.match import assert_ok

bundle = Path(sys.argv[1])
out = Path(sys.argv[2])
log: list[str] = []


def note(label: str, value: object) -> None:
    line = f"## {label}\n{value if isinstance(value, str) else json.dumps(value, indent=1, default=str)}"
    print(line, flush=True)
    log.append(line)
    out.write_text("\n".join(log))


def ps() -> str:
    text = subprocess.run(
        ["ps", "-axww", "-o", "pid=,ppid=,pgid=,lstart=,command="], capture_output=True, text=True, check=False
    ).stdout
    return "\n".join(line[:400] for line in text.splitlines() if "agterm" in line or "zmx" in line)


def lsapp() -> str:
    text = subprocess.run(["lsappinfo", "list"], capture_output=True, text=True, check=False).stdout
    keep = [line for line in text.splitlines() if "agterm" in line.lower()]
    return "\n".join(keep)[:6000]


def attempt(adapter: SwiftMacosAdapter, label: str) -> bool:
    started = time.monotonic()
    try:
        instance = adapter.launch(timeout=20)
    except Exception as error:  # noqa: BLE001
        note(f"{label} FAILED after {time.monotonic() - started:.1f}s", repr(error)[:400])
        note(f"{label} ps", ps())
        note(f"{label} lsappinfo", lsapp())
        return False
    note(f"{label} ok in {time.monotonic() - started:.1f}s", instance.pid)
    adapter.stop(instance)
    return True


def activated_holder(adapter: SwiftMacosAdapter):
    holder = adapter.launch()
    wid = assert_ok(holder.request("window.list"))["windows"][0]["id"]
    for cmd, args in (("window.select", {}), ("session.new", {}), ("quick.show", {"mode": "on"})):
        note(f"holder {cmd}", holder.request(cmd, wid if cmd == "window.select" else None, **args).raw)
        time.sleep(1)
    return holder


for label, factory in (
    ("same bundle", lambda n: SwiftMacosAdapter(bundle)),
    ("plain", lambda n: SwiftMacosAdapter(bundle, launch_mode="plain")),
    ("clone", None),
):
    adapter = SwiftMacosAdapter(bundle)
    holder = activated_holder(adapter)
    clones = Path(tempfile.mkdtemp(prefix="agtb-", dir="/tmp"))
    results = []
    for n in range(4):
        if factory is None:
            copy = clones / f"c{n}" / "agterm.app"
            copy.parent.mkdir()
            subprocess.run(["cp", "-c", "-R", str(bundle), str(copy)], check=True)
            candidate = SwiftMacosAdapter(copy)
        else:
            candidate = factory(n)
        results.append(attempt(candidate, f"{label} #{n}"))
        note(f"{label} holder windows", assert_ok(holder.request("window.list"))["windows"])
    note(f"{label} results", results)
    adapter.stop(holder)
    shutil.rmtree(clones, ignore_errors=True)
    note(f"{label} after", ps())

# Live: reap by hand and watch what is left.
adapter = SwiftMacosAdapter(bundle)
live = adapter.launch(settings={"restoreMode": "live"})
entries: list = []
try:
    for _ in range(100):
        entries = assert_ok(live.request("zmx.list"))["zmx"]["entries"]
        if entries and all(entry.get("observation") == "running" for entry in entries):
            break
        time.sleep(0.2)
    note("zmx.list", entries)
    note("ps live", ps())
    for entry in entries:
        args = {"pane": entry["pane"], "force": True, "window": entry.get("windowID")}
        note("zmx.kill", live.request("zmx.kill", entry["sessionID"], **args).raw)
    time.sleep(2)
    note("ps after kill", ps())
    note("zmx.list after kill", assert_ok(live.request("zmx.list"))["zmx"]["entries"])
finally:
    try:
        adapter.stop(live)
        note("live stop", "clean")
    except Exception as error:  # noqa: BLE001
        note("live stop failed", repr(error)[:600])
note("ps after live stop", ps())
