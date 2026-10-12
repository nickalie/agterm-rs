"""JSON-line control-socket client confined to one instance's temp root."""

from __future__ import annotations

import json
import os
import socket
import time
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path
from types import TracebackType
from typing import Any, Self

# ControlWire.maxRequestLineBytes, newline excluded: over it the server stops reading and closes
# (macOS first replies ok:false "request too large or read failed"; Linux sends nothing).
MAX_REQUEST_LINE_BYTES = 1 << 20
# agtermctl's response cap; `session.text --all` can carry several MiB of scrollback.
MAX_RESPONSE_LINE_BYTES = 64 << 20
DEFAULT_TIMEOUT = 10.0


class ClientError(Exception):
    pass


class SocketOutsideRoot(ClientError):
    pass


class RequestTooLarge(ClientError):
    pass


class ResponseTooLarge(ClientError):
    pass


class ClientTimeout(ClientError):
    pass


class ConnectFailed(ClientError):
    """The socket is missing or refuses connections: the instance is not (or no longer) serving."""


class NoResponse(ClientError):
    pass


class BadResponse(ClientError):
    pass


@dataclass(frozen=True)
class Response:
    ok: bool
    result: dict[str, Any] | None
    error: str | None
    raw: dict[str, Any]


def check_under_root(socket_path: str | os.PathLike[str], root: str | os.PathLike[str]) -> Path:
    """Resolve symlinks on both sides so a link inside the root cannot point at a live socket."""
    resolved_root = Path(root).resolve()
    resolved = Path(socket_path).resolve()
    if resolved == resolved_root or not resolved.is_relative_to(resolved_root):
        raise SocketOutsideRoot(f"refusing socket {socket_path} ({resolved}): not under temp root {resolved_root}")
    return resolved


def decode_response(line: bytes) -> Response:
    try:
        raw = json.loads(line)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise BadResponse(f"response is not JSON: {error}: {line[:200]!r}") from None
    if not isinstance(raw, dict) or not isinstance(raw.get("ok"), bool):
        raise BadResponse(f"response has no boolean ok: {line[:200]!r}")
    result, error = raw.get("result"), raw.get("error")
    if result is not None and not isinstance(result, dict):
        raise BadResponse(f"response result is not an object: {line[:200]!r}")
    if error is not None and not isinstance(error, str):
        raise BadResponse(f"response error is not a string: {line[:200]!r}")
    return Response(ok=raw["ok"], result=result, error=error, raw=raw)


class Stream:
    """A connection the server keeps after its reply: `zmx.present` presentation frames, `session.overlay.job.run`
    job frames. One JSON object per line in both directions; close it (or use it as a context manager) when done.
    """

    def __init__(self, sock: socket.socket, path: Path, reply: Response, max_line_bytes: int) -> None:
        self.sock = sock
        self.path = path
        # Set by `Client.open_stream` from the first line, before any frame is read.
        self.reply = reply
        self._pending = bytearray()
        self._max_line_bytes = max_line_bytes

    def send(self, frame: Mapping[str, Any]) -> None:
        self.sock.sendall(json.dumps(frame, separators=(",", ":"), ensure_ascii=False).encode() + b"\n")

    def read(self, timeout: float = DEFAULT_TIMEOUT) -> dict[str, Any]:
        """The next frame; `NoResponse` once the server closed the stream, `ClientTimeout` when none came in time."""
        line = self.read_line(timeout)
        try:
            frame = json.loads(line)
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise BadResponse(f"stream frame is not JSON: {error}: {line[:200]!r}") from None
        if not isinstance(frame, dict):
            raise BadResponse(f"stream frame is not an object: {line[:200]!r}")
        return frame

    def read_line(self, timeout: float = DEFAULT_TIMEOUT) -> bytes:
        deadline = time.monotonic() + timeout
        while (newline := self._pending.find(b"\n")) < 0:
            if len(self._pending) > self._max_line_bytes:
                raise ResponseTooLarge(f"stream line exceeds {self._max_line_bytes} bytes")
            try:
                self.sock.settimeout(_remaining(deadline))
                chunk = self.sock.recv(65536)
            except TimeoutError:
                raise ClientTimeout(f"no stream frame from {self.path} within {timeout}s") from None
            except ConnectionResetError as error:
                raise NoResponse(f"stream from {self.path} was reset") from error
            if not chunk:
                raise NoResponse(f"stream from {self.path} closed")
            self._pending += chunk
        line = bytes(self._pending[:newline])
        del self._pending[: newline + 1]
        return line

    def close(self) -> None:
        self.sock.close()

    def __enter__(self) -> Self:
        return self

    def __exit__(
        self, kind: type[BaseException] | None, error: BaseException | None, trace: TracebackType | None
    ) -> None:
        self.close()


class Client:
    """One request per connection, as the server expects."""

    def __init__(
        self,
        socket_path: str | os.PathLike[str],
        root: str | os.PathLike[str],
        timeout: float = DEFAULT_TIMEOUT,
        max_response_bytes: int = MAX_RESPONSE_LINE_BYTES,
    ) -> None:
        check_under_root(socket_path, root)
        self.socket_path = Path(socket_path)
        self.root = Path(root)
        self.timeout = timeout
        self.max_response_bytes = max_response_bytes

    def request(self, cmd: str, target: str | None = None, **args: Any) -> Response:
        payload: dict[str, Any] = {"cmd": cmd}
        if target is not None:
            payload["target"] = target
        if args:
            payload["args"] = args
        return self.send(payload)

    def send(self, payload: dict[str, Any], timeout: float | None = None) -> Response:
        line = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode()
        return self.send_line(line, timeout=timeout)

    def open_stream(self, cmd: str, target: str | None = None, **args: Any) -> Stream:
        """Send one request and keep its connection: after an ok reply the server streams frames on it.

        The stream is returned whatever the reply says; a refused one is already closed by the server.
        """
        payload: dict[str, Any] = {"cmd": cmd}
        if target is not None:
            payload["target"] = target
        if args:
            payload["args"] = args
        line = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode()
        if len(line) > MAX_REQUEST_LINE_BYTES:
            raise RequestTooLarge(f"request too large ({len(line)} bytes, cap {MAX_REQUEST_LINE_BYTES})")
        check_under_root(self.socket_path, self.root)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.settimeout(self.timeout)
            try:
                sock.connect(os.fspath(self.socket_path))
            except TimeoutError:
                raise ClientTimeout(f"no response from {self.socket_path} within {self.timeout}s") from None
            except OSError as error:
                raise ConnectFailed(f"cannot connect to {self.socket_path}: {error}") from error
            sock.sendall(line + b"\n")
            stream = Stream(sock, self.socket_path, Response(False, None, None, {}), self.max_response_bytes)
            stream.reply = decode_response(stream.read_line(self.timeout))
            return stream
        except BaseException:
            sock.close()
            raise

    def send_line(self, line: bytes, timeout: float | None = None, check_cap: bool = True) -> Response:
        """Write `line` plus a newline verbatim; `check_cap=False` lets a test probe the server's own cap."""
        if check_cap and len(line) > MAX_REQUEST_LINE_BYTES:
            raise RequestTooLarge(f"request too large ({len(line)} bytes, cap {MAX_REQUEST_LINE_BYTES})")
        return decode_response(self._exchange(line + b"\n", self.timeout if timeout is None else timeout))

    def _exchange(self, data: bytes, timeout: float) -> bytes:
        # Re-checked per connection: the socket file can appear (or be swapped for a link) after init.
        check_under_root(self.socket_path, self.root)
        deadline = time.monotonic() + timeout
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            try:
                sock.settimeout(timeout)
                try:
                    sock.connect(os.fspath(self.socket_path))
                except TimeoutError:
                    raise
                except OSError as error:
                    raise ConnectFailed(f"cannot connect to {self.socket_path}: {error}") from error
                sock.settimeout(_remaining(deadline))
                try:
                    sock.sendall(data)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # The server stopped reading at its line cap; a reply it sent first is still queued.
                try:
                    return self._read_line(sock, deadline)
                except ConnectionResetError as error:
                    # Linux resets the peer when the server closes with request bytes unread.
                    raise NoResponse(f"connection reset before a response line from {self.socket_path}") from error
            except TimeoutError:
                raise ClientTimeout(f"no response from {self.socket_path} within {timeout}s") from None

    def _read_line(self, sock: socket.socket, deadline: float) -> bytes:
        buffer = bytearray()
        while True:
            sock.settimeout(_remaining(deadline))
            chunk = sock.recv(65536)
            if not chunk:
                raise NoResponse(f"connection closed before a response line from {self.socket_path}")
            newline = chunk.find(b"\n")
            if newline >= 0:
                buffer += chunk[:newline]
            else:
                buffer += chunk
            if len(buffer) > self.max_response_bytes:
                raise ResponseTooLarge(f"response line exceeds {self.max_response_bytes} bytes")
            if newline >= 0:
                return bytes(buffer)


def _remaining(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError
    return remaining
