# Hasil Fase 1 — Provider, LLM, dan chat inti (lokal, VM Muse)

Tanggal: 2026-11-09. Acuan tetap commit 47910ac (v4.6.0). Semua lokal; tidak ada push, tidak ada perubahan VPS.

## Yang diporting

- `provider/settings.rs` — port provider-settings.js: defaults env (NINEROUTER_*/NEBULA_MODEL),
  validasi baseURL (HTTPS; HTTP hanya localhost; privat ditolak), file
  `.runtime-secrets/llm-providers.json` format sama, simpan atomik mode 0600,
  tampilan publik tanpa apiKey (diganti hasApiKey), orderProviders.
- `provider/router.rs` — port provider-router.js: failover maks 2 percobaan,
  circuit breaker (ambang env YUKI_PROVIDER_CIRCUIT_FAILURES=3, cooldown 60 dtk),
  budget token/biaya, usage/cost, statistik persisten format sama
  (providers/alerts/dailyTotals), snapshot getProviderOperations.
  Penyimpangan kecil: Node menulis stats dengan throttle 5 detik, Rust menulis segera.
- `llm.rs` — port llm.js non-streaming: persona config/persona.js, prompt statis,
  prompt dinamis (WIB, patokan tanggal, varians mood harian deterministik, aturan bond,
  gerbang panggilan sayang deterministik, pool idle), callLLM (payload sama,
  timeout 25 dtk, respons kosong = error, malformed_function_call, tool_calls),
  chat() dengan stripThink, extractEmotion (mapa emosi penuh), sanitizeTextForRelation,
  applyMoodGuardrails, sanitizeOutputSecrets. Plus label() + directive() dari
  emotion.js dihitung dari state tersimpan (tanpa mutasi).
- `db.rs` — subset memory.js di SQLite yang sama (rusqlite): users (+access_code_hash,
  session_version, session_jti), chat_history, rate_limits, emotion (baca),
  relationship_events, facts. Hash kode akses HMAC pepper + jalur upgrade hash lama
  SHA-256 dan plaintext, persis Node.
- `auth.rs` — token sesi format Node (payload base64url {sub, sv, jti, iat, exp} +
  HMAC-SHA256), rotasi jti, sesi admin (`AUTH_SECRET:admin`), password admin scrypt,
  cookie httpOnly (SameSite=None+Secure untuk sesi embed, Lax untuk admin),
  rate limit berbasis tabel rate_limits.
- Route: /api/register, /api/login-code, /api/session/refresh, /api/session/reset,
  /api/chat/history (paging + pencarian), /api/chat jalur companion:
  sanitasi pesan, guard injeksi, target respons, filter konten bond + retry,
  character guard + retry, pemotongan target, simpan riwayat, milestone.

## Bukti

- Unit test Rust: **17/17 lulus** (db, auth/rotasi sesi, settings, router failover +
  circuit + budget, prompt, extractEmotion, guardrails).
- Harness paritas Fase 1 (`parity/check_parity1.py`, provider tiruan di
  `parity/mock_provider.py`, DB dan AUTH_SECRET BERSAMA): **16/16 PASS**, termasuk
  uji silang dua arah:
  - Token yang diterbitkan Node diterima Rust untuk /api/chat, dan sebaliknya.
  - Kode akses hasil register Node bisa login di Rust (hash kompatibel).
  - Balasan chat identik: reply "Halo juga. Aku denger kok, santai aja ya.",
    mood `senang`, bond `orang asing`; bentuk JSON sama persis.
  - Pesan error sama: register kosong 400, tanpa token 401, kode salah 404,
    injeksi 400.
  - File statistik kedua server mencatat panggilan sukses + token.

## Perbedaan yang diketahui (sesuai ruling di rencana)

- bondValue: setelah chat yang sama, Node naik ke 0.25 (emotion.react), Rust tetap 0 —
  mutasi emosi (react/decay/bond) belum diporting; Rust membaca bond apa adanya dan
  tidak menulis tabel emotion.
- Mode agent: Rust menjawab 501 (Fase 4). Streaming SSE: belum (respons JSON biasa).
- Recall memory, konteks game/reminder/evidence, ekstraksi fakta otomatis: fasenya masing-masing.
- Binary release sekarang 12 MB (bertambah reqwest + SQLite terbundel + scrypt).

## Menjalankan ulang

```bash
python3 parity/mock_provider.py 3201 &
# Node 3101 dan Rust 3102 dengan MEMORY_DB + AUTH_SECRET + LLM_PROVIDER_CONFIG_FILE sama
python3 parity/check_parity1.py
```
