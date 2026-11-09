# Hasil Fase 4 — Agent: runner, planner, skills, subagent, workspace (2026-11-09, lokal di VM Muse)

## Yang diporting

- `src/agent/stores.rs` — port run-store.js + workflow-store.js +
  dynamic-skills.js + catatan/artefak/self-improvement dari memory.js:
  run durable (create-or-get, claim lease 180 dtk, cancel, recovery
  run terputus saat boot), langkah run (payload tersimpan memakai
  whitelist Node yang sama — input/output tool TIDAK disimpan),
  checkpoint approval (id acak base64url, idempotency key terikat user,
  kedaluwarsa 24 jam, claim atomik), skill dinamis (ceiling tool,
  validasi, approve, disable).
- `src/agent/planner.rs` — port planner.js: klasifikasi intent,
  minimize allowlist, strategy hash, retry guard, build/validate plan.
- `src/agent/security.rs` + `policy.rs` — SSRF (IP privat/reserved,
  port terlarang, DNS dicek), validasi eksekusi kode (31 pola), akar
  workspace aman (env YUKI_AGENT_WORKSPACE), segmen user per hash.
- `src/agent/sandbox.rs` — sandbox JS memakai binary `node` sistem
  dengan flag --permission + prlimit yang sama seperti Node (bukan
  implementasi ulang); cek sintaks JS/TS lewat vm.Script/strip-types
  node sehingga pesan error V8 identik.
- `src/agent/skills.rs` — skills-engine: katalog dibaca dari folder
  skills/ yang sama (16 skill), TOOL_SCHEMAS verbatim (40 tool) dari
  dump Node (parity/gen_schemas.mjs -> assets/tool_schemas.json).
- `src/agent/tools*.rs` — ke-16 handler skill: workspace files
  (snapshot/rollback/diff/patch/validasi proyek), artefak HTML
  (template universalMobileEngine diekstrak byte-exact), web search,
  read_url, http_api_request, Ryukomik, browser (fetch fallback —
  tidak ada Chromium di host, sama seperti Node di host ini),
  kalkulator, datetime, notes, scheduler, self-improvement,
  code analyzer, sandbox JS, skill health, delegate_tasks.
- `src/agent/persona.rs` — prompt sistem agent verbatim dari
  persona.js (termasuk format waktu WIB dan daftar 11 emosi).
- `src/agent/subagent.rs` — delegate_tasks: worker read-only,
  maks 4 subtask, budget dibagi bersama run induk.
- `src/agent/runner.rs` — loop runAgent utuh: plan, routing tool,
  panggilan LLM dengan SATU budget bersama (provider order
  backup->primary), ekstraksi tool_call dari teks, gerbang approval
  untuk tool mutasi, cache tool read-only/approved, retry guard,
  verifikasi deterministik pasca-run, laporan deterministik
  (scheduler/artefak), ground-check klaim sukses, ekstraksi artefak
  HTML dari teks, gesture/mood.
- `src/routes/agent.rs` — 12 route /api/agent/* (skills, dynamic
  skills, progress, runs/active, cancel, workspace import, workflows
  pending, approvals) + `agent_chat` menggantikan jawaban 501 di
  /api/chat; replay requestId mengembalikan hasil tersimpan persis
  seperti Node.

## Bukti — paritas `parity/check_parity4.py`: 39/39 PASS

DB bersama + workspace agent bersama + mock provider tool-calling:

- Katalog 16 skill identik objek-per-objek di kedua engine.
- Agent chat kalkulator: reply, mood, dan langkah identik; progress
  run Rust terbaca succeeded di Node dan sebaliknya; replay requestId
  mengembalikan hasil tersimpan; hasil tool (50) terbukti sampai ke
  LLM di kedua engine dari rekaman mock.
- Approval jadwal dua arah: workflow buatan Rust disetujui di Node
  (task masuk agenda, reply resume identik), workflow buatan Node
  disetujui di Rust; reject -> workflow + run cancelled.
- Workspace: import via Rust terbaca agent Node maupun Rust; import
  path jahat -> 400 dengan pesan yang sama.
- Dynamic skills lintas server: langgar ceiling -> 400 sama; buat di
  Rust -> validate di Node -> approve di Rust -> terbaca Node;
  disable di Node -> terbaca Rust.
- delegate_tasks kedua engine: langkah done, run succeeded, 2 worker
  berhasil dengan workerId terverifikasi dari rekaman mock.
- Cancel run via Rust terbaca cancelled di Node; progress id tidak
  valid -> 400 sama; run tak dikenal -> queued kosong sama.
- Unit test Rust: 29/29 PASS. Binary release: 50,7 MB
  (50.682.168 byte; Fase 3: 47,6 MB).

## Bug & selisih yang ditemukan paritas (sudah diperbaiki)

- **Bug produk dari port Fase 3**: parser jadwal Rust memformat ulang
  jam dari angka sehingga "07:00" menjadi "7:00" pada cron dan label
  human (cabang daily/weekly/besok). Node memakai string tangkapan
  mentah. Diperbaiki di scheduler.rs — cabang lain sudah cocok.
- **extractFacts tanpa gerbang**: Rust mengekstrak fakta di setiap
  chat (companion + agent), Node menggerbang dengan
  worthRemembering. Port `worth_remembering` ditambahkan
  (memory.rs; panjang per unit UTF-16 seperti JS) dan kedua jalur
  kini menggerbang sama.
- **Bucket rate-limit**: route agent kini memakai kunci yang sama
  seperti Node — path penuh berisi ID untuk route berparameter, dan
  IP klien untuk katalog skill tanpa sesi.
- Satu ekspektasi harness dikoreksi, bukan produk: jadwal kedua yang
  identik memang idempoten di Node (createScheduledTask menolak
  duplikat per user+judul+cron), jadi agenda tetap berisi 1 task —
  Rust kini sama persis.

## Catatan / deviasi yang disengaja

- Mode agent di Rust tetap TIDAK memutasi tabel emosi (ruling Fase 1);
  Node menaikkan bond lewat cabang agent. Mood yang dikembalikan
  tetap divalidasi terhadap bond tersimpan.
- calculate_expression memakai evaluator Rust: hasil dan penolakan
  karakter identik, redaksi error V8 untuk ekspresi rusak bisa beda.
  Pesan error JSON.parse juga bisa beda dari V8 (syntax check JS/TS
  sendiri lewat node, jadi identik).
- list_workspace_files mengurutkan entri per nama file; Node memakai
  urutan readdir OS. Isi hasil sama.
- Tool browser mengikuti keadaan host: tanpa Chromium, browse_page =
  fetch fallback dan screenshot/extract mengembalikan error
  ketidaktersediaan yang sama seperti Node di host ini.
- Harness paritas ini adalah bukti pengganti rangkaian test agent
  Node (test-agent-*, test-subagents, test-workspace-security,
  test-sandbox-escape) untuk permukaan yang sama; test Node itu
  sendiri tidak dijalankan terhadap server Rust.
- Semua server uji dimatikan sesudahnya; tidak ada push GitHub dan
  tidak ada deploy VPS pada fase ini.
