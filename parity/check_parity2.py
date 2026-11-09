#!/usr/bin/env python3
"""Paritas Fase 2: memory + recall semantik Node <-> Rust di DB bersama.

Alur:
1. Register via Node (fakta benih ter-embedding oleh Node).
2. Chat via Node -> extractFacts menulis fakta kopi (vektor Node).
3. Chat via Rust -> system prompt yang terekam harus memuat fakta kopi
   (recall Rust membaca vektor Node).
4. Chat via Rust -> fakta mendaki tertulis (vektor Rust).
5. Chat via Node -> prompt Node harus memuat fakta mendaki (arah sebaliknya).
6. Banding numerik: vektor teks identik dari kedua engine, cosine > 0.99.
7. /api/memories: daftar, settings, hapus — konsisten antar server.
8. Structured capture dari chat Rust terlihat via GET Node.
"""
import glob
import json
import math
import os
import sqlite3
import sys
import time
import urllib.request

NODE = "http://127.0.0.1:3101"
RUST = "http://127.0.0.1:3102"
DB = os.environ.get("PARITY_DB", "/tmp/yuki-parity2/yuki.db")
REQ_DIR = os.environ.get("MOCK_REQ_DIR", "/tmp/yuki-parity2/requests")

results = []


def check(name, ok, detail=""):
    results.append((name, ok, detail))
    print(("PASS" if ok else "FAIL"), name, detail)


def req(base, path, method="GET", body=None, token=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(base + path, data=data, method=method)
    if data:
        r.add_header("Content-Type", "application/json")
    if token:
        r.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode() or "{}")


def db_query(sql, args=()):
    conn = sqlite3.connect(DB)
    rows = conn.execute(sql, args).fetchall()
    conn.close()
    return rows


def wait_fact(text_like, vec_not_null=True, timeout=180):
    deadline = time.time() + timeout
    while time.time() < deadline:
        rows = db_query("SELECT text, vec FROM facts WHERE text LIKE ?", (f"%{text_like}%",))
        for text, vec in rows:
            if vec_not_null and vec:
                return text, json.loads(vec)
            if not vec_not_null:
                return text, None
        time.sleep(2)
    return None, None


def recorded_system_prompts_since(marker_count):
    files = sorted(glob.glob(os.path.join(REQ_DIR, "req_*.json")))
    prompts = []
    for path in files[marker_count:]:
        with open(path) as fh:
            body = json.load(fh)
        messages = body.get("messages", [])
        if messages and messages[0].get("role") == "system":
            prompts.append(messages[0].get("content", ""))
    return prompts


def req_count():
    return len(glob.glob(os.path.join(REQ_DIR, "req_*.json")))


def chat(base, token, text):
    return req(base, "/api/chat", "POST", {"messages": [{"role": "user", "content": text}]}, token)


def main():
    # 1. Register via Node
    status, reg = req(NODE, "/api/register", "POST", {"username": "Uji"})
    check("register via Node 200", status == 200, str(status))
    token = reg.get("sessionToken", "")
    user_id = reg.get("userId", "")
    _, seed_vec = wait_fact("Nama user adalah Uji")
    check("fakta benih ter-embedding oleh Node", seed_vec is not None and len(seed_vec) == 384,
          f"dim={len(seed_vec) if seed_vec else 0}")

    # 2. Chat via Node: fakta kopi tertulis dengan vektor Node
    before = req_count()
    status, chatres = chat(NODE, token, "Aku suka kopi pahit banget dan aku tinggal di Semarang")
    check("chat Node 200", status == 200, str(status))
    _, kopi_vec_node = wait_fact("kopi pahit")
    check("fakta kopi tertulis (vektor Node)", kopi_vec_node is not None)

    # 3. Chat via Rust: recall Rust membaca vektor Node
    before = req_count()
    status, chatres = chat(RUST, token, "Kopi apa yang aku suka?")
    check("chat Rust 200 + mood senang", status == 200 and chatres.get("mood") == "senang",
          f"{status} {chatres.get('mood')}")
    prompts = recorded_system_prompts_since(before)
    joined = "\n".join(prompts)
    check("prompt Rust memuat fakta kopi dari Node", "kopi pahit" in joined)
    check("prompt Rust memuat fakta Semarang", "Semarang" in joined)

    # 4. Chat via Rust: fakta mendaki tertulis dengan vektor Rust
    status, _ = chat(RUST, token, "Aku juga suka mendaki gunung tiap bulan")
    check("chat Rust (mendaki) 200", status == 200, str(status))
    _, mendaki_vec_rust = wait_fact("mendaki gunung")
    check("fakta mendaki tertulis (vektor Rust)", mendaki_vec_rust is not None)

    # 5. Chat via Node: recall Node membaca vektor Rust
    before = req_count()
    status, _ = chat(NODE, token, "Aku suka mendaki apa?")
    check("chat Node (tanya mendaki) 200", status == 200, str(status))
    prompts = recorded_system_prompts_since(before)
    joined = "\n".join(prompts)
    check("prompt Node memuat fakta mendaki dari Rust", "mendaki gunung" in joined)

    # 6. Banding numerik vektor untuk teks identik
    if mendaki_vec_rust:
        db_query("DELETE FROM facts WHERE text LIKE '%mendaki gunung%'")
        chat(NODE, token, "Ngomong-ngomong aku suka mendaki gunung")
        _, mendaki_vec_node = wait_fact("mendaki gunung")
        if mendaki_vec_node:
            dot = sum(a * b for a, b in zip(mendaki_vec_rust, mendaki_vec_node))
            check("cosine vektor Rust vs Node (teks sama) > 0.99", dot > 0.99, f"cos={dot:.4f}")
        else:
            check("cosine vektor Rust vs Node (teks sama) > 0.99", False, "vektor Node tidak tertulis ulang")
    else:
        check("cosine vektor Rust vs Node (teks sama) > 0.99", False, "vektor Rust kosong")

    # 7. /api/memories lintas server
    s_rust, mem_rust = req(RUST, "/api/memories", token=token)
    s_node, mem_node = req(NODE, "/api/memories", token=token)
    facts_rust = {f["content"] for f in mem_rust.get("facts", [])}
    facts_node = {f["content"] for f in mem_node.get("facts", [])}
    check("GET /api/memories 200 di kedua server", s_rust == 200 and s_node == 200, f"{s_rust}/{s_node}")
    check("daftar fakta sama antar server", facts_rust == facts_node,
          f"rust={len(facts_rust)} node={len(facts_node)}")
    check("fakta kopi tampil di menu", any("kopi pahit" in f for f in facts_rust))

    status, setres = req(RUST, "/api/memories/settings", "PUT", {"autoCapture": False}, token)
    check("PUT settings via Rust", status == 200 and setres["settings"]["autoCapture"] is False, str(status))
    _, mem_node2 = req(NODE, "/api/memories", token=token)
    check("settings terbaca Node sebagai false", mem_node2["settings"]["autoCapture"] is False)
    req(RUST, "/api/memories/settings", "PUT", {"autoCapture": True}, token)

    kopi_rows = db_query("SELECT rowid FROM facts WHERE text LIKE '%kopi pahit%'")
    if kopi_rows:
        status, _ = req(RUST, f"/api/memories/fact/{kopi_rows[0][0]}", "DELETE", token=token)
        _, mem_node3 = req(NODE, "/api/memories", token=token)
        tersisa = {f["content"] for f in mem_node3.get("facts", [])}
        check("DELETE fakta via Rust hilang di Node", status == 200 and not any("kopi pahit" in f for f in tersisa), str(status))
    else:
        check("DELETE fakta via Rust hilang di Node", False, "fakta kopi tidak ditemukan")

    # 8. Structured capture dari chat Rust. Node menyembunyikan structured
    # yang kontennya sama dengan agent_memories (tampil sebagai agent),
    # jadi kategori dicari di gabungan keduanya.
    chat(RUST, token, "Aku berjanji akan belajar besok pagi, ingatkan aku ya")
    time.sleep(1)
    _, mem_node4 = req(NODE, "/api/memories", token=token)
    cats = {s["category"] for s in mem_node4.get("structured", [])}
    cats |= {m["category"] for m in mem_node4.get("memories", [])}
    check("structured promise/open_loop tertangkap dari chat Rust",
          bool(cats & {"promise", "open_loop"}), f"cats={sorted(cats)}")

    # 9. Paritas recall pada query yang sama: MiniLM memberi skor > 0.25
    # bahkan untuk kalimat tak berkaitan (terukur dengan embedder Node
    # sendiri: 0.27-0.50), jadi yang dibandingkan adalah HIMPUNAN fakta
    # yang dipilih kedua engine untuk query identik — harus sama persis.
    def fact_section(prompt):
        marker = "Fakta spesifik relevan terdeteksi"
        if marker not in prompt:
            return set()
        tail = prompt.split(marker, 1)[1]
        lines = set()
        for line in tail.splitlines()[1:]:
            if line.startswith("- "):
                lines.add(line[2:].split(" [dicatat")[0])
            elif line.strip() == "":
                if lines:
                    break
            elif lines:
                break
        return lines

    before = req_count()
    status_r, _ = chat(RUST, token, "Ceritain resep rendang buat lebaran dong")
    facts_rust_q = set()
    for p in recorded_system_prompts_since(before):
        facts_rust_q |= fact_section(p)
    before = req_count()
    status_n, _ = chat(NODE, token, "Ceritain resep rendang buat lebaran dong")
    facts_node_q = set()
    for p in recorded_system_prompts_since(before):
        facts_node_q |= fact_section(p)
    check("chat rendang 200 di kedua server", status_r == 200 and status_n == 200,
          f"{status_r}/{status_n}")
    check("himpunan fakta recall sama untuk query identik",
          facts_rust_q == facts_node_q and len(facts_rust_q) > 0,
          f"rust={sorted(facts_rust_q)} node={sorted(facts_node_q)}")

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} PASS")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
