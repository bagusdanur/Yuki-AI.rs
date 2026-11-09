# Deploy Yuki Rust ke VPS

Paket ini hasil Fase 7 (persiapan), diperbarui setelah Fase 8: binary +
aset yang dibutuhkan untuk menjalankan Yuki versi Rust berdampingan
dengan versi Node, lalu cutover. Semua kontrak sama dengan Node: skema
`yuki.db`, format `.runtime-secrets`, nama env var — tidak ada migrasi
data.

## Isi paket (129 MB)

| Isi | Ukuran | Fungsi |
|---|---|---|
| `yuki-rs` | 42 MB | Binary server (ONNX Runtime + QuickJS tertanam statis di dalamnya) |
| `embed/` | 87 MB | Model embedding all-MiniLM-L6-v2 (FP32, file yang sama dengan Node) |
| `app/dist` + `app/public` | 2 MB | Frontend React hasil build + halaman statis |
| `yuki-rs.service` | — | Unit systemd |
| `yuki.env.example` | — | Template env |

Tidak perlu install Rust/cargo di VPS. Binary hanya butuh library
standar Linux (glibc/libstdc++), arsitektur x86_64. Sejak Fase 8 inti
server tidak butuh Node sama sekali: sandbox JS agent (scratchpad)
dan cek sintaks JS/TS sudah tertanam di dalam binary. Node di PATH
hanya dibutuhkan oleh SATU tool agent, `run_workspace_tests`, karena
file tes workspace memang program Node (node:test/fs/modul) — tanpa
Node tool itu menjawab tidak tersedia, sisanya normal. Chromium
bersifat opsional: tanpa Chromium, tool browser agent memakai
fallback fetch seperti Node tanpa Chromium; dengan Chromium
(terdeteksi di path standar atau env `CHROMIUM_PATH`) tool
`browse_page`/`screenshot_url`/`browser_extract` berjalan lewat
browser asli (CDP).

## Langkah deploy

1. Upload folder ini ke VPS, mis. `/opt/yuki-rs/`:

   ```
   scp -r deploy/* user@vps:/opt/yuki-rs/
   ```

2. Siapkan data dari instalasi Node lama (salin, jangan pindah dulu):

   ```
   mkdir -p /opt/yuki-rs/data /opt/yuki-rs/.runtime-secrets /opt/yuki-rs/agent-workspace
   cp <folder-yuki-node>/data/yuki.db /opt/yuki-rs/data/
   cp <folder-yuki-node>/.runtime-secrets/*.json /opt/yuki-rs/.runtime-secrets/
   cp -r <folder-yuki-node>/agent-workspace/* /opt/yuki-rs/agent-workspace/  # bila ada
   chmod 600 /opt/yuki-rs/.runtime-secrets/*.json
   ```

3. Buat env: `cp yuki.env.example yuki.env`, isi `AUTH_SECRET` **sama
   persis** dengan instalasi lama (kalau beda, semua sesi login gugur)
   dan kunci VAPID dari `.env` lama. `chmod 600 yuki.env`.

4. Uji berdampingan dulu di port lain (Node lama tetap melayani):

   ```
   PORT=3105 /opt/yuki-rs/yuki-rs   # dengan env dari yuki.env
   curl http://127.0.0.1:3105/api/health
   ```

   Buka lewat tunnel/reverse proxy sementara, login dengan kode akses
   yang ada, coba chat, agenda, dan panel admin.

5. Cutover: hentikan Yuki Node (PM2), arahkan reverse proxy (nginx) ke
   port Rust, pasang systemd:

   ```
   cp yuki-rs.service /etc/systemd/system/
   systemctl daemon-reload && systemctl enable --now yuki-rs
   ```

6. Rollback bila ada masalah: stop `yuki-rs`, nyalakan lagi PM2 Node,
   kembalikan proxy ke port Node. `yuki.db` yang sama bisa langsung
   dipakai Node lagi (kedua engine menulis format yang sama; selama
   uji paritas Fase 1–6 DB dipakai bergantian dua arah tanpa konversi).

## Catatan

- Edge TTS live (ke speech.platform.bing.com) belum terverifikasi dari
  VM pengembangan karena egress-nya terblokir; protokolnya diporting
  byte-faithful dari msedge-tts dan terverifikasi lawan mock + vektor
  token. Di VPS (internet langsung) uji sekali lewat
  `/api/admin/tts/test` dengan engine edge setelah deploy.
- SSE streaming chat sudah diporting penuh (Fase 8, paritas 45/45
  bersama sandbox QuickJS dan browser CDP). Deviasi kecil yang
  tersisa: kata-kata error `calculate_expression`/QuickJS/tree-sitter
  bisa beda dari V8, daftar file workspace terurut nama (Node: urutan
  readdir), format `console.log` objek di scratchpad bergaya JSON
  (Node: util.inspect), dan VOICEVOX diawasi lewat pm2 seperti
  sebelumnya.
- Bila VPS berada di belakang proxy MITM (CA lokal), set
  `YUKI_BROWSER_IGNORE_CERT_ERRORS=1` agar Chromium menerima
  sertifikat proxy; di VPS internet langsung tidak perlu.
- Temuan Fase 8 untuk versi Node lama: ada bug upstream di server.js
  — chat streaming yang gagal total (semua provider mati) membuat
  proses Node CRASH (referensi `sseSend` di luar cakupan). Versi Rust
  mengirim event error dengan benar. Selama masih memakai Node,
  hindari ketergantungan pada streaming saat provider tidak stabil.
