#!/usr/bin/env python3
"""Paritas Fase 8: SSE streaming (delta/done/error, failover, potong),
sandbox QuickJS vs child-process Node, cek sintaks QuickJS/tree-sitter
vs V8/stripTypeScriptTypes, dan guard SSRF browser. Bukti hasil tool
agent dibaca dari request yang dicatat mock (seperti Fase 4)."""
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import time

ROOT = os.path.expanduser("~/workspace/yuki-ai-rust")
NODE_DIR = os.path.join(ROOT, "original")
RUST_BIN = os.path.join(ROOT, "yuki-rs/target/release/yuki-rs")
BASE = "/tmp/yuki-parity8"
REQ_DIR = os.path.join(BASE, "requests")
CHROME = os.path.join(ROOT, "chrome-for-testing/chrome-linux64/chrome")

RESULTS = []


def check(name, cond, detail=""):
    RESULTS.append((name, bool(cond)))
    line = ("PASS " if cond else "FAIL ") + name
    if detail and not cond:
        line += f" — {detail}"
    print(line, flush=True)


def req(port, method, path, body=None, headers=None, timeout=60):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    h = {"Content-Type": "application/json"}
    if headers:
        h.update(headers)
    conn.request(method, path, json.dumps(body) if body is not None else None, h)
    res = conn.getresponse()
    raw = res.read()
    conn.close()
    try:
        parsed = json.loads(raw or b"null")
    except Exception:
        parsed = None
    return res.status, {k.lower(): v for k, v in res.getheaders()}, parsed, raw


def sse_chat(port, token, payload, timeout=60):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    conn.request(
        "POST",
        "/api/chat",
        json.dumps(payload),
        {"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    )
    res = conn.getresponse()
    raw = res.read().decode("utf-8", "replace")
    headers = {k.lower(): v for k, v in res.getheaders()}
    conn.close()
    events = []
    for block in raw.split("\n\n"):
        if not block.strip():
            continue
        ev, data = None, ""
        for line in block.split("\n"):
            if line.startswith("event:"):
                ev = line[6:].strip()
            elif line.startswith("data:"):
                data += line[5:].strip()
        try:
            parsed = json.loads(data) if data else {}
        except Exception:
            parsed = {"_raw": data}
        events.append((ev, parsed))
    return res.status, headers, events


def wait_health(port, tries=60):
    for _ in range(tries):
        try:
            s, _, body, _ = req(port, "GET", "/api/health", timeout=3)
            if s == 200 and body and body.get("status") == "ok":
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False


def wait_port(port, tries=40):
    for _ in range(tries):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return True
        except Exception:
            time.sleep(0.25)
    return False


def req_count():
    if not os.path.isdir(REQ_DIR):
        return 0
    return len([f for f in os.listdir(REQ_DIR) if f.startswith("req_")])


def tool_contents_since(start):
    """Gabungan semua content pesan role=tool dari request setelah start."""
    out = []
    files = sorted(
        f for f in os.listdir(REQ_DIR) if f.startswith("req_")
    )[start:]
    for name in files:
        try:
            body = json.load(open(os.path.join(REQ_DIR, name)))
        except Exception:
            continue
        for msg in body.get("messages", []):
            if msg.get("role") == "tool":
                out.append(msg.get("content") or "")
    return "\n".join(out)


def agent_chat(port, token, text, request_id, approve=False):
    before = req_count()
    s, _, body, _ = req(
        port,
        "POST",
        "/api/chat",
        {
            "messages": [{"role": "user", "content": text}],
            "mode": "agent",
            "requestId": request_id,
        },
        {"Authorization": f"Bearer {token}"},
        timeout=90,
    )
    # run_javascript_code termasuk ALWAYS_APPROVAL_TOOLS: setujui
    # checkpoint-nya agar tool benar-benar dieksekusi (alur Fase 4).
    approval = None
    if approve and body and body.get("awaitingApproval"):
        approval_id = body["awaitingApproval"].get("workflowId") or body[
            "awaitingApproval"
        ].get("id")
        s2, _, body2, _ = req(
            port,
            "POST",
            f"/api/agent/approvals/{approval_id}",
            {"decision": "approve"},
            {"Authorization": f"Bearer {token}"},
            timeout=90,
        )
        approval = (s2, body2)
        body = body2
    return s, body, tool_contents_since(before), approval


def chat_payload(text, **extra):
    payload = {"messages": [{"role": "user", "content": text}]}
    payload.update(extra)
    return payload


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(REQ_DIR, exist_ok=True)
    providers = {
        slot: {
            "id": slot,
            "name": f"Mock {slot}",
            "baseURL": "http://127.0.0.1:3201/v1",
            "model": f"mock-{slot}",
            "apiKey": "mock-key",
            "inputCostPerMillion": 0,
            "outputCostPerMillion": 0,
        }
        for slot in ("primary", "backup")
    }
    with open(f"{BASE}/llm-providers.json", "w") as fh:
        json.dump(providers, fh)

    env_common = {
        **os.environ,
        "AUTH_SECRET": "paritas-secret-123456",
        "MEMORY_DB": f"{BASE}/yuki.db",
        "ADMIN_PASSWORD_FILE": f"{BASE}/admin-password.json",
        "LLM_PROVIDER_CONFIG_FILE": f"{BASE}/llm-providers.json",
        "YUKI_PROVIDER_STATS_FILE": f"{BASE}/provider-stats.json",
        "YUKI_AGENT_WORKSPACE": f"{BASE}/agent-workspace",
        "AGENT_WORKSPACE_DIR": f"{BASE}/agent-workspace",
        "CHROMIUM_PATH": CHROME,
        "HOST": "127.0.0.1",
    }
    procs = []
    try:
        mock_env = {**os.environ, "MOCK_REQ_DIR": REQ_DIR}
        procs.append(subprocess.Popen(
            [sys.executable, os.path.join(ROOT, "parity/mock_provider8.py"), "3201"],
            env=mock_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        assert wait_port(3201), "mock tidak naik"

        procs.append(subprocess.Popen(
            ["/usr/bin/node", "server.js"], cwd=NODE_DIR,
            env={**env_common, "PORT": "3101"},
            stdout=open(f"{BASE}/node.log", "w"), stderr=subprocess.STDOUT))
        procs.append(subprocess.Popen(
            [RUST_BIN], cwd=os.path.join(ROOT, "yuki-rs"),
            env={**env_common, "PORT": "3102"},
            stdout=open(f"{BASE}/rust.log", "w"), stderr=subprocess.STDOUT))
        assert wait_health(3101), "Node tidak sehat"
        assert wait_health(3102), "Rust tidak sehat"

        # Dua user: A untuk streaming, B untuk agent (jaga-jaga limiter).
        _, _, reg_a, _ = req(3101, "POST", "/api/register", {"username": "sari8a"})
        _, _, reg_b, _ = req(3101, "POST", "/api/register", {"username": "sari8b"})
        token_a = reg_a["sessionToken"]
        token_b = reg_b["sessionToken"]
        auth_a = {"Authorization": f"Bearer {token_a}"}
        auth_b = {"Authorization": f"Bearer {token_b}"}

        EXPECTED_DELTAS = "Halo juga. Aku denger kok, santai aja ya.\n"
        EXPECTED_REPLY = "Halo juga. Aku denger kok, santai aja ya."

        # ===== Streaming dasar =====
        streams = {}
        for label, port in (("Node", 3101), ("Rust", 3102)):
            status, headers, events = sse_chat(
                port, token_a, chat_payload("STREAMOK halo yuki", stream=True))
            kinds = [e for e, _ in events]
            deltas = "".join(d.get("text", "") for e, d in events if e == "delta")
            done = next((d for e, d in events if e == "done"), {})
            streams[label] = (deltas, done)
            check(f"{label} stream: content-type SSE",
                  "text/event-stream" in headers.get("content-type", ""))
            check(f"{label} stream: urutan delta* lalu done",
                  kinds[-1:] == ["done"] and all(k == "delta" for k in kinds[:-1]) and len(kinds) > 2,
                  str(kinds))
            check(f"{label} stream: delta tergabung utuh, fragmen emosi tertahan",
                  deltas == EXPECTED_DELTAS and "[emosi" not in deltas, repr(deltas))
            check(f"{label} stream: done.reply final tersanitasi",
                  done.get("reply") == EXPECTED_REPLY, repr(done.get("reply")))
            check(f"{label} stream: done.mood senang + messageId",
                  done.get("mood") == "senang" and (done.get("messageId") or 0) > 0,
                  json.dumps(done)[:200])
        check("delta Node == delta Rust", streams["Node"][0] == streams["Rust"][0])
        check("done Node == done Rust (reply+mood+bond)",
              all(streams["Node"][1].get(k) == streams["Rust"][1].get(k)
                  for k in ("reply", "mood", "bond")))

        # ===== Non-stream tetap JSON & sama =====
        for label, port in (("Node", 3101), ("Rust", 3102)):
            s, headers, body, _ = req(port, "POST", "/api/chat",
                                      chat_payload("STREAMOK halo lagi"), auth_a)
            check(f"{label} non-stream: JSON reply sama dengan done stream",
                  s == 200 and body.get("reply") == EXPECTED_REPLY
                  and "application/json" in headers.get("content-type", ""),
                  f"{s} {body}")

        # ===== Gerbang streaming: agent & idle tetap JSON =====
        for label, port in (("Node", 3101), ("Rust", 3102)):
            s, headers, body, _ = req(
                port, "POST", "/api/chat",
                chat_payload("STREAMOK mode agent", stream=True, mode="agent",
                             requestId=f"gate-{label}-8"),
                auth_a)
            check(f"{label} stream+agent -> JSON, bukan SSE",
                  "application/json" in headers.get("content-type", ""),
                  headers.get("content-type", ""))
            s, headers, body, _ = req(
                port, "POST", "/api/chat",
                chat_payload("STREAMOK idle", stream=True, isIdle=True), auth_a)
            check(f"{label} stream+idle -> JSON, bukan SSE",
                  "application/json" in headers.get("content-type", ""),
                  headers.get("content-type", ""))

        # ===== Failover sebelum delta pertama =====
        for label, port in (("Node", 3101), ("Rust", 3102)):
            status, headers, events = sse_chat(
                port, token_a, chat_payload("STREAMFAILBACKUP halo", stream=True))
            deltas = "".join(d.get("text", "") for e, d in events if e == "delta")
            done = next((d for e, d in events if e == "done"), {})
            check(f"{label} stream failover: backup mengalir + done",
                  deltas == EXPECTED_DELTAS and done.get("reply") == EXPECTED_REPLY,
                  repr(deltas))

        # ===== Terputus di tengah -> parsial dipakai =====
        for label, port in (("Node", 3101), ("Rust", 3102)):
            status, headers, events = sse_chat(
                port, token_a, chat_payload("STREAMCUT halo", stream=True))
            done = next((d for e, d in events if e == "done"), None)
            check(f"{label} stream terputus: done dengan teks parsial",
                  done is not None and done.get("reply") == "Halo juga.",
                  json.dumps(events)[:300])

        # ===== Semua provider mati -> event error (Rust) =====
        status, headers, events = sse_chat(
            3102, token_a, chat_payload("STREAMDEAD halo", stream=True))
        check("Rust stream mati total: satu event error generik",
              len(events) == 1 and events[0][0] == "error"
              and events[0][1].get("error") == "Terjadi kesalahan internal pada server.",
              json.dumps(events)[:200])

        # ===== Sandbox JS via agent =====
        s, body, tools, _appr = agent_chat(3101, token_b, "JSTEST1 jalankan kode ini di scratchpad", "p8-node-js1", approve=True)
        check("Node JSTEST1: success + result 42 + log",
              '"success":true' in tools.replace(" ", "") or '"success": true' in tools,
              tools[:200])
        check("Node JSTEST1: result 42 & logs halo",
              "42" in tools and "halo dari js" in tools, tools[:300])
        check("Node JSTEST1: label child-process", "child-process" in tools)
        s, body, tools, _appr = agent_chat(3102, token_b, "JSTEST1 jalankan kode ini di scratchpad", "p8-rust-js1", approve=True)
        check("Rust JSTEST1: result 42 & logs halo",
              "42" in tools and "halo dari js" in tools, tools[:300])
        check("Rust JSTEST1: label quickjs", "quickjs" in tools, tools[:200])

        s, body, tools, _appr = agent_chat(3101, token_b, "JSTEST2 jalankan kode ini di scratchpad", "p8-node-js2", approve=True)
        check("Node JSTEST2: timer resolve", "timer-ok" in tools, tools[:200])
        s, body, tools, _appr = agent_chat(3102, token_b, "JSTEST2 jalankan kode ini di scratchpad", "p8-rust-js2", approve=True)
        check("Rust JSTEST2: setTimeout + await jalan", "timer-ok" in tools, tools[:200])

        s, body, tools, appr = agent_chat(3101, token_b, "JSTEST3 jalankan kode ini di scratchpad", "p8-node-js3", approve=True)
        check("Node JSTEST3: throw -> approval 400 failed + 'ledak'",
              appr is not None and appr[0] == 400 and appr[1].get("state") == "failed"
              and "ledak" in (appr[1].get("error") or ""),
              str(appr)[:250])
        s, body, tools, appr = agent_chat(3102, token_b, "JSTEST3 jalankan kode ini di scratchpad", "p8-rust-js3", approve=True)
        check("Rust JSTEST3: throw -> approval 400 failed + 'ledak'",
              appr is not None and appr[0] == 400 and appr[1].get("state") == "failed"
              and "ledak" in (appr[1].get("error") or ""),
              str(appr)[:250])

        # Node: proses mati oleh prlimit CPU 4 dtk (kode null) sebelum
        # timeout dinding 5 dtk-nya; Rust: interupsi QuickJS di 5 dtk
        # dengan pesan timeout. Kontrak luarnya sama: 400 + failed.
        s, body, tools, appr = agent_chat(3101, token_b, "JSTEST4 jalankan kode ini di scratchpad", "p8-node-js4", approve=True)
        check("Node JSTEST4: loop abadi -> approval 400 failed",
              appr is not None and appr[0] == 400 and appr[1].get("state") == "failed"
              and "Error saat eksekusi kode" in (appr[1].get("error") or ""),
              str(appr)[:250])
        s, body, tools, appr = agent_chat(3102, token_b, "JSTEST4 jalankan kode ini di scratchpad", "p8-rust-js4", approve=True)
        check("Rust JSTEST4: loop abadi -> approval 400 failed + pesan timeout 5 dtk",
              appr is not None and appr[0] == 400 and appr[1].get("state") == "failed"
              and "melebihi batas 5 detik" in (appr[1].get("error") or ""),
              str(appr)[:250])

        # ===== Cek sintaks via agent =====
        for marker, expect_valid, label in (
            ("SYNJSBAD", False, "JS rusak"), ("SYNJSOK", True, "JS valid"),
            ("SYNTSBAD", False, "TS rusak"), ("SYNTSOK", True, "TS valid"),
        ):
            for engine, port in (("Node", 3101), ("Rust", 3102)):
                s, body, tools, _appr = agent_chat(
                    port, token_b, f"{marker} cek sintaks kode " + ("typescript" if marker.startswith("SYNTS") else "javascript"), f"p8-{engine}-{marker}")
                needle = '"valid":true' if expect_valid else '"valid":false'
                needle_sp = '"valid": true' if expect_valid else '"valid": false'
                check(f"{engine} sintaks {label}: valid={expect_valid}",
                      needle in tools.replace(" ", "") or needle_sp in tools,
                      tools[:200])

        # ===== Browser: guard SSRF sama di kedua mesin =====
        for engine, port in (("Node", 3101), ("Rust", 3102)):
            s, body, tools, _appr = agent_chat(
                port, token_b, "BROWSESSRF tolong browse_page alamat ini", f"p8-{engine}-ssrf")
            check(f"{engine} browse_page loopback ditolak SSRF",
                  "[Keamanan Jaringan]" in tools, tools[:250])

        # ===== STREAMDEAD di Node = crash upstream (PALING AKHIR) =====
        # server.js:1453 memakai sseSend di blok catch, padahal const
        # sseSend didefinisikan DI DALAM try -> ReferenceError -> proses
        # Node mati saat stream gagal total. Rust mengirim event error
        # sesuai maksud kode itu. Diuji terakhir karena Node-nya tewas.
        try:
            status, headers, events = sse_chat(
                3101, token_a, chat_payload("STREAMDEAD halo", stream=True),
                timeout=20)
            node_dead_outcome = ("event", events)
        except Exception as exc:  # koneksi putus di tengah chunk
            node_dead_outcome = ("crash", str(exc))
        check("Node stream mati total: [divergensi upstream] crash sseSend (server.js:1453)",
              node_dead_outcome[0] == "crash"
              or (node_dead_outcome[0] == "event" and len(node_dead_outcome[1]) == 1
                  and node_dead_outcome[1][0][0] == "error"),
              str(node_dead_outcome)[:200])

    finally:
        for proc in procs:
            try:
                proc.terminate()
            except Exception:
                pass
        time.sleep(1)
        for proc in procs:
            try:
                proc.kill()
            except Exception:
                pass

    passed = sum(1 for _, ok in RESULTS if ok)
    print(f"\n=== Fase 8: {passed}/{len(RESULTS)} PASS ===")
    return 0 if passed == len(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
