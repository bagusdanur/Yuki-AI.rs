# Fase 6 — TTS — Hasil (2026-11-09, lokal di VM Muse saja)

## Yang diporting

- `src/tts_settings.rs` — port lib/tts-settings.js: TTS_ENGINES
  [edge, voicevox, kokoro, ttsquest], DEFAULTS dari env, clean()
  (validasi engine, speaker clamp 0–200, URL VOICEVOX hanya loopback
  http dan dinormalisasi scheme//host, pm2Name ≤60, edgeVoice ≤120,
  pitch/rate ≤20), file bersama TTS_CONFIG_FILE
  (./.runtime-secrets/tts-settings.json) ditulis atomik mode 0600
  format JSON pretty yang sama. Kontrol VOICEVOX via pm2:
  jlist/start/stop/startup-autorestart (+fallback start/stop),
  probe /version, daftar /speakers. Keunikan Node dipertahankan:
  tanpa file → DEFAULTS utuh (dengan fallbackEngine); file ada →
  hasil clean() (tanpa fallbackEngine). Angka speaker utuh
  diserialkan sebagai integer (14, bukan 14.0). Probe: status HTTP
  menentukan reachable; JSON versi yang gagal parse → version null
  (semantik Node persis).
- `src/tts.rs` — port lib/tts.js: cleanDialogueText (rantai regex
  sama), peta gaya VOICEVOX (speed/pitch/intonation per mood), preset
  Kokoro VTuber per mood, VOICEVOX audio_query→synthesis, Kokoro
  /synthesize timeout 12 dtk, Edge dengan geseran pitch/rate per mood
  dari basis settings. synthesize(): saklar master voicevoxEnabled
  (false → error berkode VOICEVOX_DISABLED), auto-start daemon
  VOICEVOX saat probe gagal, keepAlive=false → daemon dimatikan lagi
  setelah selesai, urutan fallback [engine, edge]. TTSQuest tidak
  punya implementasi di Node (env yatim) — run_engine jatuh ke Edge,
  sama seperti sumbernya.
- `src/edge_tts.rs` — protokol Edge TTS NATIF dari sumber msedge-tts
  ter-vendor: URL wss + TrustedClientToken + Sec-MS-GEC
  (SHA-256("{windowsTicks}{token}") huruf besar, ticks dibulatkan
  300 dtk) + Sec-MS-GEC-Version 1-143.0.3650.96, header UA Edge 143 +
  Origin ekstensi, pesan speech.config (outputFormat
  audio-24khz-48kbitrate-mono-mp3), pesan SSML (template + prosody
  sama, requestId hex 16 byte), pengumpulan frame audio biner setelah
  penanda "Path:audio\r\n" sampai turn.end requestId yang sama,
  percobaan koneksi sampai 3x seperti _send msedge-tts. Batas total
  60 dtk per sintesis (Node tidak punya; pengaman saja). Hook uji
  YUKI_EDGE_WSS_URL mengganti basis URL untuk mock lokal.
- Route: POST /api/tts (sesi, rate 15; validasi teks 1–2000 karakter
  UTF-16 persis Node; VOICEVOX_DISABLED → 204; error lain → 500
  generik) + admin: GET/PUT /api/admin/tts (60/20), voicevox start
  (10), stop (10), status (120), speakers (30), test (10) — test
  MENYIMPAN settings gabungan dulu seperti Node (komentar Node bilang
  sebaliknya; perilaku dipertahankan).

## Bukti

- `cargo build --release` bersih; binary 52.976.656 byte (53,0 MB).
- `cargo test --release` **31/31 PASS** (29 lama + 2 baru: vektor
  token GEC + bentuk SSML).
- Token GEC: vektor hardcoded dari Python hashlib
  (now=1762387200 → CB4577B8...F72537D5) cocok; token live dari
  MsEdgeTTS.generateSecMsGec asli == perhitungan Python jendela yang
  sama (2A9BFF09...6B65886).
- `parity/check_parity6.py` **31/31 PASS** (DB + AUTH_SECRET +
  TTS_CONFIG_FILE bersama; Node 3101, Rust 3102; mock VOICEVOX 50021,
  Kokoro 50030, Edge WS 50031):
  - settings default identik; probe VOICEVOX reachable dua-duanya;
    process state sama (pm2 tidak ada di VM → 'unknown').
  - Validasi /api/tts: 400 teks kosong/>2000 dan 401 tanpa sesi,
    pesan error identik.
  - PUT settings di Rust terbaca Node (file bersama).
  - VOICEVOX: byte audio identik dua engine
    (WAV:speed=1.08 pitch=0.03 int=1.25 speaker=14 untuk mood ceria);
    teks kotor dibersihkan identik ("Halo apa kabar" sampai ke mock).
  - Saklar master mati → 204 kosong dua engine.
  - Speakers + status identik.
  - Start/stop VOICEVOX tanpa pm2 → 503 {ok:false} dua engine.
  - Kokoro byte identik (KOKORO:vtuber_cute:1.08:Halo).
  - Edge: admin test Rust → audio mock utuh; skrip Node memakai
    msedge-tts ASLI (statik WSS_URL di-patch ke mock yang sama) →
    audio byte-identik; log mock membuktikan speech.config
    (outputFormat sama) dan SSML identik dua klien: voice
    id-ID-GadisNeural, pitch +5Hz, rate +4% (mood senang), volume 100,
    input bersih "Halo Yuki, apa kabar?".

## Deviasi / catatan

- Edge TTS LIVE ke speech.platform.bing.com belum terverifikasi dari
  VM ini (egress terblokir; curl langsung gagal — Node pun sama di
  sini). Verifikasi satu kali di VPS lewat /api/admin/tts/test
  setelah deploy (tertulis di README-DEPLOY paket Fase 7).
- Kegagalan paritas pertama (probe Rust "error decoding response
  body") terbukti gabungan bug mock (mengembalikan 0.25.0 tanpa
  kutip JSON) dan ketidaksetiaan port (Node mentolerir JSON versi
  rusak → version null); mock diperbaiki dan port disetarakan.
- Panjang teks /api/tts dihitung UTF-16 seperti Node; admin test
  memotong teks per char (Node per unit UTF-16) — beda hanya untuk
  teks >300 karakter berisi karakter astral.
- Error pm2 (ENOENT dsb.) teksnya berasal dari OS/spawn, bisa beda
  redaksi antar engine; bentuk {ok, state, pm2Name} sama.

Semua server uji dimatikan setelah lari (port 3101/3102/50021/50030/
50031 bersih).
