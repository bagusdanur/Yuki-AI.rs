# Fase 8 — Penutupan sisa (streaming, sandbox tertanam, browser asli)

Dikerjakan 2026-11-09 atas perintah fhr "KERJAKAN TOTAL". Tiga sisa
terakhir versi Rust ditutup: (1) SSE streaming chat, (2) sandbox JS
agent yang masih memanggil binary `node` eksternal, (3) tool browser
agent yang hanya fallback fetch. Semua lokal di VM Muse; tidak ada
push GitHub dan tidak ada deploy VPS (eksekusi Fase 7 tetap di tangan
fhr).

## Hasil akhir

- `cargo build --release` bersih; binary **42.358.392 byte (42,4 MB)**
  (naik dari 39,6 MB karena QuickJS + tree-sitter + klien CDP).
- `cargo test --release`: **31/31 PASS**.
- Paritas `parity/check_parity8.py`: **45/45 PASS** (DB bersama,
  AUTH_SECRET bersama, mock provider 3201, Node 3101 vs Rust 3102).

## 1. SSE streaming chat

Diporting penuh mengikuti server.js + lib/llm.js:

- Trigger identik: `stream===true` + mode companion + bukan idle.
  Stream + mode agent → JSON biasa; stream + idle → JSON biasa
  (terverifikasi dua engine).
- `callLLMStream`: iterasi provider langsung TANPA router (tanpa
  statistik/breaker/budget — sama seperti Node), timeout token
  pertama 15 dtk, stall 12 dtk, failover HANYA sebelum delta pertama;
  kegagalan di tengah stream → teks parsial dipakai.
- Delta melewati `makeSafeDeltaEmitter`: fragmen `[emosi: …]` yang
  belum lengkap ditahan dan TIDAK pernah terkirim sebagai delta —
  konkatenasi delta kedua engine byte-identik:
  `"Halo juga. Aku denger kok, santai aja ya.\n"`.
- Event `done` berisi payload final yang sama persis dengan respons
  JSON (reply/mood/bond/messageId), setelah seluruh efek samping
  (history, memori, emosi, milestone) — kode post-processing dipakai
  bersama oleh jalur JSON dan jalur stream (`finish_companion`).
- Skenario terverifikasi: delta→done berurutan; failover ke provider
  backup mengalir dari backup; stream terputus di tengah → `done`
  dengan reply parsial "Halo juga."; semua provider mati → Rust
  mengirim tepat satu event `error`
  `{"error":"Terjadi kesalahan internal pada server."}` lalu menutup
  stream dengan bersih.

### Temuan bug upstream Node (penting)

Di server.js, handler catch luar (~baris 1453) mereferensikan
`sseSend` yang didefinisikan DI DALAM blok try (~baris 1006). Saat
chat streaming gagal total, catch itu sendiri melempar
`ReferenceError: sseSend is not defined` dan **seluruh proses Node
crash** (terverifikasi langsung: dump fatal Node v24, koneksi klien
terputus di tengah chunk). Perilaku yang dimaksudkan (mengirim event
error) adalah yang diimplementasikan versi Rust. `original/` adalah
referensi beku dan TIDAK diubah. Cek paritas untuk skenario ini
mencatat divergensi upstream secara eksplisit.

## 2. Sandbox JS tertanam (QuickJS)

`run_javascript_code` (scratchpad) tidak lagi memanggil proses anak:

- Engine QuickJS tertanam (crate `rquickjs`): isolasi lebih kuat
  secara konstruksi — tidak ada fs, jaringan, atau builtin Node sama
  sekali. `console.*` ditangkap; hasil lewat format marker
  `__YUKI_RESULT__` yang sama; batas 5 detik ditegakkan handler
  interupsi; batas memori 128 MB.
- Timer (setTimeout/setInterval) didukung antrean timer sendiri —
  kode async `await new Promise(r => setTimeout(r, 50))` berjalan
  benar (kasus JSTEST2).
- Penolakan promise akibat interupsi diklasifikasikan sebagai
  timeout: pesan tool "eksekusi melebihi batas 5 detik" — selaras
  dengan jalur timeout Node (kasus JSTEST4; teks Node pada VM ini
  adalah artefak kill CPU prlimit, dicatat sebagai deviasi).
- Error user (throw) → string Node-style `Error: <pesan>` + stack
  (kasus JSTEST3: approval 400, workflow 'failed', 'ledak' terbaca).
- Cek sintaks JS: kompilasi QuickJS (konstruksi Function) — boolean
  `valid` selaras V8 pada kasus uji. Cek sintaks TS: parser
  tree-sitter-typescript (deteksi node ERROR), menggantikan
  stripTypeScriptTypes + vm lewat subprocess.
- Label sandbox pada output tool kini `"quickjs"` (Node:
  `"child-process"`) — perbedaan yang disengaja dan tercatat.
- `run_workspace_tests` TETAP memakai binary `node` bila ada (file
  tes workspace adalah program Node: node:test/fs/modul — tidak bisa
  ditiru setia oleh engine tertanam); tanpa Node, tool menjawab tidak
  tersedia dengan jelas.

## 3. Browser agent Chromium asli (CDP tulisan tangan)

Klien CDP ditulis tangan di atas tokio-tungstenite (tanpa crate
chromiumoxide):

- Deteksi executable sama dengan Node (CHROMIUM_PATH, path kandidat,
  ~/.agent-browser/browsers) + argumen peluncuran yang sama; konteks
  terisolasi per panggilan; cookie per user persisten di
  `data/browser-profiles/<userId>/cookies.json` (0600, ≤500 cookie,
  pruning 90 hari) — kontrak yang sama dengan implementasi
  Puppeteer Node.
- Intersepsi request: setiap URL http(s) divalidasi
  validateSafeOutboundUrl; browse_page juga membatalkan
  image/media/font/stylesheet. Paritas penjaga SSRF terverifikasi:
  browse ke 127.0.0.1 ditolak kedua engine dengan pesan
  '[Keamanan Jaringan]: Akses ke IP privat/lokal/metadata cloud
  dilarang.'
- Semantik tunggu ala Puppeteer (domcontentloaded/load/networkidle0/
  networkidle2) — termasuk gerbang frameNavigated agar lifecycle
  about:blank awal tidak memuaskan wait dokumen target (bug nyata
  yang ditemukan dan diperbaiki selama uji fungsional).
- Verifikasi fungsional terhadap halaman sungguhan (Chrome for
  Testing 157.0.8103.38 yang diunduh untuk pengujian lokal, lewat
  relay ke proxy egress):
  - browse_page https://example.com/ → title "Example Domain",
    teks halaman asli, 1 tautan (iana.org "Learn more"),
    durationMs ≈ 3,8 dtk.
  - screenshot_url → base64 JPEG valid (magic `/9j/4AAQSkZJRg…`),
    dataUrl `data:image/jpeg;base64,…`.
  - browser_extract selector "h1" → count 0 — BENAR: example.com
    versi sekarang tidak punya elemen h1 (terverifikasi dari HTML
    mentahnya; hanya `<title>Example Domain</title>`). Ekspresi
    extract yang sama dengan selector "p" mengembalikan 6 teks
    paragraf lewat urutan CDP yang identik (uji node mentah).
- Penyesuaian lingkungan: bila env proxy tanpa kredensial terdeteksi,
  `--proxy-server` diteruskan ke Chromium (Node tidak meneruskan
  apa pun — tanpa ini Chromium tidak punya egress di VM ini);
  `YUKI_BROWSER_IGNORE_CERT_ERRORS=1` adalah opt-in eksplisit untuk
  lingkungan proxy MITM (default mati, sama seperti Puppeteer).
- Perbandingan fungsional Node di VM ini tidak mungkin (Puppeteer
  Node tidak meneruskan proxy → tanpa egress); paritas lintas engine
  bertumpu pada cek SSRF + kontrak hasil yang identik.
- Desain proses-per-panggilan (Node: satu proses browser persisten)
  — kontrak terlihat-user sama (isolasi + cookie persisten), biaya
  ±1–2 detik peluncuran per panggilan tool.

## Deviasi yang disengaja/dicatat

1. Label sandbox `"quickjs"` menggantikan `"child-process"`.
2. `console.log` objek di scratchpad diformat JSON (`{"a":1}`); Node
   memakai util.inspect (`{ a: 1 }`).
3. Cek sintaks JS tertanam membolehkan top-level `return`
   (konstruksi Function); vm.Script Node menolaknya. Kata-kata error
   sintaks mengikuti QuickJS/tree-sitter, bukan V8.
4. Pesan timeout scratchpad = batas dinding 5 detik; Node di VM ini
   menghasilkan artefak kill CPU prlimit ("proses keluar dengan
   kode null").
5. Chromium proses-per-panggilan; flag `--proxy-server` diteruskan
   dari env proxy tanpa kredensial; opt-in abaikan-error-sertifikat
   lewat env (default mati).
6. Skenario stream-gagal-total: Rust mengirim event error; Node
   upstream crash (bug mereka, bukan deviasi implementasi).

## Yang tetap di luar jangkauan VM ini

- Eksekusi cutover Fase 7 di VPS (paket `deploy/` sudah diperbarui
  dengan binary Fase 8, total 129 MB).
- Verifikasi live Edge TTS ke speech.platform.bing.com (di VPS,
  sekali lewat /api/admin/tts/test engine edge).
- Push kode Rust ke repo GitHub — tidak pernah diotorisasi.
