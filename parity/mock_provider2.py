#!/usr/bin/env python3
"""Mock provider untuk paritas Fase 2.

- Permintaan chat biasa -> balasan kalengan + merekam body permintaan ke
  direktori requests (untuk memeriksa system prompt / konteks memory).
- Prompt extractFacts -> array fakta tergantung kata kunci di baris USER.
- Prompt summarizeProfile / summarizeConversation -> teks kalengan.
"""
import json
import os
import re
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

REQ_DIR = os.environ.get("MOCK_REQ_DIR", "/tmp/yuki-parity2/requests")
os.makedirs(REQ_DIR, exist_ok=True)
counter = {"n": 0}
lock = threading.Lock()

CHAT_REPLY = "Halo juga. Aku denger kok, santai aja ya.\n[emosi: senang]"


def choose_content(messages):
    text = "\n".join(str(m.get("content", "")) for m in messages)
    if "Balas HANYA berupa array JSON of string singkat" in text:
        user_line = ""
        m = re.search(r"USER: (.*)", text)
        if m:
            user_line = m.group(1)
        if "mendaki" in user_line:
            return '["Pengguna suka mendaki gunung"]'
        if "kopi" in user_line:
            return '["Pengguna menyukai kopi pahit", "Pengguna tinggal di Semarang"]'
        return "[]"
    if "Rangkum fakta-fakta ini menjadi satu profil" in text:
        return "Profil uji: pengguna menyukai kopi pahit dan tinggal di Semarang."
    if "Buat ringkasan singkat (3-5 kalimat)" in text:
        return "Ringkasan uji: percakapan berjalan santai dan bersahabat."
    return CHAT_REPLY


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"data": []}')

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw.decode())
        except Exception:
            body = {}
        messages = body.get("messages", [])
        with lock:
            counter["n"] += 1
            n = counter["n"]
        with open(os.path.join(REQ_DIR, f"req_{n:04d}.json"), "w") as fh:
            json.dump(body, fh)
        content = choose_content(messages)
        payload = {
            "choices": [
                {"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}
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
