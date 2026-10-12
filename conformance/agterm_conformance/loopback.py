"""An HTTP server on 127.0.0.1 for pages a test serves itself."""

from __future__ import annotations

import socketserver
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class LoopbackServer(ThreadingHTTPServer):
    def server_bind(self) -> None:
        # HTTPServer's own bind resolves the host with getfqdn. On macOS the test process got the Local Network prompt
        # while it did, a system alert that kept every later agterm from presenting a window and binding its socket.
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
