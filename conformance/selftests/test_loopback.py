import socket
import urllib.request
from http.server import BaseHTTPRequestHandler

import pytest
from agterm_conformance.loopback import serve


class _Hello(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"hello")

    def log_message(self, *args: object) -> None:
        pass


def test_serves_on_loopback_without_a_name_lookup(monkeypatch: pytest.MonkeyPatch) -> None:
    def no_lookup(*args: object) -> str:
        raise AssertionError("getfqdn raises the macOS Local Network prompt")

    monkeypatch.setattr(socket, "getfqdn", no_lookup)
    with serve(_Hello) as server:
        assert server.server_name == "127.0.0.1"
        with urllib.request.urlopen(f"http://127.0.0.1:{server.server_port}/") as response:
            assert response.read() == b"hello"
