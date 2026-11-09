#!/usr/bin/env python3
"""Mock push service: merekam POST Web Push (header + body mentah) ke disk."""
import base64
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

OUT_DIR = os.environ.get("MOCK_PUSH_DIR", "/tmp/yuki-parity3/push")
os.makedirs(OUT_DIR, exist_ok=True)
counter = {"n": 0}
lock = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        with lock:
            counter["n"] += 1
            n = counter["n"]
        record = {
            "path": self.path,
            "headers": {k: v for k, v in self.headers.items()},
            "body_b64": base64.b64encode(raw).decode(),
        }
        with open(os.path.join(OUT_DIR, f"push_{n:04d}.json"), "w") as fh:
            json.dump(record, fh)
        self.send_response(201)
        self.end_headers()

    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")


if __name__ == "__main__":
    import ssl

    port = int(os.environ.get("MOCK_PUSH_PORT", "3202"))
    server = HTTPServer(("127.0.0.1", port), Handler)
    cert = os.environ.get("MOCK_PUSH_CERT")
    if cert:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(cert, os.environ.get("MOCK_PUSH_KEY"))
        server.socket = ctx.wrap_socket(server.socket, server_side=True)
    server.serve_forever()
