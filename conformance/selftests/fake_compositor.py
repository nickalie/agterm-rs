"""A stand-in weston for adapter self-tests: listens on `$XDG_RUNTIME_DIR/<--socket>` until SIGTERM.

FAKE_COMPOSITOR_MODE: serve (default), exit (exit 4 before binding), silent (never binds). It records its argv and
environment in `fake-compositor.json` beside its `--log`. `--help` lists the options as weston 15 does, `--fake-seat`
among them, unless FAKE_COMPOSITOR_HELP=old (a weston 13 or 14, which has none).
"""

import json
import os
import socket
import sys
import threading

if "--help" in sys.argv[1:]:
    print("Usage: weston [OPTIONS]\n\n  --backend=BACKEND\tBackend module\n  --socket=NAME\t\tName of socket")
    if os.environ.get("FAKE_COMPOSITOR_HELP") != "old":
        print("  --fake-seat\t\tUse a fake seat for compatibility")
    sys.exit(0)

options = dict(arg.removeprefix("--").partition("=")[::2] for arg in sys.argv[1:])
with open(os.path.join(os.path.dirname(options["log"]), "fake-compositor.json"), "w") as sink:
    json.dump({"argv": sys.argv[1:], "env": dict(os.environ)}, sink)

mode = os.environ.get("FAKE_COMPOSITOR_MODE", "serve")
if mode == "exit":
    print("fake compositor: refusing to start", file=sys.stderr)
    sys.exit(4)
if mode == "serve":
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(os.path.join(os.environ["XDG_RUNTIME_DIR"], options["socket"]))
    listener.listen()
threading.Event().wait()
