#!/usr/bin/env python3
"""Mock provider untuk paritas Fase 5 (emotion/games/admin).

Perilaku berdasarkan isi permintaan:
- Prompt extractFacts -> "[]".
- "Balas tepat: OK" (tes koneksi provider admin) -> "OK".
- Tic-tac-toe move (JSON {"cell","line"}) -> kotak kosong pertama.
- RPS (JSON {"pick","line"}) -> selalu "kertas".
- Komentar ttt/line -> satu kalimat polos.
- Selain itu -> balasan companion kalengan dengan [emosi: senang].
Semua body permintaan direkam ke MOCK_REQ_DIR/req_NNNN.json.
"""
import json
import os
import re
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

REQ_DIR = os.environ.get("MOCK_REQ_DIR", "/tmp/yuki-parity5/requests")
os.makedirs(REQ_DIR, exist_ok=True)
counter = {"n": 0}
lock = threading.Lock()

COMPANION_REPLY = "Halo juga. Aku denger kok, santai aja ya.\n[emosi: senang]"


def choose(body):
    messages = body.get("messages", [])
    text_all = "\n".join(str(m.get("content") or "") for m in messages)
    if "Balas HANYA berupa array JSON of string singkat" in text_all:
        return "[]"
    user_last = ""
    for m in reversed(messages):
        if m.get("role") == "user":
            user_last = str(m.get("content") or "")
            break
    if user_last.strip() == "Balas tepat: OK":
        return "OK"
    if '"pick"' in text_all:
        return json.dumps({"pick": "kertas", "line": "Sudah kuputuskan, jangan protes."})
    if "Balas HANYA JSON" in text_all and "Tic-Tac-Toe" in text_all:
        kosong = []
        m = re.search(r"Kotak kosong: ([0-9, ]+)", user_last)
        if m:
            kosong = [int(x) for x in m.group(1).split(",") if x.strip().isdigit()]
        cell = kosong[0] if kosong else 0
        return json.dumps({"cell": cell, "line": "Hmm, segitu saja kemampuanmu."})
    if "Tic-Tac-Toe; kamu memegang O" in text_all:
        return "Baru mulai saja sudah ribut."
    return COMPANION_REPLY


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw or b"{}")
        except Exception:
            body = {}
        with lock:
            counter["n"] += 1
            n = counter["n"]
        with open(os.path.join(REQ_DIR, f"req_{n:04d}.json"), "w") as f:
            json.dump(body, f)
        content = choose(body)
        payload = {
            "id": "chatcmpl-mock5",
            "object": "chat.completion",
            "model": body.get("model", "mock-model"),
            "choices": [
                {
                    "index": 0,
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop",
                }
            ],
            "usage": {"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22},
        }
        data = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 3201), Handler).serve_forever()
