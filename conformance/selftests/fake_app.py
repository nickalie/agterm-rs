"""A stand-in app for adapter self-tests: serves the JSON-line protocol on `AGTERM_CONTROL_SOCKET`.

FAKE_APP_MODE: serve (default), exit (exit 3 before binding), silent (never binds), refuse (`version` fails),
ignore-term (serve, ignoring SIGTERM). FAKE_VERSION: JSON for the `version` result. FAKE_ZMX_LIST: JSON
replacing the `zmx.list` result.
`fake.adopt {pid, session?, pane?, daemon?}` makes that pid a claimed zmx daemon which `zmx.kill` terminates;
killing a left pane promotes the session's right pane into the left slot, as the references do.
"""

import json
import os
import signal
import socket
import sys
import threading

mode = os.environ.get("FAKE_APP_MODE", "serve")
state = os.environ["AGTERM_STATE_DIR"]
with open(os.path.join(state, "fake-env.json"), "w") as sink:
    json.dump(dict(os.environ), sink)

if mode == "exit":
    print("fake app: refusing to start", file=sys.stderr)
    sys.exit(3)
if mode == "silent":
    threading.Event().wait()
if mode == "ignore-term":
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

daemons: list[dict] = []


def handle(request: dict) -> dict:
    with open(os.path.join(state, "fake-requests.jsonl"), "a") as log:
        log.write(json.dumps(request) + "\n")
    cmd, args = request.get("cmd"), request.get("args") or {}
    if cmd == "version":
        if mode == "refuse":
            return {"ok": False, "error": "not ready"}
        return {"ok": True, "result": json.loads(os.environ.get("FAKE_VERSION", '{"app": {"version": "0.0.1"}}'))}
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
    if cmd == "zmx.list":
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
        return {"ok": True, "result": {"zmx": {"entries": entries}}}
    if cmd == "zmx.kill":
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
listener.bind(os.environ["AGTERM_CONTROL_SOCKET"])
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
