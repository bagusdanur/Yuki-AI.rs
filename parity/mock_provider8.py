#!/usr/bin/env python3
"""Mock provider Fase 8: streaming SSE + failover + potong di tengah +
tool calls untuk sandbox/sintaks/browser. Mencatat semua request body
ke $MOCK_REQ_DIR/req_NNNN.json."""
import json
import os
import socket
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REQ_DIR = os.environ.get("MOCK_REQ_DIR", "/tmp/yuki-parity8/requests")
os.makedirs(REQ_DIR, exist_ok=True)
COUNTER = [0]

CANNED = "Halo juga. Aku denger kok, santai aja ya.\n[emosi: senang]"
DELTAS = ["Halo ", "juga. ", "Aku denger kok, ", "santai aja ya.\n", "[emosi: senang]"]

JS_CASES = {
    "JSTEST1": "console.log('halo dari js'); return 40 + 2;",
    "JSTEST2": "await new Promise(r => setTimeout(r, 50)); return 'timer-ok';",
    "JSTEST3": "throw new Error('ledak');",
    "JSTEST4": "while (true) {}",
}
SYNTAX_CASES = {
    "SYNJSBAD": ("analyze_code_syntax", {"code": "const x = ;", "language": "javascript"}),
    "SYNJSOK": ("analyze_code_syntax", {"code": "const x = 1;", "language": "javascript"}),
    "SYNTSBAD": ("analyze_code_syntax", {"code": "const x: number = ;", "language": "typescript"}),
    "SYNTSOK": ("analyze_code_syntax", {"code": "const x: number = 1;", "language": "typescript"}),
    "BROWSESSRF": ("browse_page", {"url": "http://127.0.0.1:8899/rahasia"}),
}


def last_user_text(body):
    for msg in reversed(body.get("messages", [])):
        if msg.get("role") == "user":
            return msg.get("content") or ""
    return ""


def has_tool_message(body):
    return any(m.get("role") == "tool" for m in body.get("messages", []))


def tool_call_payload(name, args):
    return {
        "id": f"call_mock_{name}",
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": f"call_mock_{name}",
                            "type": "function",
                            "function": {"name": name, "arguments": json.dumps(args)},
                        }
                    ],
                },
                "finish_reason": "tool_calls",
            }
        ],
        "usage": {"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22},
    }


def completion_payload(text):
    return {
        "id": "chatcmpl-mock8",
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22},
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except Exception:
            body = {}
        COUNTER[0] += 1
        with open(os.path.join(REQ_DIR, f"req_{COUNTER[0]:04d}.json"), "w") as fh:
            json.dump(body, fh)

        text = last_user_text(body)
        all_text = json.dumps(body)
        model = body.get("model", "")

        # extractFacts -> array kosong
        if "ambil fakta penting" in all_text:
            return self._json(completion_payload("[]"))

        # Semua provider mati
        if "STREAMDEAD" in text:
            return self._fail()

        # Failover: primary menolak sebelum stream untuk marker ini
        if "STREAMFAILBACKUP" in text and model == "mock-primary":
            return self._fail()

        # Agent: kembalikan tool call sesuai marker
        if not has_tool_message(body):
            for marker, code in JS_CASES.items():
                if marker in text:
                    return self._json(
                        tool_call_payload("run_javascript_code", {"code": code})
                    )
            for marker, (name, args) in SYNTAX_CASES.items():
                if marker in text:
                    return self._json(tool_call_payload(name, args))
            if "BROWSECONTOH" in text:
                return self._json(
                    tool_call_payload("browse_page", {"url": "https://example.com/"})
                )
            if "SHOTCONTOH" in text:
                return self._json(
                    tool_call_payload("screenshot_url", {"url": "https://example.com/"})
                )
            if "EXTRACTCONTOH" in text:
                return self._json(
                    tool_call_payload(
                        "browser_extract",
                        {"url": "https://example.com/", "selector": "h1"},
                    )
                )
        else:
            return self._json(completion_payload("Selesai, sudah kujalankan. [emosi: senang]"))

        if body.get("stream"):
            if "STREAMCUT" in text:
                return self._stream_cut()
            return self._stream_full()

        return self._json(completion_payload(CANNED))

    def _json(self, payload):
        data = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _fail(self):
        data = json.dumps({"error": {"message": "mock mati"}}).encode()
        self.send_response(500)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _sse_head(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

    def _chunk(self, data: bytes):
        self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
        self.wfile.flush()

    def _stream_full(self):
        self._sse_head()
        for delta in DELTAS:
            chunk = {
                "choices": [
                    {"index": 0, "delta": {"content": delta}, "finish_reason": None}
                ]
            }
            self._chunk(f"data: {json.dumps(chunk)}\n\n".encode())
        final = {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
        self._chunk(f"data: {json.dumps(final)}\n\n".encode())
        self._chunk(b"data: [DONE]\n\n")
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()

    def _stream_cut(self):
        self._sse_head()
        for delta in DELTAS[:2]:
            chunk = {
                "choices": [
                    {"index": 0, "delta": {"content": delta}, "finish_reason": None}
                ]
            }
            self._chunk(f"data: {json.dumps(chunk)}\n\n".encode())
        # Putus kasar: tutup soket tanpa finish_reason/[DONE].
        try:
            self.connection.shutdown(socket.SHUT_RDWR)
        except Exception:
            pass
        self.close_connection = True


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 3201
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
