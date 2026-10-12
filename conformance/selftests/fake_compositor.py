"""A stand-in weston for adapter self-tests: listens on `$XDG_RUNTIME_DIR/<--socket>` until SIGTERM.

FAKE_COMPOSITOR_MODE: serve (default), exit (exit 4 before binding), silent (never binds). It records its argv and
environment in `fake-compositor.json` beside its `--log`.
"""

import json
import os
import socket
import sys
import threading

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
