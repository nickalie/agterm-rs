"""A stand-in dbus-daemon for adapter self-tests: listens on its config's `<listen>unix:path=...` until SIGTERM.

FAKE_SESSION_BUS_MODE: serve (default) or exit (exit 3 before binding). It records its argv and environment in
`fake-session-bus.json` beside its `--config-file`.
"""

import json
import os
import re
import socket
import sys
import threading

options = dict(arg.removeprefix("--").partition("=")[::2] for arg in sys.argv[1:])
config = options["config-file"]
with open(os.path.join(os.path.dirname(config), "fake-session-bus.json"), "w") as sink:
    json.dump({"argv": sys.argv[1:], "env": dict(os.environ)}, sink)

if os.environ.get("FAKE_SESSION_BUS_MODE") == "exit":
    print("fake session bus: refusing to start", file=sys.stderr)
    sys.exit(3)
with open(config) as source:
    path = re.search(r"<listen>unix:path=([^<]+)</listen>", source.read()).group(1)
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(path)
listener.listen()
threading.Event().wait()
