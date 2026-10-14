#!/usr/bin/env python3
"""Bench server for the spike: serves bench/ and the media dir (with Range), and appends POST /report bodies to a
JSON-lines file. Usage: server.py PORT MEDIA_DIR REPORT_FILE"""
import http.server
import os
import re
import sys
import time

BENCH = os.path.dirname(os.path.abspath(__file__))
PORT, MEDIA, REPORT = int(sys.argv[1]), sys.argv[2], sys.argv[3]


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=BENCH, **kwargs)

    def log_message(self, fmt, *args):
        pass

    def end_headers(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
        with open(REPORT, "a") as out:
            out.write(f'{{"t":{time.time():.3f},"data":{body}}}\n')
        self.send_response(204)
        self.end_headers()

    def do_GET(self):
        if not self.path.startswith("/media/"):
            return super().do_GET()
        path = os.path.join(MEDIA, os.path.basename(self.path.split("?")[0]))
        if not os.path.isfile(path):
            return self.send_error(404)
        size = os.path.getsize(path)
        start, end = 0, size - 1
        match = re.match(r"bytes=(\d*)-(\d*)", self.headers.get("Range", ""))
        if match:
            start = int(match.group(1) or 0)
            end = int(match.group(2) or size - 1)
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        else:
            self.send_response(200)
        self.send_header("Content-Type", "video/mp4" if path.endswith(".mp4") else "video/webm")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        with open(path, "rb") as media:
            media.seek(start)
            left = end - start + 1
            try:
                while left > 0:
                    chunk = media.read(min(1 << 20, left))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    left -= len(chunk)
            except (BrokenPipeError, ConnectionResetError):
                pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
