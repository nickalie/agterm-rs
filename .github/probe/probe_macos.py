"""Temporary Task 19 probe on the macOS runner: process trees, fullscreen timing, capture. Removed after Task 19."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

from agterm_conformance.impls.swift_macos import SwiftMacosAdapter
from agterm_conformance.match import assert_ok, find_session
from agterm_conformance.wait import wait_tree

bundle = Path(sys.argv[1])
mode = sys.argv[2]
out = Path(sys.argv[3])
log: list[str] = []


def note(label: str, value: object) -> None:
    line = f"## {label}\n{value if isinstance(value, str) else json.dumps(value, indent=1, default=str)}"
    print(line, flush=True)
    log.append(line)


def ps() -> str:
    return subprocess.run(
        ["ps", "-E", "-ww", "-o", "pid=,ppid=,pgid=,tpgid=,uid=,command=", "-u", str(os.getuid())],
        capture_output=True,
        text=True,
        check=False,
    ).stdout


def relevant(text: str) -> str:
    keys = ("agterm", "zmx", "sleep", "cat", "login", "/bin/sh", "zsh")
    return "\n".join(line[:600] for line in text.splitlines() if any(key in line for key in keys))


adapter = SwiftMacosAdapter(bundle, launch_mode=mode)
note("profile", {"caps": sorted(adapter.capabilities), "absent": dict(adapter.profile.absent)})

# 1. two instances at once
first = adapter.launch()
note("first", {"pid": first.pid})
try:
    second = adapter.launch()
    note("second", {"pid": second.pid, "ok": True})
    adapter.stop(second)
except Exception as error:  # noqa: BLE001
    note("second failed", repr(error))
adapter.stop(first)

# 2. fullscreen timing
inst = adapter.launch()
try:
    wid = assert_ok(inst.request("window.list"))["windows"][0]["id"]
    for delay in (0.0, 0.5, 1.5, 3.0):
        assert_ok(inst.request("window.fullscreen", wid))
        reads = []
        start = time.monotonic()
        while time.monotonic() - start < 6:
            node = assert_ok(inst.request("window.list"))["windows"][0]
            reads.append((round(time.monotonic() - start, 2), node["fullscreen"], node["zoomed"], node["geometry"]))
            if node["fullscreen"] and time.monotonic() - start >= delay:
                break
            time.sleep(0.1)
        assert_ok(inst.request("window.fullscreen", wid))
        start = time.monotonic()
        while time.monotonic() - start < 8:
            node = assert_ok(inst.request("window.list"))["windows"][0]
            reads.append(("off", round(time.monotonic() - start, 2), node["fullscreen"], node["geometry"]))
            if not node["fullscreen"]:
                break
            time.sleep(0.2)
        note(f"fullscreen delay {delay}", reads[-6:])
        if node["fullscreen"]:
            assert_ok(inst.request("window.fullscreen", wid))
            time.sleep(3)
            note("fullscreen recovery", assert_ok(inst.request("window.list"))["windows"][0]["fullscreen"])

    # 3. capture with exec vs job
    assert_ok(inst.request("restore.mode", mode="rerun"))
    sids = []
    for text in ("exec sleep 601\n", "sleep 602\n", "cat\n"):
        sid = assert_ok(inst.request("session.new"))["id"]
        wait_tree(inst, lambda tree, sid=sid: find_session(tree, sid)["realized"])
        time.sleep(1)
        assert_ok(inst.request("session.type", sid, text=text))
        sids.append(sid)
    time.sleep(2)
    tree = assert_ok(inst.request("tree"))
    note("foregrounds", [find_session(tree, sid).get("foreground") for sid in sids])
    note("capture", assert_ok(inst.request("restore.capture")))
    note("ps capture", relevant(ps()))
    assert_ok(inst.request("restore.mode", mode="none"))
finally:
    adapter.stop(inst)
note("ps after stop", relevant(ps()))

# 4. Live instance: what runs, and what stop leaves
live = adapter.launch(settings={"restoreMode": "live"})
try:
    tree = assert_ok(live.request("tree"))
    note("restore", assert_ok(live.request("restore.mode")))
    time.sleep(3)
    note("zmx.list", assert_ok(live.request("zmx.list")))
    note("ps live", relevant(ps()))
    note("zmx_dir", live.zmx_dir)
finally:
    try:
        adapter.stop(live)
        note("live stop", "clean")
    except Exception as error:  # noqa: BLE001
        note("live stop failed", repr(error))
note("ps after live stop", relevant(ps()))
out.write_text("\n".join(log))
