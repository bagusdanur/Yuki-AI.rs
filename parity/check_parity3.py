#!/usr/bin/env python3
"""Paritas Fase 3: scheduler + push + reminder Node <-> Rust di DB bersama.

Fase A: config push, validasi subscription (409/400), reminder dari chat
        kedua engine, agenda CRUD lintas server.
Fase B: task dibuat Node -> Node dimatikan -> Rust yang mengirim
        (pesan [YUKI_REMINDER] + Web Push terverifikasi kriptografis).
Fase C: task dibuat Rust -> Rust dimatikan -> Node yang mengirim.
"""
import glob
import json
import os
import signal
import sqlite3
import subprocess
import sys
import time
import urllib.request
import urllib.error

BASE = "/tmp/yuki-parity3"
DB = f"{BASE}/yuki.db"
REQ_DIR = f"{BASE}/requests"
PUSH_DIR = f"{BASE}/push"
NODE = "http://127.0.0.1:3101"
RUST = "http://127.0.0.1:3102"
PARITY = os.path.expanduser("~/workspace/yuki-ai-rust/parity")
ORIGINAL = os.path.expanduser("~/workspace/yuki-ai-rust/original")
RUST_BIN = os.path.expanduser("~/workspace/yuki-ai-rust/yuki-rs/target/release/yuki-rs")

results = []
procs = {}


def check(name, ok, detail=""):
    results.append((name, ok))
    print(("PASS" if ok else "FAIL"), name, detail, flush=True)


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
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {}


def db_query(sql, args=()):
    conn = sqlite3.connect(DB)
    rows = conn.execute(sql, args).fetchall()
    conn.close()
    return rows


def db_exec(sql, args=()):
    conn = sqlite3.connect(DB)
    conn.execute(sql, args)
    conn.commit()
    conn.close()


def wait_health(base, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(base + "/api/health", timeout=2) as resp:
                if resp.status == 200:
                    return True
        except Exception:
            time.sleep(0.5)
    return False


def start(name, cmd, env_extra, cwd):
    env = dict(os.environ)
    env.update(env_extra)
    log = open(f"{BASE}/{name}.log", "w")
    p = subprocess.Popen(cmd, env=env, cwd=cwd, stdout=log, stderr=subprocess.STDOUT)
    procs[name] = p
    return p


def stop(name):
    p = procs.get(name)
    if p and p.poll() is None:
        p.terminate()
        try:
            p.wait(timeout=8)
        except subprocess.TimeoutExpired:
            p.kill()


def server_env():
    keys = json.load(open(f"{BASE}/keys.json"))
    return {
        "AUTH_SECRET": "paritas-secret-123456",
        "MEMORY_DB": DB,
        "LLM_PROVIDER_CONFIG_FILE": f"{BASE}/llm-providers.json",
        "VAPID_PUBLIC_KEY": keys["vapidPublic"],
        "VAPID_PRIVATE_KEY": keys["vapidPrivate"],
        "HOST": "127.0.0.1",
    }


def start_node():
    env = server_env()
    env.update({
        "PORT": "3101",
        "YUKI_PROVIDER_STATS_FILE": f"{BASE}/node-stats.json",
        "AGENT_WORKSPACE_DIR": f"{BASE}/workspace",
        "NODE_EXTRA_CA_CERTS": f"{BASE}/ca.pem",
        "PATH": "/usr/bin:/opt/hatch-image/bin:" + os.environ.get("PATH", ""),
    })
    start("node", ["/usr/bin/node", "server.js"], env, ORIGINAL)
    return wait_health(NODE)


def start_rust():
    env = server_env()
    env.update({
        "PORT": "3102",
        "YUKI_PROVIDER_STATS_FILE": f"{BASE}/rust-stats.json",
        "YUKI_DIST_DIR": f"{ORIGINAL}/dist",
        "YUKI_PUBLIC_DIR": f"{ORIGINAL}/public",
        "YUKI_EXTRA_CA_CERT": f"{BASE}/ca.pem",
    })
    start("rust", [RUST_BIN], env, BASE)
    return wait_health(RUST)


def chat(base, token, text):
    return req(base, "/api/chat", "POST", {"messages": [{"role": "user", "content": text}]}, token)


def req_count():
    return len(glob.glob(os.path.join(REQ_DIR, "req_*.json")))


def prompts_since(marker):
    files = sorted(glob.glob(os.path.join(REQ_DIR, "req_*.json")))
    out = []
    for path in files[marker:]:
        body = json.load(open(path))
        msgs = body.get("messages", [])
        if msgs and msgs[0].get("role") == "system":
            out.append(msgs[0].get("content", ""))
    return "\n".join(out)


def agenda(base, token):
    _, body = req(base, "/api/agenda", token=token)
    return body


def wait_reminder_message(title_part, timeout=200):
    deadline = time.time() + timeout
    while time.time() < deadline:
        rows = db_query(
            "SELECT content FROM chat_history WHERE role='assistant' AND content LIKE '[YUKI_REMINDER]%'"
        )
        if any(title_part in r[0] for r in rows):
            return True
        time.sleep(3)
    return False


def verify_push_file(path):
    out = subprocess.run(
        ["/usr/bin/node", f"{PARITY}/verify_push.mjs", path, f"{BASE}/keys.json"],
        capture_output=True, text=True, timeout=30,
    )
    try:
        return json.loads(out.stdout.strip().splitlines()[-1])
    except Exception:
        return {"ok": False, "error": out.stdout + out.stderr}


def make_certs():
    """CA lokal + sertifikat server untuk fcm.googleapis.com (SAN)."""
    def run(*args):
        subprocess.run(["openssl", *args], check=True, capture_output=True)
    run("req", "-x509", "-newkey", "rsa:2048", "-keyout", f"{BASE}/ca-key.pem",
        "-out", f"{BASE}/ca.pem", "-days", "2", "-nodes", "-subj", "/CN=Yuki Parity CA")
    run("req", "-newkey", "rsa:2048", "-keyout", f"{BASE}/srv-key.pem",
        "-out", f"{BASE}/srv.csr", "-nodes", "-subj", "/CN=localhost")
    with open(f"{BASE}/srv-ext.cnf", "w") as fh:
        fh.write("subjectAltName=DNS:localhost\n")
    run("x509", "-req", "-in", f"{BASE}/srv.csr", "-CA", f"{BASE}/ca.pem",
        "-CAkey", f"{BASE}/ca-key.pem", "-CAcreateserial", "-out", f"{BASE}/srv.pem",
        "-days", "2", "-extfile", f"{BASE}/srv-ext.cnf")




def hosts_add():
    pass


def hosts_remove():
    pass


def main():
    os.makedirs(REQ_DIR, exist_ok=True)
    os.makedirs(PUSH_DIR, exist_ok=True)
    keys = json.loads(subprocess.run(
        ["/usr/bin/node", f"{PARITY}/gen_keys.mjs"], capture_output=True, text=True).stdout)
    json.dump(keys, open(f"{BASE}/keys.json", "w"))
    make_certs()
    hosts_add()
    env = dict(os.environ)
    env["MOCK_REQ_DIR"] = REQ_DIR
    start("mock", ["python3", f"{PARITY}/mock_provider2.py"], env, BASE)
    env2 = dict(os.environ)
    env2.update({
        "MOCK_PUSH_DIR": PUSH_DIR, "MOCK_PUSH_PORT": "8443",
        "MOCK_PUSH_CERT": f"{BASE}/srv.pem", "MOCK_PUSH_KEY": f"{BASE}/srv-key.pem",
    })
    start("mockpush", ["python3", f"{PARITY}/mock_push.py"], env2, BASE)
    time.sleep(1)
    assert start_node(), "node gagal start"
    assert start_rust(), "rust gagal start"

    # ===== Fase A =====
    _, reg = req(NODE, "/api/register", "POST", {"username": "Uji"})
    token = reg["sessionToken"]
    user_id = reg["userId"]
    _, reg_b = req(RUST, "/api/register", "POST", {"username": "Bima"})
    token_b = reg_b["sessionToken"]

    s_r, cfg_r = req(RUST, "/api/push/config", token=token)
    s_n, cfg_n = req(NODE, "/api/push/config", token=token)
    check("push config enabled & kunci sama",
          s_r == 200 and s_n == 200 and cfg_r["enabled"] and cfg_r["publicKey"] == keys["vapidPublic"] == cfg_n["publicKey"],
          f"{cfg_r.get('enabled')}/{cfg_n.get('enabled')}")

    fcm_sub = {
        "endpoint": "https://fcm.googleapis.com/fcm/send/parity-test-token",
        "keys": {"p256dh": keys["subscription"]["p256dh"], "auth": keys["subscription"]["auth"]},
    }
    s, _ = req(RUST, "/api/push/subscriptions", "POST", {"subscription": fcm_sub}, token)
    check("POST subscription FCM via Rust 200", s == 200, str(s))
    s, _ = req(RUST, "/api/push/subscriptions", "POST", {"subscription": fcm_sub}, token_b)
    check("POST subscription sama oleh user lain -> 409", s == 409, str(s))
    s, _ = req(NODE, "/api/push/subscriptions", "POST",
               {"subscription": {"endpoint": "https://evil.example.com/x",
                                 "keys": fcm_sub["keys"]}}, token)
    check("POST subscription host asing via Node -> 400", s == 400, str(s))
    s, _ = req(RUST, "/api/push/subscriptions", "DELETE", {"endpoint": fcm_sub["endpoint"]}, token)
    check("DELETE subscription via Rust 200", s == 200, str(s))

    before = req_count()
    s, _ = chat(NODE, token, "ingatkan aku minum obat besok jam 7 pagi")
    prompt = prompts_since(before)
    check("chat Node buat pengingat: konteks BERHASIL", s == 200 and "Pengingat BERHASIL dibuat" in prompt, str(s))
    ag = agenda(RUST, token)
    obat = [t for t in ag["tasks"] if "minum obat" in t["title"]]
    check("task obat tampil di agenda Rust", len(obat) == 1, f"{len(obat)}")
    if obat:
        nxt = obat[0]["next_run_at_utc"] or ""
        check("next run obat besok 07:00 WIB (00:00 UTC)", nxt.endswith("T00:00:00.000Z") or "T00:00" in nxt, nxt)
        tid = obat[0]["id"]
        s, res = req(RUST, f"/api/agenda/{tid}", "PATCH", {"action": "pause"}, token)
        ag2 = agenda(NODE, token)
        st = [t["status"] for t in ag2["tasks"] if t["id"] == tid]
        check("pause via Rust terbaca Node", s == 200 and st == ["paused"], f"{s} {st}")
        s, res = req(RUST, f"/api/agenda/{tid}", "PATCH", {"action": "resume"}, token)
        ag3 = agenda(NODE, token)
        st = [t["status"] for t in ag3["tasks"] if t["id"] == tid]
        check("resume via Rust terbaca Node", s == 200 and st == ["active"], f"{s} {st}")
        s, res = req(RUST, f"/api/agenda/{tid}", "PATCH", {"action": "cancel"}, token)
        ag4 = agenda(NODE, token)
        check("cancel via Rust hilang dari agenda Node",
              s == 200 and all(t["id"] != tid for t in ag4["tasks"]), str(s))

    # ===== Fase B: Node membuat, Rust mengirim =====
    before = req_count()
    s, _ = chat(NODE, token, "ingatkan aku minum vitamin 1 menit lagi")
    prompt = prompts_since(before)
    check("chat Node buat vitamin: konteks BERHASIL", s == 200 and "Pengingat BERHASIL dibuat" in prompt, str(s))
    # Subscription mock HTTPS (ditanam langsung; allowlist API memang
    # menolak selain host resmi — endpoint ini memakai nama host FCM yang
    # diarahkan /etc/hosts ke mock lokal ber-TLS CA uji).
    local_sub = {
        "endpoint": "https://localhost:8443/push/uji",
        "expirationTime": None,
        "keys": {"p256dh": keys["subscription"]["p256dh"], "auth": keys["subscription"]["auth"]},
    }
    db_exec(
        "INSERT OR REPLACE INTO push_subscriptions (endpoint, user_id, subscription_json, failure_count) VALUES (?, ?, ?, 0)",
        (local_sub["endpoint"], user_id, json.dumps(local_sub)),
    )
    stop("node")
    ok = wait_reminder_message("minum vitamin", timeout=210)
    check("Rust mengirim pengingat vitamin (pesan tersimpan)", ok)
    rows = db_query("SELECT status FROM scheduled_occurrences")
    check("occurrence vitamin delivered", any(r[0] == "delivered" for r in rows), str(rows))
    _, rem = req(RUST, "/api/chat/reminders", token=token)
    check("GET /api/chat/reminders via Rust (awalan terpotong)",
          any("minum vitamin" in r["content"] and not r["content"].startswith("[YUKI_REMINDER]")
              for r in rem.get("reminders", [])),
          str(len(rem.get("reminders", []))))
    push_files = sorted(glob.glob(os.path.join(PUSH_DIR, "push_*.json")))
    if push_files:
        v = verify_push_file(push_files[-1])
        check("push Rust: JWT valid + dekripsi cocok",
              v.get("ok") and v.get("sigOk") and "minum vitamin" in json.dumps(v.get("payload", {})),
              json.dumps(v)[:160])
    else:
        check("push Rust: JWT valid + dekripsi cocok", False, "tidak ada rekaman push")

    # ===== Fase C: Rust membuat, Node mengirim =====
    # Node menjadwalkan job hanya dari restore saat boot (tidak polling DB),
    # jadi urutannya: buat task via Rust -> matikan Rust -> nyalakan Node;
    # restore Node memulihkan occurrence run_once yang terlewat lalu mengirim.
    before = req_count()
    s, _ = chat(RUST, token, "ingatkan aku stretching 1 menit lagi")
    prompt = prompts_since(before)
    check("chat Rust buat stretching: konteks BERHASIL", s == 200 and "Pengingat BERHASIL dibuat" in prompt, str(s))
    n_push_before = len(glob.glob(os.path.join(PUSH_DIR, "push_*.json")))
    stop("rust")
    print("menunggu jadwal stretching jatuh tempo sebelum Node dinyalakan...", flush=True)
    time.sleep(62)
    assert start_node(), "node gagal restart"
    ok = wait_reminder_message("stretching", timeout=120)
    check("Node mengirim pengingat stretching (pesan tersimpan)", ok)
    push_files = sorted(glob.glob(os.path.join(PUSH_DIR, "push_*.json")))
    if len(push_files) > n_push_before:
        v = verify_push_file(push_files[-1])
        check("push Node: JWT valid + dekripsi cocok",
              v.get("ok") and v.get("sigOk") and "stretching" in json.dumps(v.get("payload", {})),
              json.dumps(v)[:160])
    else:
        check("push Node: JWT valid + dekripsi cocok", False, "tidak ada rekaman push baru")
    _, rem = req(NODE, "/api/chat/reminders", token=token)
    titles = [r["content"] for r in rem.get("reminders", [])]
    check("kedua pengingat tampil di /api/chat/reminders Node",
          any("minum vitamin" in t for t in titles) and any("stretching" in t for t in titles),
          str(len(titles)))

    for name in list(procs):
        stop(name)
    hosts_remove()
    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} PASS")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
