#!/usr/bin/env python3
"""Paritas Fase 4: agent (runner/planner/skills/subagent) Node <-> Rust.

DB bersama + workspace agent bersama + mock provider tool-calling.
Skenario: katalog skill, agent chat kalkulator (lintas baca progress +
replay), alur approval jadwal dua arah, reject approval, workspace
import + baca via agent, dynamic skills lintas server, delegate_tasks,
cancel run + bentuk progress.
"""
import json
import os
import subprocess
import sys
import time
import urllib.request
import urllib.error

BASE = "/tmp/yuki-parity4"
DB = f"{BASE}/yuki.db"
REQ_DIR = f"{BASE}/requests"
WORKSPACE = f"{BASE}/agent-workspace"
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
        with urllib.request.urlopen(r, timeout=120) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except Exception:
            return e.code, {}


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


def stop(name):
    p = procs.get(name)
    if p and p.poll() is None:
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


def agent_chat(base, token, text, request_id=None):
    body = {"messages": [{"role": "user", "content": text}], "mode": "agent"}
    if request_id:
        body["requestId"] = request_id
    return req(base, "/api/chat", "POST", body, token)


def step_sig(steps):
    return [(s.get("tool"), s.get("status"), s.get("title")) for s in steps or []]


def count_tool_content(needle):
    """Jumlah file permintaan mock yang memuat needle di isi pesan
    (hasil tool yang diteruskan ke LLM tercatat di sini)."""
    import glob as _glob
    hits = 0
    for path in _glob.glob(os.path.join(REQ_DIR, "req_*.json")):
        try:
            body = json.load(open(path))
        except Exception:
            continue
        text = "\n".join(str(m.get("content") or "") for m in body.get("messages", []))
        if needle in text:
            hits += 1
    return hits


def main():
    os.makedirs(REQ_DIR, exist_ok=True)
    os.makedirs(WORKSPACE, exist_ok=True)
    providers = {
        "primary": {"id": "primary", "name": "Mock Primary", "baseURL": "http://127.0.0.1:3201/v1",
                    "model": "mock-model", "apiKey": "mock-key", "inputCostPerMillion": 0, "outputCostPerMillion": 0},
        "backup": {"id": "backup", "name": "Mock Backup", "baseURL": "http://127.0.0.1:3201/v1",
                   "model": "mock-model", "apiKey": "mock-key", "inputCostPerMillion": 0, "outputCostPerMillion": 0},
    }
    json.dump(providers, open(f"{BASE}/llm-providers.json", "w"))

    env = dict(os.environ)
    env["MOCK_REQ_DIR"] = REQ_DIR
    start("mock", ["python3", f"{PARITY}/mock_provider4.py"], env, BASE)
    time.sleep(1)
    assert start_node(), "node gagal start"
    assert start_rust(), "rust gagal start"

    _, reg = req(NODE, "/api/register", "POST", {"username": "Uji"})
    token = reg["sessionToken"]
    user_id = reg["userId"]

    # ===== A. Katalog skill =====
    s_r, skills_r = req(RUST, "/api/agent/skills")
    s_n, skills_n = req(NODE, "/api/agent/skills")
    canon = lambda items: sorted(
        ({k: s.get(k) for k in ("name", "title", "category", "version", "description")} for s in items),
        key=lambda s: s["name"] or "",
    )
    check("katalog skill 16 sama",
          s_r == s_n == 200
          and len(skills_r.get("skills", [])) == 16
          and canon(skills_r["skills"]) == canon(skills_n["skills"]),
          f"rust={len(skills_r.get('skills', []))} node={len(skills_n.get('skills', []))}")
    full_r = sorted(skills_r.get("skills", []), key=lambda s: s.get("name") or "")
    full_n = sorted(skills_n.get("skills", []), key=lambda s: s.get("name") or "")
    check("objek skill utuh identik", full_r == full_n,
          f"rust={len(full_r)} node={len(full_n)}")

    # ===== B. Agent chat kalkulator dua arah + replay =====
    s, calc_r = agent_chat(RUST, token, "hitung 25% * 200 untukku", "paritas-calc-rust-01")
    check("agent kalkulator Rust sukses",
          s == 200 and calc_r.get("workflowState") == "succeeded" and calc_r.get("mode") == "agent",
          f"status={s} state={calc_r.get('workflowState')}")
    s, calc_n = agent_chat(NODE, token, "hitung 25% * 200 untukku", "paritas-calc-node-01")
    check("agent kalkulator Node sukses",
          s == 200 and calc_n.get("workflowState") == "succeeded",
          f"status={s} state={calc_n.get('workflowState')}")
    check("reply + mood kalkulator sama",
          calc_r.get("reply") == calc_n.get("reply") and calc_r.get("mood") == calc_n.get("mood"),
          f"rust={calc_r.get('reply')!r} node={calc_n.get('reply')!r}")
    check("langkah kalkulator sama",
          step_sig(calc_r.get("steps")) == step_sig(calc_n.get("steps")),
          f"rust={step_sig(calc_r.get('steps'))} node={step_sig(calc_n.get('steps'))}")
    # Payload langkah tersimpan memang tanpa output (kedua engine sama);
    # hasil tool dibuktikan dari pesan tool yang tercatat di mock.
    check("hasil kalkulator 50 terkirim ke LLM kedua engine",
          count_tool_content('"result":50') >= 2,
          f"hits={count_tool_content(chr(34) + 'result' + chr(34) + ':50')}")
    _, prog_r = req(NODE, "/api/agent/progress/paritas-calc-rust-01", token=token)
    _, prog_n = req(RUST, "/api/agent/progress/paritas-calc-node-01", token=token)
    check("progress lintas server sama-sama succeeded",
          prog_r.get("state") == "succeeded" and prog_n.get("state") == "succeeded"
          and prog_r.get("done") is True and prog_n.get("done") is True,
          f"rust-run={prog_r.get('state')} node-run={prog_n.get('state')}")
    s, replay = agent_chat(RUST, token, "hitung 25% * 200 untukku", "paritas-calc-rust-01")
    check("replay requestId mengembalikan hasil tersimpan",
          s == 200 and replay.get("reply") == calc_r.get("reply"),
          f"status={s}")

    # ===== C. Approval jadwal: buat di Rust, setujui di Node =====
    s, sched_r = agent_chat(RUST, token, "jadwalkan pengingat minum vitamin", "paritas-sched-rust-01")
    approval_r = sched_r.get("awaitingApproval") or {}
    check("agent jadwal Rust menunggu approval",
          s == 200 and sched_r.get("workflowState") == "awaiting_approval" and bool(approval_r.get("workflowId")),
          f"status={s} state={sched_r.get('workflowState')}")
    _, pend_n = req(NODE, "/api/agent/workflows/pending", token=token)
    wf = next((w for w in pend_n.get("workflows", []) if w.get("id") == approval_r.get("workflowId")), None)
    check("workflow Rust terbaca pending di Node",
          wf is not None and wf.get("toolName") == "schedule_task"
          and (wf.get("args") or {}).get("title") == "Minum vitamin",
          f"wf={wf}")
    _, active_n = req(NODE, "/api/agent/runs/active", token=token)
    check("run aktif di Node = run Rust (awaiting_approval)",
          (active_n.get("run") or {}).get("requestId") == "paritas-sched-rust-01"
          and (active_n.get("run") or {}).get("state") == "awaiting_approval",
          f"active={active_n.get('run')}")
    s, appr_n = req(NODE, f"/api/agent/approvals/{approval_r.get('workflowId')}", "POST",
                    {"decision": "approve"}, token)
    check("approve di Node sukses + run selesai",
          s == 200 and appr_n.get("success") is True and appr_n.get("state") == "succeeded",
          f"status={s} state={appr_n.get('state')} reply={str(appr_n.get('reply'))[:80]!r}")
    _, agenda_r = req(RUST, "/api/agenda", token=token)
    titles = [t.get("title") for t in agenda_r.get("tasks", [])]
    check("task jadwal hasil approve ada di agenda (baca Rust)",
          titles.count("Minum vitamin") == 1, f"titles={titles}")
    _, prog_r2 = req(RUST, "/api/agent/progress/paritas-sched-rust-01", token=token)
    check("progress run Rust succeeded (baca Rust)",
          prog_r2.get("state") == "succeeded", f"state={prog_r2.get('state')}")

    # ===== D. Approval jadwal: buat di Node, setujui di Rust =====
    s, sched_n = agent_chat(NODE, token, "jadwalkan pengingat minum vitamin", "paritas-sched-node-01")
    approval_n = sched_n.get("awaitingApproval") or {}
    check("agent jadwal Node menunggu approval",
          s == 200 and sched_n.get("workflowState") == "awaiting_approval" and bool(approval_n.get("workflowId")),
          f"status={s} state={sched_n.get('workflowState')}")
    s, appr_r = req(RUST, f"/api/agent/approvals/{approval_n.get('workflowId')}", "POST",
                    {"decision": "approve"}, token)
    check("approve di Rust sukses + run selesai",
          s == 200 and appr_r.get("success") is True and appr_r.get("state") == "succeeded",
          f"status={s} state={appr_r.get('state')} reply={str(appr_r.get('reply'))[:80]!r}")
    _rr, _rn = appr_r.get("reply") or "", appr_n.get("reply") or ""
    _diff = next((i for i in range(min(len(_rr), len(_rn))) if _rr[i] != _rn[i]), min(len(_rr), len(_rn)))
    check("reply approve lintas engine sama",
          _rr == _rn,
          f"len_rust={len(_rr)} len_node={len(_rn)} diff_at={_diff} "
          f"rust_ctx={_rr[max(0, _diff - 20):_diff + 20]!r} node_ctx={_rn[max(0, _diff - 20):_diff + 20]!r}")
    _, agenda_n = req(NODE, "/api/agenda", token=token)
    titles_n = [t.get("title") for t in agenda_n.get("tasks", [])]
    # createScheduledTask Node idempoten per (user,judul,deskripsi,cron,zone):
    # jadwal kedua yang identik tidak membuat baris baru. Rust harus sama.
    check("task kedua idempoten: agenda tetap 1 task sama (baca Node)",
          titles_n.count("Minum vitamin") == 1,
          f"titles={titles_n}")

    # ===== E. Reject approval =====
    s, sched_x = agent_chat(RUST, token, "jadwalkan pengingat minum vitamin", "paritas-reject-01")
    approval_x = sched_x.get("awaitingApproval") or {}
    s, rej = req(NODE, f"/api/agent/approvals/{approval_x.get('workflowId')}", "POST",
                 {"decision": "reject"}, token)
    check("reject di Node -> workflow + run cancelled",
          s == 200 and rej.get("status") == "rejected" and rej.get("state") == "cancelled",
          f"status={s} body={rej}")
    _, pend_r = req(RUST, "/api/agent/workflows/pending", token=token)
    check("workflow reject hilang dari pending Rust",
          all(w.get("id") != approval_x.get("workflowId") for w in pend_r.get("workflows", [])),
          f"pending={len(pend_r.get('workflows', []))}")
    _, prog_x = req(RUST, "/api/agent/progress/paritas-reject-01", token=token)
    check("progress run reject = cancelled",
          prog_x.get("state") == "cancelled" and prog_x.get("done") is True,
          f"state={prog_x.get('state')}")

    # ===== F. Workspace import + baca via agent =====
    s, imp_r = req(RUST, "/api/agent/workspace/import", "POST",
                   {"files": [{"path": "src/app.js", "content": "const x = 1\n"},
                              {"path": "README.md", "content": "# Uji Paritas\n"}]}, token)
    check("import workspace via Rust sukses",
          s == 200 and imp_r.get("success") is True, f"status={s} body={str(imp_r)[:160]}")
    s_r2, imp_bad_r = req(RUST, "/api/agent/workspace/import", "POST",
                          {"files": [{"path": "../evil.txt", "content": "x"}]}, token)
    s_n2, imp_bad_n = req(NODE, "/api/agent/workspace/import", "POST",
                          {"files": [{"path": "../evil.txt", "content": "x"}]}, token)
    check("import path jahat 400 sama",
          s_r2 == s_n2 == 400 and imp_bad_r.get("error") == imp_bad_n.get("error"),
          f"rust={imp_bad_r.get('error')} node={imp_bad_n.get('error')}")
    s, ws_n = agent_chat(NODE, token, "lihat file di workspace saya", "paritas-ws-node-01")
    s2, ws_r = agent_chat(RUST, token, "lihat file di workspace saya", "paritas-ws-rust-01")
    step_n = next((x for x in ws_n.get("steps", []) if x.get("tool") == "list_workspace_files"), {})
    step_r = next((x for x in ws_r.get("steps", []) if x.get("tool") == "list_workspace_files"), {})
    check("agent baca workspace kedua engine done + isi terbaca LLM",
          step_n.get("status") == "done" and step_r.get("status") == "done"
          and count_tool_content("README.md") >= 2,
          f"node_status={step_n.get('status')} rust_status={step_r.get('status')} "
          f"readme_hits={count_tool_content('README.md')}")

    # ===== G. Dynamic skills =====
    s, bad = req(RUST, "/api/agent/dynamic-skills", "POST",
                 {"name": "Skill Jahat", "description": "uji", "instructions": "lakukan sesuatu",
                  "toolAllowlist": ["run_javascript_code"]}, token)
    s_nb, bad_n = req(NODE, "/api/agent/dynamic-skills", "POST",
                      {"name": "Skill Jahat", "description": "uji", "instructions": "lakukan sesuatu",
                       "toolAllowlist": ["run_javascript_code"]}, token)
    check("dynamic skill langgar ceiling 400 sama",
          s == s_nb == 400 and bad.get("error") == bad_n.get("error"),
          f"rust={bad.get('error')} node={bad_n.get('error')}")
    s, created = req(RUST, "/api/agent/dynamic-skills", "POST",
                     {"name": "Skill Uji Paritas", "description": "skill uji fase 4",
                      "instructions": "Jawab dengan singkat dan jelas.",
                      "toolAllowlist": ["get_current_time"]}, token)
    skill = created.get("skill") or created
    skill_id = skill.get("id")
    check("dynamic skill dibuat di Rust 201",
          s == 201 and created.get("success") is True and bool(skill_id),
          f"status={s} body={str(created)[:160]}")
    s, validated = req(NODE, f"/api/agent/dynamic-skills/{skill_id}/validate", "POST", {}, token)
    check("validate di Node -> pending_approval",
          s == 200 and validated.get("success") is True
          and (validated.get("skill") or {}).get("status") == "pending_approval",
          f"status={s} body={str(validated)[:160]}")
    s, approved = req(RUST, f"/api/agent/dynamic-skills/{skill_id}/approve", "POST", {}, token)
    check("approve di Rust -> active",
          s == 200 and (approved.get("skill") or {}).get("status") == "active",
          f"status={s} body={str(approved)[:160]}")
    _, dyn_n = req(NODE, "/api/agent/dynamic-skills", token=token)
    check("skill aktif terbaca di Node",
          any(x.get("id") == skill_id and x.get("status") == "active" for x in dyn_n.get("skills", [])),
          f"skills={len(dyn_n.get('skills', []))}")
    s, disabled = req(NODE, f"/api/agent/dynamic-skills/{skill_id}/disable", "POST", {}, token)
    _, dyn_r2 = req(RUST, "/api/agent/dynamic-skills", token=token)
    check("disable di Node -> disabled (terbaca di Rust)",
          s == 200 and disabled.get("success") is True
          and any(x.get("id") == skill_id and x.get("status") == "disabled" for x in dyn_r2.get("skills", [])),
          f"status={s} body={disabled}")

    # ===== H. delegate_tasks =====
    # Output langkah tidak disimpan di payload run (kedua engine sama);
    # hasil delegate dibuktikan dari pesan tool yang tercatat di mock.
    s, del_r = agent_chat(RUST, token, "tolong delegasikan dan bagi tugas riset kecil ini", "paritas-delegate-01")
    step_del_r = next((x for x in del_r.get("steps", []) if x.get("tool") == "delegate_tasks"), {})
    check("delegate Rust sukses (langkah done + run succeeded)",
          s == 200 and del_r.get("workflowState") == "succeeded"
          and step_del_r.get("status") == "done",
          f"status={s} step={step_del_r.get('status')}")
    s, del_n = agent_chat(NODE, token, "tolong delegasikan dan bagi tugas riset kecil ini", "paritas-delegate-02")
    step_del_n = next((x for x in del_n.get("steps", []) if x.get("tool") == "delegate_tasks"), {})
    check("delegate Node sukses (langkah done + run succeeded)",
          s == 200 and del_n.get("workflowState") == "succeeded"
          and step_del_n.get("status") == "done",
          f"status={s} step={step_del_n.get('status')}")
    check("hasil delegate kedua engine: 2 worker berhasil + workerId sama",
          count_tool_content('"successful":2') >= 2
          and count_tool_content('"workerId":"worker_') >= 2,
          f"succ_hits={count_tool_content(chr(34) + 'successful' + chr(34) + ':2')} "
          f"wid_hits={count_tool_content(chr(34) + 'workerId' + chr(34))}")

    # ===== I. Cancel + bentuk progress =====
    import sqlite3
    conn = sqlite3.connect(DB)
    conn.execute(
        "INSERT INTO agent_runs (request_id, user_id, goal, state) VALUES (?, ?, ?, 'running')",
        ("paritas-cancel-01", user_id, "uji cancel"))
    conn.commit()
    conn.close()
    s, cancelled = req(RUST, "/api/agent/runs/paritas-cancel-01/cancel", "POST", {}, token)
    check("cancel run via Rust -> cancelled",
          s == 200 and cancelled.get("success") is True
          and (cancelled.get("run") or {}).get("state") == "cancelled",
          f"status={s} body={str(cancelled)[:160]}")
    _, prog_c = req(NODE, "/api/agent/progress/paritas-cancel-01", token=token)
    check("progress cancel terbaca di Node",
          prog_c.get("state") == "cancelled", f"state={prog_c.get('state')}")
    s_r3, e_r3 = req(RUST, "/api/agent/progress/x", token=token)
    s_n3, e_n3 = req(NODE, "/api/agent/progress/x", token=token)
    check("progress id tidak valid 400 sama",
          s_r3 == s_n3 == 400 and e_r3.get("error") == e_n3.get("error"),
          f"rust={e_r3.get('error')} node={e_n3.get('error')}")
    _, unk_r = req(RUST, "/api/agent/progress/tidakada12345", token=token)
    _, unk_n = req(NODE, "/api/agent/progress/tidakada12345", token=token)
    check("progress run tak dikenal = queued kosong",
          unk_r == unk_n == {"steps": [], "done": False, "state": "queued"},
          f"rust={unk_r} node={unk_n}")
    _, act_r = req(RUST, "/api/agent/runs/active", token=token)
    _, act_n = req(NODE, "/api/agent/runs/active", token=token)
    check("tidak ada run aktif di akhir kedua engine",
          act_r.get("run") is None and act_n.get("run") is None,
          f"rust={act_r} node={act_n}")

    stop("node")
    stop("rust")
    stop("mock")
    failed = [n for n, ok in results if not ok]
    print(f"\n=== {len(results) - len(failed)}/{len(results)} PASS ===")
    if failed:
        print("GAGAL:", failed)
        sys.exit(1)


if __name__ == "__main__":
    main()
