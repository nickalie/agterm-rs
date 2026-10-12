"""Temporary Task 19 probe: pane overlay exit statuses on macOS."""

import sys
import time
from pathlib import Path

from agterm_conformance.impls.swift_macos import SwiftMacosAdapter
from agterm_conformance.match import assert_ok, find_session

adapter = SwiftMacosAdapter(Path(sys.argv[1]))
inst = adapter.launch()
try:
    for pane, command, prior in (
        ("right", "exit 5", None),
        ("left", "exit 6", None),
        (None, "exit 7", None),
        ("right", "exit 5", "exec cat"),
        ("right", "sh -c 'exit 9'", None),
        ("right", "false", None),
    ):
        sid = assert_ok(inst.request("session.new"))["id"]
        time.sleep(1)
        assert_ok(inst.request("session.split", sid, mode="on"))
        time.sleep(1)
        args = {"pane": pane} if pane else {}
        if prior:
            assert_ok(inst.request("session.overlay.open", sid, command=prior, **args))
            time.sleep(1)
            assert_ok(inst.request("session.overlay.close", sid, **args))
            time.sleep(1)
        assert_ok(inst.request("session.overlay.open", sid, command=command, **args))
        reads = []
        for _ in range(12):
            time.sleep(0.5)
            reads.append(inst.request("session.overlay.result", sid, **args).raw)
        node = find_session(assert_ok(inst.request("tree")), sid)
        print(pane, command, prior, reads[0], reads[-1], node.get("paneOverlays"), flush=True)
finally:
    adapter.stop(inst)
