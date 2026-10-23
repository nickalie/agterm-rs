"""A stand-in app for adapter self-tests: serves the JSON-line protocol on `AGTERM_CONTROL_SOCKET` (under the Rust
adapter, `NGTERM_CONTROL_SOCKET`: whichever prefix the adapter set).

FAKE_APP_MODE: serve (default), exit (exit 3 before binding), silent (never binds), refuse (`version` fails),
ignore-term (serve, ignoring SIGTERM and SIGABRT, each recorded in `fake-signals`; FAKE_ABORT=exit makes SIGABRT
exit 134 instead, without a core). FAKE_VERSION: JSON for the `version` result. FAKE_ZMX_LIST: JSON
replacing the `zmx.list` result. FAKE_LIVE_GROUP: the live-session group's name, `zmx` (upstream's) by default; the
fake answers `<group>.list` under the result key `<group>`, as the Rust build answers `live.list` under `live`.
`fake.adopt {pid, session?, pane?, daemon?}` makes that pid a claimed zmx daemon which `zmx.kill` terminates;
killing a left pane promotes the session's right pane into the left slot, as upstream does.
`tree` serves a settable tree (`fake.set_tree {tree?, error?, delay?}`); `events.read` serves a ring with
upstream's cursor semantics, appended to by `fake.emit {kind, session?, payload?, delay?}`. A delay applies the
change from a timer after the reply, as an app does work after acknowledging a command.
"""

import json
import os
import signal
import socket
import sys
import threading
import uuid

mode = os.environ.get("FAKE_APP_MODE", "serve")
# The adapter's prefix: upstream's AGTERM_, the Rust build's NGTERM_.
PREFIX = "NGTERM_" if "NGTERM_STATE_DIR" in os.environ else "AGTERM_"
state = os.environ[PREFIX + "STATE_DIR"]
with open(os.path.join(state, "fake-env.json"), "w") as sink:
    json.dump(dict(os.environ), sink)

if mode == "exit":
    print("fake app: refusing to start", file=sys.stderr)
    sys.exit(3)
if mode == "silent":
    threading.Event().wait()
if mode == "ignore-term":

    def record(signum: int, frame: object) -> None:
        name = signal.Signals(signum).name.removeprefix("SIG")
        with open(os.path.join(state, "fake-signals"), "a") as sink:
            sink.write(f"{name}\n")
        if signum == signal.SIGABRT and os.environ.get("FAKE_ABORT") == "exit":
            os._exit(134)

    signal.signal(signal.SIGTERM, record)
    # Handled, never the default action: no test may leave a real core behind.
    signal.signal(signal.SIGABRT, record)

daemons: list[dict] = []
lock = threading.Lock()
tree = {"workspaces": [{"id": "w1", "active": True, "sessions": [{"id": "s1", "name": "one", "active": True}]}]}
tree_error = None
run_id = str(uuid.uuid4()).upper()
ring: list[dict] = []


def later(args: dict, change) -> None:
    delay = float(args.get("delay", 0))
    if delay:
        threading.Timer(delay, change).start()
    else:
        change()


def set_tree(args: dict) -> None:
    global tree, tree_error
    with lock:
        tree, tree_error = args.get("tree", tree), args.get("error")


def emit(args: dict) -> None:
    with lock:
        ring.append(
            {
                "seq": len(ring) + 1,
                "ts": 0,
                "kind": args["kind"],
                "session": args.get("session"),
                "payload": args.get("payload", {}),
            }
        )


def read_events(args: dict) -> dict:
    with lock:
        anchor = {"run": run_id, "next": len(ring), "items": []}
        if "run" not in args and "after" not in args:
            return {"ok": True, "result": {"events": anchor}}
        if args.get("run") != run_id:
            return {"ok": False, "error": "event run changed", "result": {"events": anchor}}
        after = int(args["after"])
        if after > len(ring):
            return {"ok": False, "error": "event cursor is ahead of the current sequence", "result": {"events": anchor}}
        kinds, limit = args.get("kinds"), int(args.get("limit", 100))
        items = []
        for event in ring[after:]:
            if kinds and event["kind"] not in kinds:
                continue
            items.append(event)
            if len(items) == limit:
                return {"ok": True, "result": {"events": {"run": run_id, "next": event["seq"], "items": items}}}
        return {"ok": True, "result": {"events": {"run": run_id, "next": len(ring), "items": items}}}


def handle(request: dict) -> dict:
    with open(os.path.join(state, "fake-requests.jsonl"), "a") as log:
        log.write(json.dumps(request) + "\n")
    cmd, args = request.get("cmd"), request.get("args") or {}
    if cmd == "version":
        if mode == "refuse":
            return {"ok": False, "error": "not ready"}
        return {"ok": True, "result": json.loads(os.environ.get("FAKE_VERSION", '{"app": {"version": "0.0.1"}}'))}
    if cmd == "tree":
        with lock:
            return {"ok": False, "error": tree_error} if tree_error else {"ok": True, "result": {"tree": tree}}
    if cmd == "fake.set_tree":
        later(args, lambda: set_tree(args))
        return {"ok": True}
    if cmd == "fake.emit":
        later(args, lambda: emit(args))
        return {"ok": True}
    if cmd == "events.read":
        return read_events(args)
    if cmd == "fake.adopt":
        pid = int(args["pid"])
        daemons.append(
            {
                "pid": pid,
                "daemon": args.get("daemon", f"d{pid}"),
                "sessionID": args.get("session", f"s{pid}"),
                "pane": args.get("pane", "left"),
            }
        )
        return {"ok": True}
    group = os.environ.get("FAKE_LIVE_GROUP", "zmx")
    if cmd == f"{group}.list":
        if "FAKE_ZMX_LIST" in os.environ:
            return {"ok": True, "result": json.loads(os.environ["FAKE_ZMX_LIST"])}
        entries = [
            {
                "daemon": d["daemon"],
                "state": "claimed",
                "sessionID": d["sessionID"],
                "pane": d["pane"],
                "windowID": "w1",
            }
            for d in sorted(daemons, key=lambda d: d["daemon"])
        ]
        return {"ok": True, "result": {group: {"entries": entries}}}
    if cmd == f"{group}.kill":
        session, pane = request.get("target"), args.get("pane")
        match = next((d for d in daemons if d["sessionID"] == session and d["pane"] == pane), None)
        if match is None:
            return {"ok": False, "error": f"no {pane} pane daemon for session {session}"}
        os.kill(match["pid"], signal.SIGTERM)
        daemons.remove(match)
        for survivor in daemons:
            if pane == "left" and survivor["sessionID"] == session:
                survivor["pane"] = "left"
        return {"ok": True}
    return {"ok": False, "error": f"unknown command: {cmd}"}


listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
# A previous run on the same state dir (a relaunch) leaves its socket file behind, as a quit app may.
try:
    os.unlink(os.environ[PREFIX + "CONTROL_SOCKET"])
except FileNotFoundError:
    pass
listener.bind(os.environ[PREFIX + "CONTROL_SOCKET"])
listener.listen()
while True:
    conn, _ = listener.accept()
    with conn:
        data = b""
        while b"\n" not in data:
            chunk = conn.recv(65536)
            if not chunk:
                break
            data += chunk
        if b"\n" in data:
            reply = handle(json.loads(data.split(b"\n", 1)[0]))
            conn.sendall(json.dumps(reply).encode() + b"\n")
