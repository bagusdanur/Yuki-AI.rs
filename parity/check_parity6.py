#!/usr/bin/env python3
"""Paritas Fase 6 (TTS): Node (3101) vs Rust (3102), DB + AUTH_SECRET +
TTS_CONFIG_FILE bersama. Engine melawan mock lokal: VOICEVOX 50021,
Kokoro 50030, Edge WS 50031. Edge Node diuji lewat skrip yang mem-patch
WSS_URL msedge-tts ke mock yang sama."""
import http.client
import json
import os
import shutil
import subprocess
import sys
import time

BASE = "/tmp/yuki-parity6"
ROOT = os.path.expanduser("~/workspace/yuki-ai-rust")
NODE_DIR = os.path.join(ROOT, "original")
RUST_BIN = os.path.join(ROOT, "yuki-rs/target/release/yuki-rs")
ADMIN_PW = "admin-paritas-123"
RESULTS = []


def check(name, cond, extra=""):
    RESULTS.append((name, bool(cond)))
    print(("PASS " if cond else "FAIL ") + name + (f"  [{extra}]" if extra and not cond else ""))


def req(port, method, path, body=None, headers=None, raw=False):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    h = {"Content-Type": "application/json"}
    if headers:
        h.update(headers)
    conn.request(method, path, json.dumps(body) if body is not None else None, h)
    res = conn.getresponse()
    data = res.read()
    out_headers = {k.lower(): v for k, v in res.getheaders()}
    conn.close()
    if raw:
        return res.status, out_headers, data
    try:
        return res.status, out_headers, json.loads(data or b"null")
    except Exception:
        return res.status, out_headers, data


def wait_health(port, tries=60):
    for _ in range(tries):
        try:
            s, _, _ = req(port, "GET", "/api/health")
            if s == 200:
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False


def wait_port(port, tries=40):
    import socket
    for _ in range(tries):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=1).close()
            return True
        except Exception:
            time.sleep(0.25)
    return False


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(BASE, exist_ok=True)
    procs = []
    env_common = {
        **os.environ,
        "AUTH_SECRET": "paritas-secret-123456",
        "MEMORY_DB": f"{BASE}/yuki.db",
        "TTS_CONFIG_FILE": f"{BASE}/tts-settings.json",
        "ADMIN_PASSWORD": ADMIN_PW,
        "ADMIN_PASSWORD_FILE": f"{BASE}/admin-password.json",
        "LLM_PROVIDER_CONFIG_FILE": f"{BASE}/llm-providers.json",
        "YUKI_PROVIDER_STATS_FILE": f"{BASE}/provider-stats.json",
        "KOKORO_URL": "http://127.0.0.1:50030",
        "HOST": "127.0.0.1",
    }
    try:
        # --- Mock TTS ---
        vv_env = {**os.environ, "VV_LOG": f"{BASE}/vv-log.json"}
        procs.append(subprocess.Popen(
            [sys.executable, os.path.join(ROOT, "parity/mock_voicevox.py")],
            env=vv_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        procs.append(subprocess.Popen(
            [sys.executable, os.path.join(ROOT, "parity/mock_kokoro.py")],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        edge_env = {**os.environ, "MOCK_EDGE_LOG": f"{BASE}/edge-log.jsonl"}
        procs.append(subprocess.Popen(
            ["/usr/bin/node", os.path.join(ROOT, "parity/mock_edge.mjs")],
            env=edge_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        for p in (50021, 50030, 50031):
            assert wait_port(p), f"mock port {p} tidak naik"

        # --- Server ---
        node_env = {**env_common, "PORT": "3101"}
        procs.append(subprocess.Popen(
            ["/usr/bin/node", "server.js"], cwd=NODE_DIR, env=node_env,
            stdout=open(f"{BASE}/node.log", "w"), stderr=subprocess.STDOUT))
        rust_env = {**env_common, "PORT": "3102",
                    "YUKI_EDGE_WSS_URL": "ws://127.0.0.1:50031/edge"}
        procs.append(subprocess.Popen(
            [RUST_BIN], cwd=os.path.join(ROOT, "yuki-rs"), env=rust_env,
            stdout=open(f"{BASE}/rust.log", "w"), stderr=subprocess.STDOUT))
        assert wait_health(3101), "Node tidak sehat"
        assert wait_health(3102), "Rust tidak sehat"

        # --- User + admin ---
        s, _, reg = req(3101, "POST", "/api/register", {"username": "rina6"})
        user_token = reg["sessionToken"]
        s, hdrs, _ = req(3102, "POST", "/api/admin/login", {"password": ADMIN_PW}, raw=True)
        set_cookie = hdrs.get("set-cookie", "")
        admin_token = set_cookie.split("yuki_admin_session=")[1].split(";")[0]
        AH = {"Authorization": f"Bearer {admin_token}"}
        UH = {"Authorization": f"Bearer {user_token}"}

        # A. Settings default identik (belum ada file)
        _, _, a_node = req(3101, "GET", "/api/admin/tts", headers=AH)
        _, _, a_rust = req(3102, "GET", "/api/admin/tts", headers=AH)
        check("A1 settings default sama", a_node["settings"] == a_rust["settings"],
              f'{a_node["settings"]} vs {a_rust["settings"]}')
        check("A2 engines sama", a_node["engines"] == a_rust["engines"] == ["edge", "voicevox", "kokoro", "ttsquest"])
        check("A3 probe voicevox reachable dua-duanya",
              a_node["voicevox"] == {"reachable": True, "version": "0.25.0"} == a_rust["voicevox"])
        check("A4 process state sama (pm2 absen)",
              a_node["process"]["state"] == a_rust["process"]["state"] == "unknown"
              and a_node["process"]["pm2Name"] == a_rust["process"]["pm2Name"] == "voicevox")

        # J. Validasi /api/tts
        for port, tag in ((3101, "Node"), (3102, "Rust")):
            s, _, e1 = req(port, "POST", "/api/tts", {"text": "   "}, headers=UH)
            check(f"J1 teks kosong 400 ({tag})",
                  s == 400 and e1["error"] == "Teks suara harus berisi 1-2000 karakter.")
            s, _, e2 = req(port, "POST", "/api/tts", {"text": "a" * 2001}, headers=UH)
            check(f"J2 teks >2000 400 ({tag})", s == 400 and "error" in e2)
            s, _, _ = req(port, "POST", "/api/tts", {"text": "Halo"})
            check(f"J3 tanpa sesi 401 ({tag})", s == 401)

        # B. PUT engine voicevox di Rust terbaca Node (file bersama)
        s, _, put = req(3102, "PUT", "/api/admin/tts", {"engine": "voicevox"}, headers=AH)
        check("B1 PUT Rust ok", s == 200 and put["settings"]["engine"] == "voicevox")
        _, _, got = req(3101, "GET", "/api/admin/tts", headers=AH)
        check("B2 settings terbaca Node", got["settings"]["engine"] == "voicevox"
              and got["settings"]["voicevoxSpeaker"] == 14)

        # C. Sintesis VOICEVOX identik
        expected_wav = b"WAV:speed=1.08 pitch=0.03 int=1.25 speaker=14"
        bodies = {}
        for port, tag in ((3102, "Rust"), (3101, "Node")):
            s, hdrs, data = req(port, "POST", "/api/tts",
                                {"text": "Halo Yuki", "mood": "ceria"}, headers=UH, raw=True)
            bodies[tag] = data
            check(f"C voicevox ({tag})", s == 200 and data == expected_wav
                  and hdrs.get("content-type", "").startswith("audio/wav"),
                  f"{s} {data!r}")
        check("C3 byte identik", bodies["Rust"] == bodies["Node"])

        # L. cleanDialogueText: teks kotor -> query bersih di mock
        dirty = "*aksi* Halo [emosi: senang] apa `kode` kabar https://x.y/z"
        for port, tag in ((3102, "Rust"), (3101, "Node")):
            req(port, "POST", "/api/tts", {"text": dirty, "mood": "tenang"}, headers=UH, raw=True)
            with open(f"{BASE}/vv-log.json") as f:
                log = json.load(f)
            check(f"L teks dibersihkan ({tag})", log["text"] == "Halo apa kabar", str(log))

        # D. Saklar master mati -> 204 di dua-duanya
        req(3101, "PUT", "/api/admin/tts", {"voicevoxEnabled": False}, headers=AH)
        for port, tag in ((3101, "Node"), (3102, "Rust")):
            s, _, data = req(port, "POST", "/api/tts", {"text": "Halo"}, headers=UH, raw=True)
            check(f"D disabled 204 ({tag})", s == 204 and data == b"")

        # H. Speakers
        _, _, sp_node = req(3101, "GET", "/api/admin/tts/voicevox/speakers", headers=AH)
        _, _, sp_rust = req(3102, "GET", "/api/admin/tts/voicevox/speakers", headers=AH)
        check("H speakers sama", sp_node == sp_rust and sp_rust["ok"] is True
              and len(sp_rust["speakers"]) == 2
              and sp_rust["speakers"][0] == {"id": 14, "name": "Yuki-chan · Normal",
                                             "character": "Yuki-chan", "style": "Normal"},
              json.dumps(sp_rust)[:200])

        # I. Status voicevox
        _, _, st_node = req(3101, "GET", "/api/admin/tts/voicevox/status", headers=AH)
        _, _, st_rust = req(3102, "GET", "/api/admin/tts/voicevox/status", headers=AH)
        check("I status sama", all(
            st_node[k] == st_rust[k] for k in ("engine", "keepAlive", "enabled"))
            and st_rust["enabled"] is False and st_rust["voicevox"]["reachable"] is True)

        # K. Start/stop tanpa pm2 -> 503 ok:false dua-duanya
        for method_path, tag in (("start", "start"), ("stop", "stop")):
            for port, who in ((3102, "Rust"), (3101, "Node")):
                s, _, r = req(port, "POST", f"/api/admin/tts/voicevox/{method_path}",
                              {}, headers=AH)
                check(f"K {tag} ({who}) 503", s == 503 and r.get("ok") is False,
                      f"{s} {str(r)[:120]}")

        # E. Kokoro
        req(3102, "PUT", "/api/admin/tts", {"engine": "kokoro", "voicevoxEnabled": True}, headers=AH)
        for port, tag in ((3102, "Rust"), (3101, "Node")):
            s, hdrs, data = req(port, "POST", "/api/tts",
                                {"text": "Halo", "mood": "ceria"}, headers=UH, raw=True)
            check(f"E kokoro ({tag})", s == 200 and data == b"KOKORO:vtuber_cute:1.08:Halo",
                  f"{s} {data!r}")

        # G. Edge lewat mock WS: admin test Rust vs skrip Node asli
        req(3102, "PUT", "/api/admin/tts", {"engine": "edge"}, headers=AH)
        open(f"{BASE}/edge-log.jsonl", "w").close()
        s, hdrs, rust_audio = req(
            3102, "POST", "/api/admin/tts/test",
            {"text": "Halo Yuki, *melambaikan tangan* apa kabar?", "mood": "senang"},
            headers=AH, raw=True)
        check("G1 admin test Rust -> audio mock",
              s == 200 and rust_audio == b"FAKE-MP3-CHUNK-1-CHUNK-2"
              and hdrs.get("content-type", "").startswith("audio/mpeg"),
              f"{s} {rust_audio!r}")
        out = subprocess.run(
            ["/usr/bin/node", os.path.join(ROOT, "parity/edge_node_test.mjs"),
             f"{BASE}/node-edge.mp3"],
            capture_output=True, text=True, timeout=60)
        node_audio = open(f"{BASE}/node-edge.mp3", "rb").read() if os.path.exists(
            f"{BASE}/node-edge.mp3") else b""
        check("G2 klien Node asli -> audio sama",
              out.returncode == 0 and node_audio == rust_audio,
              out.stdout + out.stderr)
        with open(f"{BASE}/edge-log.jsonl") as f:
            entries = [json.loads(x) for x in f if x.strip()]
        ssml = [e for e in entries if e["type"] == "ssml"]
        conf = [e for e in entries if e["type"] == "config"]
        check("G3 config outputFormat benar dua-duanya",
              len(conf) == 2 and all(
                  c["outputFormat"] == "audio-24khz-48kbitrate-mono-mp3" for c in conf),
              json.dumps(conf))
        check("G4 SSML identik (voice/pitch/rate/input)",
              len(ssml) == 2 and ssml[0]["voice"] == ssml[1]["voice"] == "id-ID-GadisNeural"
              and ssml[0]["pitch"] == ssml[1]["pitch"] == "+5Hz"
              and ssml[0]["rate"] == ssml[1]["rate"] == "+4%"
              and ssml[0]["input"] == ssml[1]["input"] == "Halo Yuki, apa kabar?",
              json.dumps(ssml))
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(timeout=5)
            except Exception:
                p.kill()

    passed = sum(1 for _, ok in RESULTS if ok)
    print(f"\n== {passed}/{len(RESULTS)} PASS ==")
    return 0 if passed == len(RESULTS) else 1


if __name__ == "__main__":
    sys.exit(main())
