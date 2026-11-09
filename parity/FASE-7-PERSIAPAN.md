# Fase 7 — Cutover — Persiapan (2026-11-09, lokal di VM Muse saja)

Eksekusi cutover ada di VPS milik fhr; Muse tidak punya akses SSH ke
VPS itu, jadi Fase 7 diselesaikan sejauh yang bisa dibuktikan lokal:
paket deploy utuh + uji standalone dari paket itu sendiri.

## Paket: ~/workspace/yuki-ai-rust/deploy/ (140 MB)

- `yuki-rs` — binary release 53,0 MB (ONNX Runtime tertanam statis;
  ldd hanya library standar Linux: libc/libm/libstdc++/libgcc_s).
- `embed/` — model all-MiniLM-L6-v2 FP32 (87 MB, file yang sama
  persis dengan cache Node: onnx/model.onnx + tokenizer.json).
- `app/dist` (1,2 MB) + `app/public` (740 KB) — frontend.
- `yuki-rs.service` — unit systemd (EnvironmentFile + WorkingDirectory).
- `yuki.env.example` — template env; nama variabel terverifikasi dari
  kode (PORT, HOST, MEMORY_DB, LLM_PROVIDER_CONFIG_FILE,
  YUKI_PROVIDER_STATS_FILE, ADMIN_PASSWORD_FILE, TTS_CONFIG_FILE,
  YUKI_DIST_DIR, YUKI_PUBLIC_DIR, YUKI_EMBED_DIR, AGENT_WORKSPACE_DIR,
  YUKI_AGENT_WORKSPACE, AUTH_SECRET, VAPID_PUBLIC_KEY,
  VAPID_PRIVATE_KEY, VAPID_SUBJECT, GAMES_ENABLED, GAMES_PUBLIC,
  KOKORO_URL).
- `README-DEPLOY.md` — langkah: upload → salin yuki.db +
  .runtime-secrets + agent-workspace dari instalasi Node → env
  (AUTH_SECRET wajib sama) → uji berdampingan di port lain →
  cutover systemd + proxy → rollback satu langkah ke PM2 Node (DB
  yang sama langsung bisa dipakai Node lagi, terbukti dua arah di
  paritas Fase 1–6).

## Uji standalone (dari salinan paket di /tmp, env dibersihkan)

Binary dijalankan dari salinan `deploy/` dengan `env -i` (tanpa
proxy, tanpa PATH cargo/node selain /usr/bin), semua path menunjuk
ke dalam folder salinan, provider LLM = mock lokal:

- GET /api/health → {"status":"ok"}; GET / → 200 (index dari dist).
- POST /api/register → sessionToken terbit.
- POST /api/chat → balasan mock provider, mood "senang", bondValue
  bergerak 0.25 dan milestone first_meeting tercatat (emotion live).
- Fakta seed "Nama user adalah UjiDeploy" tersimpan di facts dengan
  vec TERISI (bukan null) → embedder ONNX memuat model dari
  YUKI_EMBED_DIR paket dan menghasilkan vektor dalam lingkungan
  bersih tanpa cache node_modules.

Server uji dimatikan dan folder uji dihapus sesudahnya.

## Yang tersisa (di tangan fhr di VPS)

1. Upload paket, ikuti README-DEPLOY langkah 1–4 (berdampingan).
2. Uji Edge TTS live sekali via /api/admin/tts/test (engine edge) —
   satu-satunya jalur yang belum terverifikasi live karena egress
   VM pengembangan terblokir.
3. Observasi singkat (log, statistik provider, RAM/CPU), lalu
   cutover systemd + proxy; rollback tetap tersedia selama DB dan
   .runtime-secrets tidak diubah formatnya (tidak berubah).
