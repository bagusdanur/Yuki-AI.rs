#!/usr/bin/env python3
"""Mock provider untuk paritas Fase 4 (agent).

Skrip perilaku berdasarkan isi permintaan:
- Prompt extractFacts -> "[]".
- Worker subagent ("Kamu worker read-only Yuki"): turn pertama memanggil
  get_current_time, setelah ada hasil tool -> ringkasan teks.
- Mode agent (ada tools):
  * ada pesan WORKFLOW RESUME -> teks final.
  * pesan terakhir role=tool -> teks final.
  * kata kunci user: "hitung" -> calculate_expression,
    "jadwalkan" -> schedule_task, "delegasikan" -> delegate_tasks,
    "workspace" -> list_workspace_files.
- Selain itu -> balasan companion kalengan.
Semua body permintaan direkam ke MOCK_REQ_DIR/req_NNNN.json.
"""
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

REQ_DIR = os.environ.get("MOCK_REQ_DIR", "/tmp/yuki-parity4/requests")
os.makedirs(REQ_DIR, exist_ok=True)
counter = {"n": 0}
lock = threading.Lock()

COMPANION_REPLY = "Halo juga. Aku denger kok, santai aja ya.\n[emosi: senang]"
FINAL_REPLY = "Selesai, hasilnya sudah Yuki periksa.\n[emosi: tenang]"


def tool_call(name, args):
    return {
        "id": f"call_mock_{name}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(args)},
    }


def choose(body):
    messages = body.get("messages", [])
    text_all = "\n".join(str(m.get("content") or "") for m in messages)
    if "Balas HANYA berupa array JSON of string singkat" in text_all:
        return {"content": "[]"}
    if "Kamu worker read-only Yuki" in text_all:
        if any(m.get("role") == "tool" for m in messages):
            return {"content": "Ringkasan worker: subtask selesai dicek dengan tool."}
        return {"tool_calls": [tool_call("get_current_time", {"timezone": "WIB"})]}
    tools = body.get("tools") or []
    if tools:
        if any("WORKFLOW RESUME" in str(m.get("content") or "") for m in messages):
            return {"content": "Siap, sudah dilanjutkan dan terverifikasi.\n[emosi: tenang]"}
        last = messages[-1] if messages else {}
        if last.get("role") == "tool":
            return {"content": FINAL_REPLY}
        user_text = ""
        for m in reversed(messages):
            if m.get("role") == "user":
                user_text = str(m.get("content") or "")
                break
        low = user_text.lower()
        if "hitung" in low:
            return {"tool_calls": [tool_call("calculate_expression", {"expression": "25% * 200"})]}
        if "jadwalkan" in low:
            return {"tool_calls": [tool_call("schedule_task", {"title": "Minum vitamin", "schedule": "besok jam 07:00"})]}
        if "delegasikan" in low:
            return {"tool_calls": [tool_call("delegate_tasks", {"tasks": ["cek waktu sekarang", "cek jam sekarang"]})]}
        if "workspace" in low:
            return {"tool_calls": [tool_call("list_workspace_files", {"path": "."})]}
        return {"content": "Baik.\n[emosi: tenang]"}
    return {"content": COMPANION_REPLY}


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
        with lock:
            counter["n"] += 1
            n = counter["n"]
        with open(os.path.join(REQ_DIR, f"req_{n:04d}.json"), "w") as fh:
            json.dump(body, fh)
        choice = choose(body)
        message = {"role": "assistant", "content": choice.get("content", "")}
        if choice.get("tool_calls"):
            message["tool_calls"] = choice["tool_calls"]
        payload = {
            "choices": [{"message": message, "finish_reason": "stop"}],
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
