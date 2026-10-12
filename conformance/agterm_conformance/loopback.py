"""An HTTP server on 127.0.0.1 for pages a test serves itself."""

from __future__ import annotations

import socketserver
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class LoopbackServer(ThreadingHTTPServer):
    def server_bind(self) -> None:
        # HTTPServer's own bind resolves the host with getfqdn, and on macOS that lookup raises the Local Network
        # prompt for the test process: a system alert that keeps every agterm launched after it from presenting a
        # window, so none of them binds its socket.
        socketserver.TCPServer.server_bind(self)
        host, port = self.server_address[:2]
        self.server_name, self.server_port = str(host), int(port)


@contextmanager
def serve(handler: type[BaseHTTPRequestHandler]) -> Iterator[LoopbackServer]:
    server = LoopbackServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
