#!/usr/bin/env python3
"""Paritas Fase 5: layanan pinggiran (emotion, games, comics, ip-block,
admin, feedback, hapus akun, analyze-url) Node <-> Rust.

DB bersama + workspace bersama + mock provider deterministik.
Token admin Rust dipakai lintas ke Node (AUTH_SECRET sama) — login
admin dibatasi 5/15 mnt per bucket bersama, jadi login dijaga hemat.
"""
import http.client
import json
import os
import subprocess
import time
import urllib.parse
import urllib.request
import urllib.error

BASE = "/tmp/yuki-parity5"
DB = f"{BASE}/yuki.db"
REQ_DIR = f"{BASE}/requests"
WORKSPACE = f"{BASE}/agent-workspace"
NODE = "http://127.0.0.1:3101"
RUST = "http://127.0.0.1:3102"
PARITY = os.path.expanduser("~/workspace/yuki-ai-rust/parity")
ORIGINAL = os.path.expanduser("~/workspace/yuki-ai-rust/original")
RUST_BIN = os.path.expanduser("~/workspace/yuki-ai-rust/yuki-rs/target/release/yuki-rs")
ADMIN_PW = "admin-paritas-5"
ADMIN_PW2 = "admin-baru-123"

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
        with urllib.request.urlopen(r, timeout=120) as resp:
            raw = resp.read().decode()
            return resp.status, (json.loads(raw) if raw else {})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {}


def login_admin(base, password):
    """POST /api/admin/login via http.client agar Set-Cookie terbaca."""
    conn = http.client.HTTPConnection("127.0.0.1", 3101 if base == NODE else 3102, timeout=30)
    conn.request("POST", "/api/admin/login", json.dumps({"password": password}),
                 {"Content-Type": "application/json"})
    resp = conn.getresponse()
    raw = resp.read().decode()
    cookie = resp.getheader("Set-Cookie") or ""
    conn.close()
    try:
        payload = json.loads(raw or "{}")
    except Exception:
        payload = {}
    token = ""
    for part in cookie.split(";"):
        part = part.strip()
        if part.startswith("yuki_admin_session="):
            token = part.split("=", 1)[1]
    return resp.status, payload, token


def wait_health(base, timeout=40):
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


def stop_all():
    for p in procs.values():
        if p.poll() is None:
            p.terminate()
            try:
                p.wait(timeout=8)
            except subprocess.TimeoutExpired:
                p.kill()


def server_env():
    return {
        "AUTH_SECRET": "paritas-secret-123456",
        "MEMORY_DB": DB,
        "LLM_PROVIDER_CONFIG_FILE": f"{BASE}/llm-providers.json",
        "YUKI_AGENT_WORKSPACE": WORKSPACE,
        "AGENT_WORKSPACE_DIR": WORKSPACE,
        "ADMIN_PASSWORD": ADMIN_PW,
        "ADMIN_PASSWORD_FILE": f"{BASE}/admin-password.json",
        "GAMES_ENABLED": "true",
        "GAMES_PUBLIC": "true",
        "HOST": "127.0.0.1",
    }


def start_node():
    env = server_env()
    env.update({
        "PORT": "3101",
        "YUKI_PROVIDER_STATS_FILE": f"{BASE}/node-stats.json",
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
    })
    start("rust", [RUST_BIN], env, BASE)
    return wait_health(RUST)


def chat(base, token, text):
    return req(base, "/api/chat", "POST",
               {"messages": [{"role": "user", "content": text}]}, token)


def main():
    os.makedirs(REQ_DIR, exist_ok=True)
    os.makedirs(WORKSPACE, exist_ok=True)
    with open(f"{WORKSPACE}/manual.txt", "w") as f:
        f.write("isi berkas manual paritas")
    providers = {
        "primary": {"id": "primary", "name": "Mock Primary", "baseURL": "http://127.0.0.1:3201/v1",
                    "model": "mock-model", "apiKey": "mock-key", "inputCostPerMillion": 0, "outputCostPerMillion": 0},
        "backup": {"id": "backup", "name": "Mock Backup", "baseURL": "http://127.0.0.1:3201/v1",
                   "model": "mock-model", "apiKey": "mock-key", "inputCostPerMillion": 0, "outputCostPerMillion": 0},
    }
    json.dump(providers, open(f"{BASE}/llm-providers.json", "w"))

    env = dict(os.environ)
    env["MOCK_REQ_DIR"] = REQ_DIR
    start("mock", ["python3", f"{PARITY}/mock_provider5.py"], env, BASE)
    time.sleep(1)
    assert start_node(), "node gagal start"
    assert start_rust(), "rust gagal start"

    _, reg_a = req(RUST, "/api/register", "POST", {"username": "ParitasLima"})
    token_a, user_a = reg_a["sessionToken"], reg_a["userId"]
    _, reg_b = req(NODE, "/api/register", "POST", {"username": "ParitasEnam"})
    token_b, user_b = reg_b["sessionToken"], reg_b["userId"]

    # ===== A. Emotion lintas engine =====
    # CATATAN: teks chat pertama sengaja TANPA kata romantis — di
    # emotion.js, rayuan ke "orang asing" (bond < 8) memicu bondDelta
    # -0.80 (penolakan tsundere) sehingga bond akhir 0 di KEDUA engine
    # (terverifikasi langsung pada class Emotion Node: teks romantis
    # -> bond 0, teks hangat biasa -> bond 0.4). Port Rust setia.
    s, _ = chat(RUST, token_a, "Makasih banyak ya Yuki, kamu hebat banget dan selalu bikin aku senang pas lagi capek gini")
    check("chat companion Rust sukses", s == 200, f"status={s}")
    s, rel_n = req(NODE, f"/api/relationship/{user_a}", token=token_a)
    check("bond hasil Rust terbaca Node (>0)",
          s == 200 and (rel_n.get("bondValue") or 0) > 0.2 and rel_n.get("bond") == "orang asing",
          f"status={s} bond={rel_n.get('bondValue')} nama={rel_n.get('bond')}")
    s, _ = chat(NODE, token_a, "Aku senang banget bisa ngobrol sama kamu lagi, kamu selalu bikin aku nyaman dan tenang")
    check("chat companion Node sukses", s == 200, f"status={s}")
    s, rel_r = req(RUST, f"/api/relationship/{user_a}", token=token_a)
    check("bond gabungan terbaca Rust + milestones daftar",
          s == 200 and (rel_r.get("bondValue") or 0) > 0.2 and isinstance(rel_r.get("milestones"), list),
          f"status={s} bond={rel_r.get('bondValue')}")
    s, rel_x = req(RUST, f"/api/relationship/{user_b}", token=token_a)
    check("relationship user lain 403 (Rust)",
          s == 403 and rel_x.get("error") == "Akses ditolak.", f"status={s}")
    s, rel_x = req(NODE, f"/api/relationship/{user_a}", token=token_b)
    check("relationship user lain 403 (Node)",
          s == 403 and rel_x.get("error") == "Akses ditolak.", f"status={s}")

    # ===== B. Feedback =====
    s, fb = req(RUST, "/api/feedback", "POST", {"messageId": 1, "rating": 1, "reason": "bagus"}, token_a)
    check("feedback rating 1 ok (Rust)", s == 200 and fb.get("ok") is True, f"status={s}")
    s, fb = req(NODE, "/api/feedback", "POST", {"messageId": 1, "rating": 5}, token_a)
    check("feedback rating 5 ditolak (Node)",
          s == 400 and fb.get("error") == "Feedback tidak valid.", f"status={s}")
    s, fb = req(RUST, "/api/feedback", "POST", {"messageId": 1, "rating": 0}, token_a)
    check("feedback rating 0 ditolak (Rust)",
          s == 400 and fb.get("error") == "Feedback tidak valid.", f"status={s}")

    # ===== C. Bookmark komik =====
    comic1 = {"title": "Komik Uji Satu", "url": "https://ryukomik.my.id/komik/komiku/uji-satu",
              "type": "Manga", "chapter": "Ch. 3", "score": "8.1", "image": "https://img/1.jpg"}
    s, bm = req(RUST, "/api/comics/bookmarks", "POST", {"comic": comic1}, token_a)
    check("bookmark simpan (Rust)",
          s == 200 and bm.get("ok") is True and len(bm.get("bookmarks", [])) == 1
          and bm["bookmarks"][0]["type"] == "Manga",
          f"status={s} bm={bm.get('bookmarks')}")
    s, bm = req(NODE, "/api/comics/bookmarks", token=token_a)
    check("bookmark terbaca Node",
          s == 200 and len(bm.get("bookmarks", [])) == 1
          and bm["bookmarks"][0]["title"] == "Komik Uji Satu",
          f"status={s}")
    comic2 = {"title": "Komik Uji Dua", "url": "https://ryukomik.my.id/komik/komiku/uji-dua", "type": "Manhwa"}
    s, bm = req(NODE, "/api/comics/bookmarks", "POST", comic2, token_a)
    check("bookmark simpan (Node, body langsung)",
          s == 200 and len(bm.get("bookmarks", [])) == 2, f"status={s}")
    s, bm = req(RUST, "/api/comics/bookmarks?url=" + urllib.parse.quote(comic1["url"], safe=""),
                "DELETE", token=token_a)
    check("bookmark hapus (Rust via query)",
          s == 200 and len(bm.get("bookmarks", [])) == 1
          and bm["bookmarks"][0]["title"] == "Komik Uji Dua", f"status={s}")
    s, bm = req(RUST, "/api/comics/bookmarks", "POST", {"comic": {"title": "Tanpa URL"}}, token_a)
    check("bookmark tanpa url 400 (Rust)",
          s == 400 and bm.get("error") == "Data komik tidak valid.", f"status={s}")
    s, bm = req(NODE, "/api/comics/bookmarks", "POST", {"comic": {"url": "https://x"}}, token_a)
    check("bookmark tanpa judul 400 (Node)",
          s == 400 and bm.get("error") == "Data komik tidak valid.", f"status={s}")

    # ===== D. Comic cover =====
    s, cv = req(RUST, "/api/comic-cover?url=" + urllib.parse.quote("https://example.com/manga/abc", safe=""))
    check("comic-cover pola asing -> kosong (Rust)",
          s == 200 and cv.get("image") == "", f"status={s} cv={cv}")
    s, cv = req(NODE, "/api/comic-cover?url=" + urllib.parse.quote("https://example.com/manga/abc", safe=""))
    check("comic-cover pola asing -> kosong (Node)",
          s == 200 and cv.get("image") == "", f"status={s} cv={cv}")

    # ===== E. Games =====
    s, st = req(RUST, "/api/game/status", token=token_a)
    check("status game eligible (Rust)", s == 200 and st.get("eligible") is True, f"status={s}")
    s, st = req(NODE, "/api/game/status", token=token_a)
    check("status game eligible (Node)", s == 200 and st.get("eligible") is True, f"status={s}")
    s, ln = req(RUST, "/api/game/ttt/line", "POST", {"event": "start"}, token_a)
    check("komentar ttt dari mock (Rust)",
          s == 200 and ln.get("line") == "Baru mulai saja sudah ribut.", f"status={s} line={ln}")
    s, nw = req(RUST, "/api/game/ttt/new", "POST", {}, token_a)
    check("ttt baru (Rust)",
          s == 200 and nw["game"]["status"] == "playing" and all(c == "" for c in nw["game"]["board"]),
          f"status={s}")
    s, gt = req(NODE, "/api/game/ttt", token=token_a)
    check("sesi ttt terbaca Node",
          s == 200 and gt["game"]["status"] == "playing" and gt["game"]["board"] == [""] * 9,
          f"status={s}")
    s, mv = req(NODE, "/api/game/ttt/move", "POST", {"cell": 4}, token_a)
    check("ttt move X=4 (Node, Yuki dari model)",
          s == 200 and mv.get("source") == "yuki" and mv.get("yukiMove") == 0
          and mv["game"]["board"][4] == "X" and mv["game"]["board"][0] == "O",
          f"status={s} mv={mv.get('yukiMove')} src={mv.get('source')}")
    s, mv = req(RUST, "/api/game/ttt/move", "POST", {"cell": 4}, token_a)
    check("ttt move kotak terisi 400 (Rust)",
          s == 400 and mv.get("error") == "Kotak itu sudah terisi." and "game" in mv, f"status={s}")
    s, mv = req(RUST, "/api/game/ttt/move", "POST", {"cell": 8}, token_a)
    check("ttt move X=8 (Rust, Yuki dari model)",
          s == 200 and mv.get("source") == "yuki" and mv.get("yukiMove") == 1
          and mv["game"]["board"][8] == "X" and mv["game"]["board"][1] == "O",
          f"status={s} mv={mv.get('yukiMove')}")
    s, gt = req(NODE, "/api/game/ttt", token=token_a)
    check("papan ttt konsisten di Node",
          s == 200 and gt["game"]["board"] == ["O", "O", "", "", "X", "", "", "", "X"],
          f"status={s} board={gt.get('game', {}).get('board')}")
    s, rp = req(RUST, "/api/game/rps/play", "POST", {"choice": "batu"}, token_a)
    check("rps batu vs kertas mock (Rust)",
          s == 200 and rp.get("yuki") == "kertas" and rp.get("hasil") == "yuki_win"
          and rp.get("source") == "yuki" and rp["stats"]["losses"] == 1,
          f"status={s} rp={rp.get('yuki')}/{rp.get('hasil')} stats={rp.get('stats')}")
    s, rg = req(NODE, "/api/game/rps", token=token_a)
    check("sesi rps terbaca Node",
          s == 200 and rg.get("last") == "yuki_win" and rg["stats"]["losses"] == 1,
          f"status={s} last={rg.get('last')}")
    s, rp = req(NODE, "/api/game/rps/play", "POST", {"choice": "api"}, token_a)
    check("rps pilihan invalid 400 (Node)",
          s == 400 and rp.get("error") == "Pilih batu, kertas, atau gunting.", f"status={s}")
    s, rp = req(RUST, "/api/game/rps/play", "POST", {"choice": "api"}, token_a)
    check("rps pilihan invalid 400 (Rust)",
          s == 400 and rp.get("error") == "Pilih batu, kertas, atau gunting.", f"status={s}")

    # ===== F. Admin =====
    s, _, _ = login_admin(RUST, "password-salah")
    check("login admin salah 401 (Rust)", s == 401, f"status={s}")
    s, payload, admin_token = login_admin(RUST, ADMIN_PW)
    check("login admin ok (Rust) + cookie",
          s == 200 and payload.get("expiresIn") == 43200 and bool(admin_token), f"status={s}")
    s, stats = req(NODE, "/api/admin/stats", token=admin_token)
    check("stats admin lintas token (Node)",
          s == 200 and stats.get("users", 0) >= 2 and "runtime" in stats and "providers" in stats
          and stats["runtime"]["requests"] > 0,
          f"status={s} users={stats.get('users')}")
    s, ov = req(RUST, "/api/admin/overview", token=admin_token)
    check("overview admin (Rust)",
          s == 200 and "database" in ov and "games" in ov and len(ov.get("chatTrend", [])) == 30
          and "providerHealth" in ov and "alerts" in ov,
          f"status={s} keys={sorted(ov.keys()) if isinstance(ov, dict) else ov}")
    s, pv = req(RUST, "/api/admin/providers", token=admin_token)
    check("providers GET (Rust)",
          s == 200 and pv["providers"]["primary"]["id"] == "primary"
          and pv["providers"]["primary"].get("hasApiKey") is True, f"status={s}")
    test_body = {"id": "primary", "name": "Mock Primary", "baseURL": "http://127.0.0.1:3201/v1",
                 "model": "mock-model", "apiKey": "mock-key"}
    s, pt = req(RUST, "/api/admin/providers/test", "POST", test_body, admin_token)
    check("providers test (Rust -> mock OK)",
          s == 200 and pt.get("ok") is True and pt.get("reply") == "OK", f"status={s} pt={pt}")
    s, pt = req(NODE, "/api/admin/providers/test", "POST", test_body, admin_token)
    check("providers test (Node -> mock OK)",
          s == 200 and pt.get("ok") is True and pt.get("reply") == "OK", f"status={s} pt={pt}")
    s, pv = req(NODE, "/api/admin/providers", token=admin_token)
    put_body = pv["providers"]
    put_body["backup"]["name"] = "Mock Backup Baru"
    s, pv2 = req(NODE, "/api/admin/providers", "PUT", put_body, admin_token)
    check("providers PUT (Node)",
          s == 200 and pv2.get("ok") is True and pv2["providers"]["backup"]["name"] == "Mock Backup Baru",
          f"status={s}")
    s, pv = req(RUST, "/api/admin/providers", token=admin_token)
    check("providers PUT terbaca Rust",
          s == 200 and pv["providers"]["backup"]["name"] == "Mock Backup Baru", f"status={s}")
    s, rs = req(RUST, "/api/admin/providers/reset-stats", "POST", {}, admin_token)
    check("providers reset-stats (Rust)",
          s == 200 and rs.get("ok") is True and "providers" in rs, f"status={s}")
    s, tb = req(RUST, "/api/admin/db/tables", token=admin_token)
    names = {t["name"]: t for t in tb.get("tables", [])}
    check("db tables (Rust)",
          s == 200 and names.get("users", {}).get("count", 0) >= 2 and "chat_history" in names,
          f"status={s} n={len(names)}")
    s, tu = req(NODE, "/api/admin/db/table/users", token=admin_token)
    check("db table users (Node) + redaksi access_code",
          s == 200 and tu.get("total", 0) >= 2 and "access_code" in tu.get("redactedColumns", [])
          and "access_code" not in tu.get("columns", []),
          f"status={s} total={tu.get('total')}")
    s, lk = req(RUST, "/api/admin/lookup-key?q=ParitasLima", token=admin_token)
    hit = [u for u in lk.get("users", []) if u.get("userId") == user_a]
    check("lookup-key (Rust)",
          s == 200 and len(hit) == 1 and hit[0].get("keyRecoverable") is False,
          f"status={s} hit={hit}")
    s, rk = req(NODE, f"/api/admin/users/{user_b}/reset-key", "POST", {}, admin_token)
    new_code = rk.get("accessCode", "")
    check("reset-key user B (Node)",
          s == 200 and new_code.startswith("YUKI-"), f"status={s} code={new_code[:6]}")
    s, lg = req(RUST, "/api/login-code", "POST", {"accessCode": new_code})
    check("kode baru B login di Rust",
          s == 200 and lg.get("userId") == user_b, f"status={s}")
    s, us = req(RUST, "/api/admin/users", token=admin_token)
    ids = {u["userId"] for u in us.get("users", [])}
    check("admin users (Rust)", s == 200 and {user_a, user_b} <= ids, f"status={s}")
    s, ut = req(NODE, f"/api/admin/users/{user_a}/tables", token=admin_token)
    tbl = {t["name"]: t["count"] for t in ut.get("tables", [])}
    check("user tables (Node)",
          s == 200 and tbl.get("chat_history", 0) >= 2, f"status={s} tbl={tbl.get('chat_history')}")
    s, ws = req(RUST, "/api/admin/workspace", token=admin_token)
    check("workspace summary (Rust)",
          s == 200 and ws.get("exists") is True, f"status={s} ws={ws.get('exists')}")
    s, wl = req(NODE, "/api/admin/workspace/list?path=", token=admin_token)
    entry_names = {e["name"] for e in wl.get("entries", [])}
    check("workspace list (Node)", s == 200 and "manual.txt" in entry_names, f"status={s}")
    s, wf = req(RUST, "/api/admin/workspace/file?path=manual.txt", token=admin_token)
    check("workspace file (Rust)",
          s == 200 and wf.get("content") == "isi berkas manual paritas", f"status={s}")
    s, bk = req(RUST, "/api/admin/block-ip", "POST",
                {"ip": "203.0.113.7", "hours": 2, "reason": "tes-paritas"}, admin_token)
    check("block-ip (Rust)",
          s == 200 and bk.get("ok") is True and bk.get("ip") == "203.0.113.7", f"status={s} bk={bk}")
    s, bl = req(NODE, "/api/admin/blocked-ips", token=admin_token)
    check("blocked-ips terbaca Node",
          s == 200 and any(b["ip"] == "203.0.113.7" and b["reason"] == "tes-paritas"
                           for b in bl.get("blocked", [])),
          f"status={s} bl={bl.get('blocked')}")
    s, ub = req(NODE, "/api/admin/blocked-ips/203.0.113.7", "DELETE", token=admin_token)
    check("unblock-ip (Node)", s == 200 and ub.get("removed") is True, f"status={s}")
    s, bl = req(RUST, "/api/admin/blocked-ips", token=admin_token)
    check("blocked-ips kosong di Rust",
          s == 200 and bl.get("blocked") == [], f"status={s}")

    # ===== G. analyze-url =====
    s, az = req(RUST, "/api/analyze-url", "POST", {"url": "http://127.0.0.1:3101/api/health"}, token_a)
    check("analyze-url privat ditolak (Rust)",
          s == 200 and az.get("ok") is False, f"status={s} az={az}")
    s, az = req(NODE, "/api/analyze-url", "POST", {"url": "http://127.0.0.1:3101/api/health"}, token_a)
    check("analyze-url privat ditolak (Node)",
          s == 200 and az.get("ok") is False, f"status={s} az={az}")

    # ===== H. 404 API + hapus akun =====
    s, nf = req(RUST, "/api/tidak-ada")
    check("404 API (Rust)",
          s == 404 and nf.get("error") == 'Rute API "/api/tidak-ada" tidak ditemukan.', f"status={s}")
    s, nf = req(NODE, "/api/tidak-ada")
    check("404 API (Node)",
          s == 404 and nf.get("error") == 'Rute API "/api/tidak-ada" tidak ditemukan.', f"status={s}")
    _, reg_c = req(NODE, "/api/register", "POST", {"username": "ParitasHapus"})
    token_c, code_c = reg_c["sessionToken"], reg_c["accessCode"]
    s, dl = req(RUST, "/api/account", "DELETE", token=token_c)
    check("hapus akun (Rust)", s == 200 and dl.get("ok") is True, f"status={s}")
    s, lg = req(NODE, "/api/login-code", "POST", {"accessCode": code_c})
    check("akun terhapus tidak bisa login (Node)", s == 404, f"status={s}")
    s, _ = req(RUST, "/api/chat/history", token=token_c)
    check("sesi akun terhapus invalid (Rust)", s == 401, f"status={s}")

    # ===== I. Ganti password admin (terakhir; bucket login bersama) =====
    s, pw = req(RUST, "/api/admin/password", "POST", {"password": ADMIN_PW2}, admin_token)
    check("ganti password admin (Rust)", s == 200 and pw.get("ok") is True, f"status={s}")
    s, _, _ = login_admin(NODE, ADMIN_PW)
    check("password lama ditolak Node", s == 401, f"status={s}")
    s, _, _ = login_admin(NODE, ADMIN_PW2)
    check("password baru diterima Node", s == 200, f"status={s}")

    failed = [n for n, ok in results if not ok]
    print(f"\n=== {len(results) - len(failed)}/{len(results)} PASS ===")
    if failed:
        print("GAGAL:", failed)
    stop_all()
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    import sys
    main()
