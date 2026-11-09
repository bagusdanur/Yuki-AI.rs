# Rencana Migrasi Yuki Agent ke Rust

Dibuat 2026-11-09. Sumber yang dibedah: bagusdanur/Yuki---AI commit 47910ac (v4.6.0, 2026-11-09).
Status: RENCANA. Belum ada kode Rust yang ditulis dan belum ada keputusan eksekusi. Setiap fase
mendapat rencana implementasi rinci sendiri (format docs/superpowers/plans di repo Yuki)
sebelum dikerjakan.

## Inventaris kondisi sekarang (terukur dari repo)

- Backend Node.js: server.js 1.822 baris + lib/ 8.376 baris = 10.198 baris JS.
- Frontend React/TypeScript: frontend/src 5.482 baris (App.tsx 2.185, styles.css 2.193).
- Route Express: 84 route (chat, memories, agent runs/skills/workflows, scheduler/agenda,
  push, games tic-tac-toe + RPS, comics/Ryukomik, TTS, admin, auth/session).
- Penyimpanan: SQLite data/yuki.db + file JSON di data/ (provider-stats) dan
  .runtime-secrets/ (llm-providers, tts-settings, admin-password, permission 600).
- Konfigurasi: sekitar 55 variabel environment.
- Modul terbesar: lib/agent/runner.js 1.042, lib/memory.js 816, lib/agent/skills-engine.js 812,
  lib/llm.js 707, lib/emotion.js 458, lib/scheduler.js 423, lib/admin-browser.js 386,
  lib/browser.js 369, lib/provider-router.js 221.
- Test: 21 script test Node di scripts/ (workspace security, agent runs, planner, scheduler,
  memory-skills, subagents, provider router/settings, agent tools, skills, redteam, sandbox escape).
- CI GitHub Actions sudah ada (npm test subset + build). Produksi: PM2 + reverse proxy.
- Dependensi khusus: express, openai, node-cron, web-push, msedge-tts,
  @huggingface/transformers (embedding di lib/semantic.js), puppeteer-core
  (lib/browser.js dan lib/admin-browser.js, Chromium eksternal via CHROMIUM_PATH).
- Layanan TTS sebagian besar eksternal lewat HTTP: Kokoro, VOICEVOX, TTSQuest.
  Yang in-process hanya Edge TTS (msedge-tts).

## Keputusan arsitektur yang diusulkan

1. Frontend TIDAK diporting. React/Vite tetap, hasil `vite build` disajikan sebagai file
   statis oleh server Rust. Nol perubahan perilaku UI.
2. Backend menjadi satu binary Rust (axum + tokio) dengan struktur modul per domain:

   ```
   yuki-rs/
     Cargo.toml
     src/
       main.rs            bootstrap, config, router
       config.rs          semua env var, satu struct tervalidasi
       state.rs           AppState (db pool, http client, settings)
       error.rs           tipe error seragam -> respons API
       routes/            chat.rs memories.rs agent.rs scheduler.rs push.rs
                          games.rs comics.rs tts.rs admin.rs auth.rs misc.rs
       agent/             runner.rs planner.rs skills_engine.rs dynamic_skills.rs
                          run_store.rs workflow_store.rs subagent.rs
                          workspace_policy.rs sandbox.rs observability.rs
       memory/            store.rs recall.rs semantic.rs
       provider/          router.rs settings.rs stats.rs llm.rs
       services/          scheduler.rs push.rs tts.rs browser.rs ryukomik.rs
                          emotion.rs security.rs ip_block.rs time.rs
       db.rs              rusqlite, skema sama persis dengan yuki.db sekarang
   ```

3. Kontrak tidak berubah: path route, bentuk JSON, format file data/yuki.db, format file
   .runtime-secrets, dan nama env var dipertahankan. Versi Rust harus bisa membaca data
   produksi yang dibuat versi Node tanpa migrasi data.
4. Strategi strangler: versi Rust dikembangkan di repo/branch terpisah, dijalankan di port
   lain, diuji terhadap test yang sama, baru menggantikan Node di PM2 setelah paritas lulus.
   Versi Node tetap jadi cadangan rollback sampai fase terakhir stabil.

## Pemetaan dependensi

| Node sekarang | Rust | Catatan |
|---|---|---|
| express | axum + tower-http | routing, static files, CORS |
| openai SDK + fetch provider | reqwest | endpoint OpenAI-compatible, streaming SSE |
| node-cron | tokio + cron parser | scheduler dan companion reminder |
| web-push (VAPID) | web-push crate / VAPID manual via jwt + ece | wajib uji kirim nyata ke subscription lama |
| SQLite (file yuki.db) | rusqlite (bundled) | skema dipertahankan |
| file JSON + fs | serde_json + tokio fs | permission 600 untuk secrets dipertahankan |
| msedge-tts | implementasi protokol WebSocket Edge TTS sendiri, atau sidecar | lihat risiko |
| @huggingface/transformers (embedding) | ort (ONNX Runtime) dengan model embedding yang sama | lihat risiko |
| puppeteer-core | chromiumoxide, atau pertahankan layanan browser terpisah | lihat risiko |
| bcrypt/password hashing dan token | cek implementasi Node sekarang per modul security/auth | samakan algoritma agar sesi dan password lama tetap valid |

## Fase pengerjaan

### Fase 0 — Fondasi dan harness paritas — SELESAI 2026-11-09 (lokal VM, lihat parity/FASE-0-HASIL.md)
- [x] Clone repo ke workspace kerja, bekukan commit acuan, catat semua 84 route + contoh respons.
- [x] Scaffold Cargo workspace, config dari env, logging, error type, /api/health.
- [x] Sajikan hasil build frontend Vite sebagai static files.
- [x] Buat harness uji paritas: jalankan test script Node yang relevan sebagai acuan perilaku
      dan skrip pembanding respons (Node di port A, Rust di port B, payload sama).
- Selesai bila: binary jalan, /api/health dan halaman utama identik, harness paritas merah/hijau jelas.
  Hasil: unit test 3/3, harness paritas 13/13 PASS, binary release 3,3 MB.

### Fase 1 — Provider, LLM, dan chat inti

Ruling cakupan (2026-11-09, dicatat saat eksekusi): route /api/chat di Node terjalin dengan
recall memory (Fase 2), scheduler/reminder (Fase 3), dan mode agent (Fase 4). Fase 1 memporting
lapisan provider + llm.chat() + auth/sesi + DB subset (users, chat_history, rate_limits,
relationship_events, facts) dan jalur companion non-streaming dari /api/chat. Yang DITUNDA
dengan sadar ke fasenya masing-masing: mode agent (Rust menjawab 501 untuk mode=agent),
streaming SSE, recall memory/semantic, mesin emosi penuh (react/decay/bond naik — Rust membaca
bond apa adanya dan tidak menulis ulang tabel emotion agar data Node tidak rusak), konteks
game/reminder/evidence, character-quality retry, ekstraksi fakta otomatis, dan recordStrike
IP-block. Konsekuensi yang diterima: selama Fase 1, bond tidak bertambah lewat server Rust
dan balasan chat belum melewati retry kualitas karakter; keduanya kembali di fase terkait.

### Fase 1 — Provider, LLM, dan chat inti (judul asli) — SELESAI 2026-11-09 (lokal VM, lihat parity/FASE-1-HASIL.md)
- [x] Porting lib/provider-settings.js, lib/provider-router.js, lib/llm.js (707 baris):
      primary + satu backup, circuit breaker, cooldown, statistik ke data/provider-stats.json.
- [x] Porting route /api/chat dan /api/chat/history, sesi/auth dasar (lib/security.js).
- [x] Uji: test-provider-router-v32, test-provider-settings-v32, test-security versi paritas.
- Selesai bila: chat end-to-end lewat Rust dengan provider yang sama dan statistik tercatat sama.
  Hasil: unit test Rust 17/17; harness paritas DB-bersama 16/16 PASS termasuk token sesi
  dan kode akses kompatibel dua arah Node<->Rust, balasan chat identik, statistik tercatat.
  Pengecualian sesuai ruling Fase 1 di atas (agent 501, tanpa SSE, bond tidak bermutasi).

### Fase 2 — Memory dan semantic recall

Ruling embedding (2026-11-09, dicatat saat eksekusi): Node memakai
onnx-community/all-MiniLM-L6-v2-ONNX lewat @huggingface/transformers dan versi yang
terunduh di mesin ini adalah model FP32 (onnx/model.onnx + model.onnx_data, 87 MB) —
bukan quantized. Rust memakai file model dan tokenizer.json yang SAMA lewat crate ort
(versi sama seperti xengine-rs) + tokenizers, mean-pooling + normalisasi yang sama,
sehingga vektor di kolom `vec` bisa dibaca silang Node<->Rust. Pencarian folder model:
YUKI_EMBED_DIR, lalu cache unduhan Rust, lalu cache Node di repo original (lokal);
bila tidak ada, Rust mengunduh dari HuggingFace sekali; bila tetap gagal, embedder
nonaktif dan recall jatuh ke jalur tanpa vektor — persis fallback Node. Yang ikut
Fase 2: seluruh DDL tabel memory.js dibuat Rust juga agar DB benar-benar dapat
ditukar; captureMemoryCandidates + captureStructuredMemory + extractFacts disambungkan
ke /api/chat; summarizeAndTrimHistory jalan sebagai tugas latar seperti Node.

### Fase 2 — Memory dan semantic recall (judul asli) — SELESAI 2026-11-09
- [x] Porting lib/memory.js (816) dan lib/agent/memory-store.js ke rusqlite, skema identik.
- [x] Porting lib/semantic.js: embedding lewat ort dengan model yang sama (EMBED_MODEL),
      fallback bila model absen harus sama seperti Node.
- [x] Porting route /api/memories lengkap (CRUD, settings, structured).
- [x] Uji paritas DB bersama (pengganti test-memory-* yang butuh server Node penuh):
      21/21 PASS, cosine vektor Rust vs Node = 1.0000, recall dua arah terbukti.
      Detail: parity/FASE-2-HASIL.md. Binary 45 MB (runtime ONNX).

### Fase 3 — Scheduler, push, agenda, reminder

Ruling eksekusi (2026-11-09): (1) Mesin jadwal Rust memakai SATU ticker 15 detik
(meneladani deliveryTimer Node) + kolom next_run_at_utc untuk enqueue occurrence,
bukan timer cron per-task ala node-cron; pipeline occurrence di DB (enqueue OR
IGNORE, claim 'delivering', delivered/retrying maks 5x backoff 2^n menit, task
run_once -> done) identik. (2) Web Push ditulis native: RFC 8291 (aes128gcm:
ECDH P-256 + HKDF + AES-128-GCM) + VAPID JWT ES256 (crate p256), header TTL 3600
& Urgency high sama; baris subscription dapat ditukar Node<->Rust. (3)
companion-reminder disambungkan ke /api/chat sekarang (konteks "FAKTA PENGENAL"
masuk ChatContext.reminder_context yang sudah ada sejak Fase 1). (4) Uji kirim
push: endpoint lokal tidak lolos allowlist API (memang benar), jadi subscription
uji ditanam langsung di DB seperti baris lama, lalu pengiriman kedua engine ke
mock push server diverifikasi kriptografis (tanda tangan JWT + dekripsi
aes128gcm) memakai Node crypto.

### Fase 3 — Scheduler, push, agenda, reminder (judul asli) — SELESAI 2026-11-09
- [x] Porting lib/scheduler.js (423), lib/push.js, lib/companion-reminder.js.
- [x] Uji kirim Web Push nyata: terverifikasi kriptografis dua arah
      (JWT VAPID valid + aes128gcm terdekripsi) untuk pengiriman Rust
      maupun Node terhadap subscription yang sama.
- [x] Uji paritas DB bersama (pengganti test-scheduler-v32): 20/20 PASS —
      jadwal jalan dua arah, push diterima, agenda CRUD sama.
      Detail: parity/FASE-3-HASIL.md. Binary 47,6 MB.

### Fase 4 — Agent: runner, planner, skills, subagent, workspace

Ruling eksekusi (2026-11-09): (1) Seluruh lib/agent diporting (workspace-policy,
sandbox, run-store, workflow-store, planner, tool-result, observability,
dynamic-skills, subagent, persona, runner) + skills-engine (katalog SKILL.md
dibaca dari folder skills/ yang sama; TOOL_SCHEMAS verbatim) + ke-16 handler
skill. (2) Sandbox JS memakai binary `node` sistem dengan flag --permission +
prlimit yang sama seperti Node (bukan implementasi ulang), dan cek sintaks JS
memakai vm.Script lewat proses node — pesan error V8 identik. (3) Tool browser
(Puppeteer) mengikuti keadaan host: tanpa Chromium, Node sendiri jatuh ke
fetch fallback — Rust melakukan hal yang sama; browse_page memakai fallback
fetch, screenshot/extract mengembalikan error ketidaktersediaan yang sama.
(4) calculate_expression memakai evaluator ekspresi Rust (grammar sama:
angka, operator, %, Math.*) — pesan error V8 untuk ekspresi rusak bisa beda
redaksi, hasil & penolakan karakter identik. (5) Validasi .ts di
validate_workspace_project memakai strip-types node bila tersedia
(node:module stripTypeScriptTypes lewat proses node), fallback heuristik.
(6) Mode agent di Rust tetap TIDAK memutasi tabel emosi (konsisten dengan
ruling Fase 1) — cabang agent Node memanggil emotion.updateFromLLMMood;
selisih ini dicatat sebagai deviasi yang sama, bukan bug baru. (7) Template
universalMobileEngine untuk artifact diekstrak byte-exact dari handler.js
ke aset Rust agar artifact yang dihasilkan identik.

### Fase 4 — Agent: runner, planner, skills, subagent, workspace (judul asli) — SELESAI 2026-11-09
- Fase terberat. Urutan di dalam fase: workspace-policy + sandbox dulu (keamanan),
  lalu run-store/workflow-store, planner, skills-engine (812), dynamic-skills, runner (1.042),
  subagent, observability.
- [x] Kontrak file skills di folder skills/ (kategori computing/learning/media/dll) dipertahankan.
- [x] Uji paritas DB bersama (pengganti test-workspace-security, test-sandbox-escape,
      test-agent-runs, test-agent-planner, test-agent-tools, test-subagents-v32,
      test-skills): 39/39 PASS — katalog 16 skill identik, approval workflow dua
      arah, delegate_tasks, workspace, dynamic skills, cancel/replay run.
- [x] Selesai: run agent durable bisa cancel/resume dan batas budget token/biaya
      bersama bekerja sama. Paritas juga menangkap & memperbaiki bug parser
      jadwal Fase 3 (nol di depan jam) dan gerbang worthRemembering untuk
      extractFacts. Detail: parity/FASE-4-HASIL.md. Binary 50,7 MB.

### Fase 5 — Layanan pinggiran

Ruling eksekusi (2026-11-09): (1) Emotion diporting PENUH (lib/emotion.js:
react/decay/updateFromLLMMood/label/feeling/directive/serialize) dan
disambungkan ke chat companion + agent persis server.js — dengan ini
deviasi Fase 1 gugur: bond ikut bergerak di Rust, disimpan ke tabel
emotion yang sama. Jitter decay memakai rand (Math.random di Node),
jadi angka mood/bond tidak byte-identik antar-run, tapi aturan,
ambang, dan arahnya sama. (2) Games diporting utuh (store + tic-tac-toe
minimax + RPS) dengan gating env yang sama (route hanya ada bila
GAMES_ENABLED=true; /api/game/status selalu ada; beta via
GAMES_PUBLIC/GAMES_BETA_IDS); langkah/komentar Yuki dari LLM dengan
timeout & cadangan yang sama (4,5 dtk minimax, 3,5 dtk acak, jeda
buatan 500 ms di RPS); gameMemoryText masuk konteks chat companion.
(3) Comics: bookmarks CRUD + /api/comic-cover memakai searchComics/
latestComics yang sudah diporting di Fase 4. (4) IP-block: tabel
ip_blocks di DB yang sama, middleware 403, recordStrike (429 &
login gagal; 10 strike/10 menit -> blokir 1 jam, level berikutnya
24 jam) + route admin. (5) Admin lengkap kecuali TTS (tetap Fase 6):
login/logout/password, stats, providers (get/put/test/reset-stats),
overview, db browser (tables/table/users/lookup-key/reset-key/tabel
per-user), workspace browser. Metrics runtime (requests/errors/
latency/chats) ditambah ke AppState meniru metrics server.js.
(6) Ikut serta: POST /api/feedback, DELETE /api/account,
POST /api/analyze-url (fetchSiteEvidence di atas validasi SSRF yang
sudah ada), GET /api/relationship/:userId. Route TTS (/api/tts dan
/api/admin/tts*) TIDAK termasuk — tetap Fase 6.

### Fase 5 — Layanan pinggiran (judul asli) — SELESAI 2026-11-09
- [x] Games (tic-tac-toe, RPS + store): logika murni, cepat.
- [x] Ryukomik/comics (lib/ryukomik.js 207) + route bookmarks dan comic-cover.
- [x] Emotion (458), time, ip-block, admin stats.
- [x] Admin browser dan browser evidence (lib/browser.js 369, lib/admin-browser.js 386):
      admin-browser diporting penuh; browser evidence sebagai fetch fallback
      (sama seperti Node di host tanpa Chromium) — chromiumoxide tidak dipakai.
- [x] Selesai: unit test 29/29, paritas check_parity5.py 64/64 PASS di DB
      bersama (emotion lintas engine, games lintas server, admin penuh,
      ip-block, hapus akun, analyze-url). Deviasi Fase 1 tertutup: bond
      kini bergerak di Rust. Detail: parity/FASE-5-HASIL.md. Binary 52,1 MB.

### Fase 6 — TTS — SELESAI 2026-11-09 (hasil: parity/FASE-6-HASIL.md)

Ruling eksekusi (2026-11-09, perintah "Kerjakan sampai selesai"): (1) Survei
membuktikan TTSQuest di Node hanya env yatim — tidak ada implementasinya di
lib/tts.js; engine nyata hanya edge (default + fallback), voicevox, kokoro.
runEngine('ttsquest') jatuh ke Edge, diporting apa adanya. (2) Edge TTS:
protokol WebSocket diimplementasikan NATIF di Rust dari sumber msedge-tts
yang ter-vendor di node_modules (URL wss + token Sec-MS-GEC SHA-256 +
framing speech.config/ssml/turn.end) — tanpa sidecar Node. Token GEC
diverifikasi vektor lawan implementasi JS; audio diverifikasi lawan mock
server Edge lokal (Node di-patch statiknya ke mock yang sama). Verifikasi
live ke speech.platform.bing.com tertunda ke VPS (egress VM ini terblokir,
curl langsung gagal — sama seperti Node di VM ini). Hook env
YUKI_EDGE_WSS_URL ditambah di Rust khusus untuk pengujian mock itu.
(3) tts-settings diporting penuh termasuk kontrol proses VOICEVOX via
pm2 (jlist/start/stop/startup-autorestart) dan saklar master
voicevoxEnabled (204 untuk /api/tts saat nonaktif). (4) /api/admin/tts/test
di Node nyatanya MENYIMPAN settings gabungan (saveTtsSettings) walau
komentarnya bilang tidak — diporting setia apa adanya.

- [x] Engine eksternal dulu (Kokoro, VOICEVOX, TTSQuest): semua HTTP, porting langsung.
- [x] Edge TTS: putuskan dari hasil spike singkat — implementasi protokol WebSocket sendiri
      vs mempertahankan proses Node kecil khusus Edge TTS sebagai sidecar.
      Keputusan: protokol WS NATIF di Rust (tanpa sidecar), lihat ruling di atas.
- [x] Route /api/tts dan tts-settings (237) dengan format respons audio identik.

Hasil Fase 6: unit test 31/31; paritas check_parity6.py 31/31 (VOICEVOX/Kokoro
byte-identik dua engine, Edge: SSML + audio identik lawan mock untuk klien
Rust dan klien msedge-tts asli, token GEC cocok dengan JS asli dan vektor
Python independen). Binary 53,0 MB.

### Fase 8 — Penutupan sisa (perintah "KERJAKAN TOTAL", 2026-11-09)

Ruling eksekusi: (1) SSE streaming chat diporting penuh — trigger
stream===true + companion + !idle, header SSE Node, event delta
(lewat makeSafeDeltaEmitter: tahan fragmen [emosi/<think/YUKI- di
ujung + sanitizeOutputSecrets per potongan), done (payload final
identik respons JSON), error (generik, setelah SSE terbuka).
callLLMStream: failover HANYA sebelum delta pertama; terputus di
tengah -> teks parsial dipakai; stall 12 dtk, token pertama 15 dtk;
streaming TIDAK dicatat ke statistik provider (sama seperti Node).
Quality guard + seluruh efek samping (history/memory/emotion/
milestone) berjalan sama, retry guard memakai chat non-stream.
(2) Sandbox snippet run_javascript_code diganti mesin QuickJS
TERTANAM (rquickjs): tanpa proses anak, tanpa Node — isolasi lebih
kuat secara konstruksi (tidak ada fs/network/builtin sama sekali),
console ditangkap, hasil lewat penanda yang sama, timeout interupsi
5 dtk, batas memori 128 MB, label sandbox menjadi "quickjs".
Cek sintaks JS memakai kompilasi QuickJS (pesan error mengikuti
QuickJS, bukan V8 — kelas deviasi yang sudah tercatat). Cek sintaks
TS memakai parser tree-sitter-typescript (deteksi node ERROR),
menggantikan stripTypeScriptTypes+vm. run_workspace_tests TETAP
memakai node eksternal bila ada (file tes adalah program Node —
node:test/fs/modul tidak bisa ditiru mesin tertanam dengan setia);
bila node tidak ada, tool mengembalikan error ketidaktersediaan
yang jelas. (3) Browser agent: klien CDP ditulis tangan di atas
tokio-tungstenite yang sudah ada (tanpa chromiumoxide) — deteksi
executable sama (CHROMIUM_PATH, kandidat, ~/.agent-browser),
argumen launch sama, konteks terisolasi per panggilan, validasi
SSRF sama; Chromium diunduh (Chrome for Testing) untuk verifikasi
lokal bila jaringan mengizinkan. (4) Yang memang tidak bisa
ditutup dari VM ini: verifikasi LIVE Edge TTS dan eksekusi cutover
Fase 7 — keduanya di VPS. Push ke GitHub tetap tidak dilakukan
(belum pernah diizinkan).

**SELESAI 2026-11-09:** unit test 31/31, paritas Fase 8 **45/45 PASS**
(streaming SSE byte-identik termasuk failover, stream terputus, dan
penahanan fragmen [emosi: …]; sandbox QuickJS JSTEST1–4; cek sintaks
JS/TS; penjaga SSRF browser). Browser CDP juga terverifikasi
fungsional ke halaman sungguhan (browse/screenshot/extract di
example.com; extract h1 count 0 terbukti benar — halaman itu kini
tanpa h1). Temuan sampingan: bug upstream Node — chat streaming
gagal total membuat proses Node crash (sseSend di luar cakupan,
server.js ~1453); Rust mengirim event error dengan benar. Binary
akhir 42,4 MB; paket deploy diperbarui (total 129 MB). Detail:
parity/FASE-8-HASIL.md.

### Fase 7 — Cutover — PERSIAPAN SELESAI 2026-11-09 (eksekusi di VPS menunggu fhr)

Paket deploy siap di ~/workspace/yuki-ai-rust/deploy/ (binary + model
embedding + dist/public + unit systemd + template env + README-DEPLOY.md
dengan langkah cutover & rollback). Uji standalone lulus: binary dari
paket berjalan dengan env bersih di folder salinan — /api/health ok,
halaman 200, register + chat jalan, fakta seed ter-embedding nyata dari
YUKI_EMBED_DIR paket (vec terisi di DB). Detail:
parity/FASE-7-PERSIAPAN.md. Langkah yang tersisa murni eksekusi di VPS
oleh fhr (Muse tidak punya akses SSH ke VPS):

- [ ] Jalan berdampingan di VPS (Rust di port lain, proxy terbatas) selama masa observasi.
- [ ] Bandingkan log, statistik provider, dan pemakaian RAM/CPU dengan versi Node.
- [ ] Ganti PM2 ke binary Rust (atau systemd), siapkan perintah rollback satu langkah ke Node.
- [ ] Hapus ketergantungan Node di server hanya setelah masa observasi bersih.
      (Catatan: Node tetap dibutuhkan di PATH untuk sandbox JS agent —
      lihat README-DEPLOY; yang dihapus adalah server Node-nya.)

## Risiko utama (jujur)

- Tiga dependensi tanpa padanan Rust sematang Node: Edge TTS, embedding Transformers,
  Puppeteer. Ketiganya diisolasi di fase akhir dengan opsi sidecar agar tidak memblokir inti.
- Auth dan sesi: algoritma hash/token harus sama persis, kalau tidak semua pengguna
  ter-logout dan password admin tidak cocok. Diverifikasi di Fase 1, bukan di akhir.
- Web Push: kunci VAPID dan format enkripsi harus kompatibel dengan subscription lama.
- Estimasi: ini proyek beberapa hari kerja yang terbagi ke banyak sesi, bukan sekali duduk.
  Fase 0–1 adalah potongan terkecil yang berguna dan bisa dibatalkan tanpa rugi bila hasilnya
  tidak memuaskan.

## Yang tidak berubah

- Nama domain, reverse proxy, dan cara deploy dari sisi pengguna.
- Data pengguna, memory, dan file secrets yang sudah ada.
- Frontend dan seluruh tampilan.
