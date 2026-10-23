"""A second zmx client on one of an instance's daemons: the local stand-in for a viewer attached over ssh.

Upstream hands an overlay to a presenter only while every pane's own client follows (`followsRemotely`), and
a pane follows only when another client of its daemon claimed the lead. `leading_client` is that client, started
the way `RemoteSession.attachCommand` starts the viewer's on the origin, minus ssh.
"""

from __future__ import annotations

import fcntl
import os
import struct
import subprocess
import termios
import threading
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from types import TracebackType
from typing import Any, Protocol, Self

from agterm_conformance.impls.base import MINIMAL_PATH

STOP_TIMEOUT = 5.0
_KEPT_OUTPUT = 65536
# The create-only command: should the daemon have gone, attach would otherwise create one running a shell.
GONE_GUARD = "printf '%s\\n' 'agterm: remote session is gone'; exit 1"


class ZmxInstance(Protocol):
    live_dir: str
    home: Path


class PtyProcess:
    """A program on a pty of its own, its output drained so it never blocks writing; `stop` ends and reaps it."""

    def __init__(self, argv: Sequence[str], env: Mapping[str, str], columns: int = 80, rows: int = 24) -> None:
        master, slave = os.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
            self.process = subprocess.Popen(
                list(argv), stdin=slave, stdout=slave, stderr=slave, env=dict(env), start_new_session=True
            )
        except BaseException:
            os.close(master)
            raise
        finally:
            os.close(slave)
        self._master: int | None = master
        self._output = bytearray()
        self._lock = threading.Lock()
        self._drain = threading.Thread(target=self._read_all, args=(master,), daemon=True)
        self._drain.start()

    def _read_all(self, master: int) -> None:
        while True:
            try:
                chunk = os.read(master, 4096)
            except OSError:  # EIO on Linux once the program and its pty slave are gone
                return
            if not chunk:
                return
            with self._lock:
                self._output += chunk
                del self._output[:-_KEPT_OUTPUT]

    def output(self) -> str:
        with self._lock:
            return self._output.decode(errors="replace")

    def stop(self, timeout: float = STOP_TIMEOUT) -> None:
        """SIGTERM this program's own pid, SIGKILL it if it ignores that, then close the pty."""
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout)
        self._drain.join(timeout)
        if self._master is not None:
            os.close(self._master)
            self._master = None

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self.stop()


def leading_client(instance: ZmxInstance, endpoint: Mapping[str, Any], daemon: str) -> PtyProcess:
    """`zmx attach <daemon>` claiming the lead, so the pane's own client in `instance` follows.

    `endpoint` is the `zmx.tree` read-back (`live.tree` in the Rust build); one naming another instance's daemons is
    refused.
    """
    executable, directory = endpoint["executable"], endpoint["socketDirectory"]
    if directory != instance.live_dir:
        raise ValueError(f"endpoint socket directory {directory!r} is not this instance's {instance.live_dir!r}")
    if not os.path.isabs(executable):
        raise ValueError(f"endpoint executable {executable!r} is not absolute")
    env = {
        "PATH": MINIMAL_PATH,
        "HOME": os.fspath(instance.home),
        "TERM": "xterm-256color",
        # The variables the origin's own pane sets (`ZmxSupport`), empty meaning unset to zmx.
        "ZMX_SESSION": "",
        "ZMX_SESSION_PREFIX": "",
        "ZMX_NO_DETACH_KEY": "1",
        "ZMX_DIR": directory,
        "ZMX_MANAGED": uuid.uuid4().hex,
        "ZMX_MANAGED_CLAIM": "1",
    }
    return PtyProcess([executable, "attach", daemon, "/bin/sh", "-c", GONE_GUARD], env)
