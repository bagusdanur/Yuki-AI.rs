#!/usr/bin/env python3
"""Provider LLM tiruan (OpenAI-compatible) untuk harness paritas Fase 1.

POST /v1/chat/completions -> completion kalengan dengan tag emosi,
supaya jalur extractEmotion di Node dan Rust menghasilkan mood sama.
Header `x-mock-fail: 1` atau model "mock-fail" -> HTTP 500 (uji failover).
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REPLY = "Halo juga. Aku denger kok, santai aja ya.\n[emosi: senang]"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # senyapkan log
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            payload = json.loads(raw)
        except Exception:
            payload = {}
        if self.headers.get("x-mock-fail") == "1" or payload.get("model") == "mock-fail":
            body = json.dumps({"error": {"message": "mock failure"}}).encode()
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        body = json.dumps({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": REPLY},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22},
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 3201
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
