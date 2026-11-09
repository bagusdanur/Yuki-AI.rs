# Hasil Fase 3 — Scheduler, Web Push, agenda, reminder (2026-11-09, lokal di VM Muse)

## Yang diporting

- `src/timeutil.rs` — port lib/time.js di atas chrono-tz: normalizeTimezone
  (alias WIB/WITA/WIT + validasi IANA), zonedParts, zonedDateTimeToUtc
  (iterasi koreksi seperti implementasi Intl di Node).
- `src/scheduler.rs` — port lib/scheduler.js: parser bahasa natural ->
  cron (termasuk normalisasi "jam 7 pagi" dan jam titik "19.15"),
  estimateNextRun, CRUD task (create idempoten, list, status, snooze,
  update, cancel, reschedule), dan pipeline occurrence yang sama persis
  di DB (enqueue OR IGNORE, claim delivering, delivered, retry backoff).
  Mesinnya beda (ruling): satu ticker 15 detik yang memindai DB
  menggantikan timer node-cron per task — efek samping positif: task yang
  dibuat proses lain tetap terkirim; Node hanya mengirim task yang ia
  jadwalkan sendiri sejak boot (dibuktikan di uji Fase C).
- `src/push.rs` — port lib/push.js: validasi subscription identik
  (allowlist host push resmi, kunci EC 65 byte, anti-hijack endpoint ->
  409), dan pengiriman Web Push native: RFC 8291 aes128gcm (ECDH P-256 +
  HKDF-SHA256 + AES-128-GCM) + JWT VAPID ES256, TTL 3600, Urgency high.
- `src/reminder.rs` — port lib/companion-reminder.js, kini tersambung ke
  /api/chat (konteks "FAKTA PENGENAL" masuk prompt; jadwal dibuat beneran).
- Route: GET /api/agenda, PATCH /api/agenda/{taskId},
  GET /api/chat/reminders, GET /api/push/config,
  POST/DELETE /api/push/subscriptions.
- Pengiriman saat task jatuh tempo meniru callback server.js persis:
  pesan "[YUKI_REMINDER]..." disimpan ke chat_history + push dikirim.
- Client HTTP bersama kini menerima env YUKI_EXTRA_CA_CERT (root CA
  tambahan) — dipakai uji untuk CA lokal; berguna juga untuk CA privat.

## Bukti — paritas `parity/check_parity3.py`: 20/20 PASS

- Push config sama di kedua server; subscription FCM via Rust tersimpan,
  pendaftaran ulang oleh user lain -> 409, host asing -> 400, DELETE ok.
- Reminder dari chat kedua engine membuat task nyata; konteks
  "Pengingat BERHASIL dibuat" muncul di prompt terekam; agenda lintas
  server konsisten (pause/resume/cancel via Rust terbaca Node;
  next_run "besok jam 7 pagi" = 2026-11-10T00:00:00Z persis).
- Pengiriman lintas engine dua arah: task buatan Node dikirim Rust
  (Node dimatikan), task buatan Rust dikirim Node (lewat restore saat
  boot, sesuai arsitektur Node).
- **Kriptografi Web Push diverifikasi untuk kedua engine**: tanda tangan
  JWT VAPID valid terhadap kunci publik, dan body aes128gcm terdekripsi
  benar dengan kunci subscription (mock push HTTPS lokal + CA uji;
  verifikator Node crypto di parity/verify_push.mjs).
- Unit test Rust: 29/29 PASS. Binary release: 47,6 MB.

## Catatan

- Retry occurrence saat callback gagal tidak terpicu di Rust karena
  callback pengiriman bersifat internal dan total (simpan pesan + push
  menelan error per-subscription seperti Node); cabang retry DB tetap
  ada dan dipakai bila claim/deliver dipisah nanti.
- Endpoint push lokal (http/localhost) tidak bisa dipakai lewat API —
  allowlist Node memang menolaknya; subscription uji ditanam langsung
  di DB meniru baris lama, dan kedua engine mengirim ke mock HTTPS
  ber-hostname localhost dengan CA uji.
