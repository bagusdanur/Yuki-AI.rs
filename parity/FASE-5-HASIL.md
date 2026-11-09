# Hasil Fase 5 — Layanan Pinggiran (2026-11-09)

Status: **SELESAI** (lokal di VM Muse saja; tidak ada push GitHub / deploy VPS).

## Ringkasan angka

- Unit test Rust: **29/29 PASS** (`cargo test --release`).
- Paritas Node↔Rust: **64/64 PASS** (`parity/check_parity5.py`, run final).
- Binary release: **52.071.240 byte (52,1 MB)** — naik dari 50,7 MB di Fase 4.

## Yang diporting

- **Emotion penuh** (`src/emotion.rs`, port lib/emotion.js): react/decay/
  updateFromLLMMood/rememberEpisode/serialize + baseline per tier bond.
  Disambungkan ke chat companion DAN agent persis server.js — deviasi
  Fase 1 (Rust tidak mengubah tabel emotion) **tertutup**: bond kini
  bergerak di Rust dan dibaca Node dari tabel yang sama.
- **Games** (`src/games.rs` + `src/routes/games.rs`): store sesi/event,
  tic-tac-toe (minimax + slipChance per bond), batu-kertas-gunting;
  gating env identik (route selain /status hanya bila
  GAMES_ENABLED=true; beta via GAMES_PUBLIC/GAMES_BETA_IDS); langkah &
  komentar Yuki dari LLM dengan timeout/cadangan Node (4,5 dtk TTT,
  3,5 dtk RPS, jeda buatan 500 ms RPS); gameMemoryText masuk konteks
  chat companion.
- **Comics**: bookmark CRUD (termasuk insert preferensi terstruktur
  Node) + /api/comic-cover di atas searchComics/latestComics Fase 4.
- **IP-block + recordStrike** (`src/ipblock.rs`): tabel ip_blocks di DB
  bersama, middleware 403 global, strike dari respons 429 (diamati
  middleware) & login gagal; 10 strike/10 menit → blokir 1 jam, level
  berikut 24 jam.
- **Admin lengkap kecuali TTS**: login/logout/password (cookie admin,
  scrypt file bersama), stats, providers get/put/test/reset-stats,
  overview, db browser (tables/table/users/lookup-key/reset-key/tabel
  per-user, redaksi kolom sensitif persis Node), workspace browser
  read-only (AGENT_WORKSPACE_DIR).
- **Lainnya**: POST /api/feedback, DELETE /api/account (urutan hapus
  Node), POST /api/analyze-url (fetchSiteEvidence: redirect manual,
  timeout 10 dtk, penanda techstack), GET /api/relationship/{userId},
  metrics runtime (requests/errors/latency/chats/chatFailures),
  404 JSON untuk /api/* tak dikenal.

## Bukti paritas (check_parity5.py)

DB bersama + AUTH_SECRET sama + mock provider deterministik
(mock_provider5.py); Node 3101, Rust 3102, mock 3201,
GAMES_ENABLED=true + GAMES_PUBLIC=true di kedua server.

- Emotion lintas engine: chat Rust menaikkan bond yang dibaca Node
  lewat /api/relationship; chat Node menaikkan lagi dan dibaca Rust;
  relationship user lain → 403 di kedua engine.
- Games lintas server: sesi TTT dibuat Rust → langkah oleh Node
  (source 'yuki', mock JSON) → langkah oleh Rust → papan identik
  dibaca Node; kotak terisi → 400 sama; RPS batu vs kertas mock →
  yuki_win, stats losses=1 terbaca Node; pilihan invalid → 400 sama;
  komentar ttt/line dari mock identik.
- Bookmarks: simpan Rust → baca Node → simpan Node → hapus Rust;
  validasi 400 sama. Comic-cover pola asing → {image:''} di keduanya.
- Admin: token admin hasil login Rust dipakai di Node (stats);
  overview Rust lengkap (chatTrend 30 hari, providerHealth, alerts);
  providers test ke mock → {ok, reply 'OK'} di kedua engine; PUT oleh
  Node terbaca Rust; db browser + redaksi access_code; lookup-key;
  reset-key oleh Node → kode baru bisa login di Rust; workspace
  browser dua arah; block-ip Rust → terbaca Node → unblock Node →
  kosong di Rust; ganti password admin di Rust (file scrypt bersama)
  → password lama ditolak Node, password baru diterima Node.
- analyze-url ke IP privat → {ok:false} dengan alasan yang sama di
  kedua engine. Hapus akun di Rust → login kode ditolak Node (404) dan
  sesi lama invalid di Rust (401). 404 API identik.

### Satu-satunya "kegagalan" run pertama = ekspektasi harness, bukan produk

Run pertama 63/64: cek "bond hasil Rust terbaca Node" mengharapkan
bond > 0 setelah chat Rust berisi kata romantis ("suka sama kamu").
Di emotion.js, rayuan pada bond < 8 (orang asing) memicu
bondDelta −0,80 (penolakan tsundere) yang meniadakan semua bonus —
terverifikasi langsung pada class Emotion Node: teks yang sama →
bond 0; teks hangat tanpa romantis → bond 0,4. Port Rust setia;
teks harness diganti dan run final 64/64.

## Deviasi yang disadari

- Jitter decay emotion memakai rand (Math.random di Node): angka
  mood/bond tidak byte-identik antar-run; aturan/ambang/arah sama.
- saveSession: Node membandingkan state sebagai string JSON; Rust
  memakai kesetaraan Value (setara, tahan perbedaan urutan kunci
  lintas engine).
- recordStrike untuk 429 diamati dari middleware respons (Node
  memanggilnya di dalam middleware rateLimit) — hasil akhirnya sama;
  strike login gagal dipanggil eksplisit di handler login-code.
- metrics.chatFailures dihitung pada dua jalur 500 handler chat
  (LLM companion & run agent); Node menghitung semua throw di handler.
- analyze-url memakai klien khusus redirect-manual + timeout 10 dtk
  seperti Node; htmlLength dihitung per karakter (Node: unit UTF-16).
- Browser evidence = fetch fallback, sama seperti Node di host tanpa
  Chromium; chromiumoxide tidak diperkenalkan.
- Route TTS (/api/tts, /api/admin/tts*) SENGAJA belum ada — Fase 6.

## Berkas bukti

- `parity/check_parity5.py`, `parity/mock_provider5.py`
- Log & DB run: `/tmp/yuki-parity5/` (server uji sudah dimatikan).
