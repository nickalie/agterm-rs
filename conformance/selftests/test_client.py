import json
import shutil
import socket
import tempfile
import threading
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest
from agterm_conformance.client import (
    MAX_REQUEST_LINE_BYTES,
    BadResponse,
    Client,
    ClientTimeout,
    ConnectFailed,
    NoResponse,
    RequestTooLarge,
    ResponseTooLarge,
    SocketOutsideRoot,
)

Handler = Callable[[bytes], bytes | None]


class FakeServer:
    """Serves one connection at a time; `handler` maps the request line to raw reply bytes (None: close).

    With `limit`, it stops reading once the line exceeds it and leaves the rest unread, as the references do.
    """

    def __init__(self, path: Path, handler: Handler, limit: int | None = None) -> None:
        self.path = path
        self.handler = handler
        self.limit = limit
        self.lines: list[bytes] = []
        self.release = threading.Event()
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(path))
        self.listener.listen()
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self) -> None:
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            with conn:
                line = bytearray()
                while not line.endswith(b"\n"):
                    if self.limit is not None and len(line) > self.limit:
                        break
                    size = 65536 if self.limit is None else min(65536, self.limit + 1 - len(line))
                    chunk = conn.recv(size)
                    if not chunk:
                        break
                    line += chunk
                self.lines.append(bytes(line))
                reply = self.handler(bytes(line).rstrip(b"\n"))
                if reply is not None:
                    conn.sendall(reply)

    def close(self) -> None:
        self.release.set()
        # close() alone does not wake a blocked accept() on Linux.
        self.listener.shutdown(socket.SHUT_RDWR)
        self.listener.close()
        self.thread.join(timeout=5)


@pytest.fixture
def root() -> Iterator[Path]:
    # pytest's tmp_path can exceed the AF_UNIX path cap.
    path = Path(tempfile.mkdtemp(prefix="agc-"))
    yield path
    shutil.rmtree(path)


@pytest.fixture
def serve(root: Path) -> Iterator[Callable[..., FakeServer]]:
    servers: list[FakeServer] = []

    def start(handler: Handler, limit: int | None = None) -> FakeServer:
        servers.append(FakeServer(root / "s.sock", handler, limit))
        return servers[-1]

    yield start
    for server in servers:
        server.close()


def reply(obj: object) -> Handler:
    return lambda _line: json.dumps(obj).encode() + b"\n"


def test_success(serve, root: Path) -> None:
    server = serve(reply({"ok": True, "result": {"version": "0.36.0"}}))
    resp = Client(server.path, root).request("version")
    assert (resp.ok, resp.result, resp.error) == (True, {"version": "0.36.0"}, None)
    assert server.lines == [b'{"cmd":"version"}\n']


def test_request_encodes_target_and_args(serve, root: Path) -> None:
    server = serve(reply({"ok": True}))
    Client(server.path, root).request("session.rename", target="abc", name="ü x", hideAfter=1.5)
    assert json.loads(server.lines[0]) == {
        "cmd": "session.rename",
        "target": "abc",
        "args": {"name": "ü x", "hideAfter": 1.5},
    }


def test_error_keeps_result(serve, root: Path) -> None:
    server = serve(reply({"ok": False, "error": "cursor expired", "result": {"events": {"cursor": 7}}}))
    resp = Client(server.path, root).request("events.read", cursor=1)
    assert resp.ok is False
    assert resp.error == "cursor expired"
    assert resp.result == {"events": {"cursor": 7}}


def test_reply_split_across_writes(serve, root: Path) -> None:
    body = json.dumps({"ok": True, "result": {"text": "x" * 200_000}}).encode()
    server = serve(lambda _line: body + b"\n")
    assert Client(server.path, root).request("session.text").result == {"text": "x" * 200_000}


def test_oversized_request_refused_before_connect(serve, root: Path) -> None:
    server = serve(reply({"ok": True}))
    client = Client(server.path, root)
    with pytest.raises(RequestTooLarge, match=f"cap {MAX_REQUEST_LINE_BYTES}"):
        client.request("session.type", text="x" * MAX_REQUEST_LINE_BYTES)
    assert server.lines == []


def test_request_at_cap_is_sent(serve, root: Path) -> None:
    server = serve(reply({"ok": True}))
    line = b'"' + b"x" * (MAX_REQUEST_LINE_BYTES - 2) + b'"'
    assert Client(server.path, root).send_line(line).ok
    assert len(server.lines[0]) == MAX_REQUEST_LINE_BYTES + 1


def test_cap_check_can_be_skipped(serve, root: Path) -> None:
    server = serve(lambda _line: None, limit=MAX_REQUEST_LINE_BYTES)
    with pytest.raises(NoResponse):
        Client(server.path, root).send_line(b"x" * (MAX_REQUEST_LINE_BYTES + 1), check_cap=False)
    assert server.lines == [b"x" * (MAX_REQUEST_LINE_BYTES + 1)]


def test_over_cap_reply_before_close_is_read(serve, root: Path) -> None:
    server = serve(reply({"ok": False, "error": "request too large or read failed"}), limit=MAX_REQUEST_LINE_BYTES)
    resp = Client(server.path, root).send_line(b"x" * (4 * MAX_REQUEST_LINE_BYTES), check_cap=False)
    assert (resp.ok, resp.error) == (False, "request too large or read failed")


def test_missing_socket(root: Path) -> None:
    with pytest.raises(ConnectFailed):
        Client(root / "s.sock", root).request("tree")


def test_oversized_response(serve, root: Path) -> None:
    server = serve(reply({"ok": True, "result": {"text": "x" * 100}}))
    with pytest.raises(ResponseTooLarge, match="exceeds 64 bytes"):
        Client(server.path, root, max_response_bytes=64).request("session.text")


def test_timeout(serve, root: Path) -> None:
    holder: list[FakeServer] = []

    def hang(_line: bytes) -> None:
        holder[0].release.wait(timeout=5)

    holder.append(serve(hang))
    with pytest.raises(ClientTimeout, match="within 0.2s"):
        Client(holder[0].path, root, timeout=0.2).request("tree")
    holder[0].release.set()


def test_closed_without_reply(serve, root: Path) -> None:
    server = serve(lambda _line: None)
    with pytest.raises(NoResponse):
        Client(server.path, root).request("tree")


@pytest.mark.parametrize(
    "raw",
    [b"not json\n", b"[1]\n", b'{"result": {}}\n', b'{"ok": "yes"}\n', b'{"ok": true, "result": 3}\n'],
)
def test_bad_response(serve, root: Path, raw: bytes) -> None:
    server = serve(lambda _line: raw)
    with pytest.raises(BadResponse):
        Client(server.path, root).request("tree")


@pytest.fixture
def live_server() -> Iterator[FakeServer]:
    other = Path(tempfile.mkdtemp(prefix="agc-"))
    server = FakeServer(other / "live.sock", reply({"ok": True}))
    yield server
    server.close()
    shutil.rmtree(other)


def test_socket_outside_root_refused(live_server: FakeServer, root: Path) -> None:
    with pytest.raises(SocketOutsideRoot):
        Client(live_server.path, root)
    assert live_server.lines == []


@pytest.mark.parametrize("relative", ["..", "../x.sock", "."])
def test_socket_escaping_root_refused(root: Path, relative: str) -> None:
    with pytest.raises(SocketOutsideRoot):
        Client(root / relative, root)


def test_symlink_out_of_root_refused_at_connect(live_server: FakeServer, root: Path) -> None:
    link = root / "s.sock"
    client = Client(link, root)
    link.symlink_to(live_server.path)
    with pytest.raises(SocketOutsideRoot):
        client.request("tree")
    assert live_server.lines == []


# -- streams


class StreamServer:
    """Accepts one connection, reads its request line and hands the open connection to `script`."""

    def __init__(self, path: Path, script: Callable[[socket.socket, bytes], None]) -> None:
        self.path = path
        self.request: bytes | None = None
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(path))
        self.listener.listen()
        self.thread = threading.Thread(target=self._serve, args=(script,), daemon=True)
        self.thread.start()

    def _serve(self, script: Callable[[socket.socket, bytes], None]) -> None:
        conn, _ = self.listener.accept()
        with conn:
            line = bytearray()
            while not line.endswith(b"\n"):
                chunk = conn.recv(65536)
                if not chunk:
                    return
                line += chunk
            self.request = bytes(line)
            script(conn, bytes(line))

    def close(self) -> None:
        self.listener.close()
        self.thread.join(timeout=5)


def frames(conn: socket.socket):
    buffer = b""
    while True:
        while b"\n" not in buffer:
            chunk = conn.recv(65536)
            if not chunk:
                return
            buffer += chunk
        line, buffer = buffer.split(b"\n", 1)
        yield json.loads(line)


@pytest.fixture
def stream_server(root: Path) -> Iterator[Callable[[Callable[[socket.socket, bytes], None]], StreamServer]]:
    servers: list[StreamServer] = []

    def start(script: Callable[[socket.socket, bytes], None]) -> StreamServer:
        servers.append(StreamServer(root / "st.sock", script))
        return servers[-1]

    yield start
    for server in servers:
        server.close()


def test_stream_keeps_the_connection_after_an_ok_reply(stream_server, root: Path) -> None:
    received: list[dict] = []

    def echo(conn: socket.socket, _line: bytes) -> None:
        # The reply and the first frame in one write: the client must split them.
        conn.sendall(b'{"ok":true,"result":{"id":"s1"}}\n{"kind":"hello","gen":1}\n')
        for frame in frames(conn):
            received.append(frame)
            conn.sendall(json.dumps({"kind": "echo", "of": frame}).encode() + b"\n")

    server = stream_server(echo)
    with Client(server.path, root).open_stream("zmx.present", "s1") as stream:
        assert (stream.reply.ok, stream.reply.result) == (True, {"id": "s1"})
        assert stream.read() == {"kind": "hello", "gen": 1}
        stream.send({"kind": "ping", "gen": 1, "rev": 0})
        assert stream.read() == {"kind": "echo", "of": {"kind": "ping", "gen": 1, "rev": 0}}
    server.thread.join(timeout=5)
    assert json.loads(server.request) == {"cmd": "zmx.present", "target": "s1"}
    assert received == [{"kind": "ping", "gen": 1, "rev": 0}]


def test_stream_refused_reply_then_closed(stream_server, root: Path) -> None:
    server = stream_server(lambda conn, _line: conn.sendall(b'{"ok":false,"error":"session is not live-backed"}\n'))
    with Client(server.path, root).open_stream("zmx.present", "s1") as stream:
        assert (stream.reply.ok, stream.reply.error) == (False, "session is not live-backed")
        with pytest.raises(NoResponse, match="closed"):
            stream.read()


def test_stream_read_times_out(stream_server, root: Path) -> None:
    release = threading.Event()

    def hold(conn: socket.socket, _line: bytes) -> None:
        conn.sendall(b'{"ok":true}\n')
        release.wait(5)

    server = stream_server(hold)
    try:
        with (
            Client(server.path, root).open_stream("session.overlay.job.run", "j1") as stream,
            pytest.raises(ClientTimeout, match="no stream frame"),
        ):
            stream.read(timeout=0.2)
    finally:
        release.set()


def test_stream_frame_must_be_a_json_object(stream_server, root: Path) -> None:
    server = stream_server(lambda conn, _line: conn.sendall(b'{"ok":true}\n[1]\nnope\n'))
    with Client(server.path, root).open_stream("zmx.present", "s1") as stream:
        with pytest.raises(BadResponse, match="not an object"):
            stream.read()
        with pytest.raises(BadResponse, match="not JSON"):
            stream.read()


def test_stream_symlink_out_of_root_refused(live_server: FakeServer, root: Path) -> None:
    link = root / "st.sock"
    client = Client(link, root)
    link.symlink_to(live_server.path)
    with pytest.raises(SocketOutsideRoot):
        client.open_stream("zmx.present", "s1")
    assert live_server.lines == []
