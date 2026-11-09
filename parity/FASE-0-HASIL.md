# Hasil Fase 0 — Migrasi Rust Yuki (lokal, VM Muse)

Tanggal: 2026-11-09. Acuan: bagusdanur/Yuki---AI commit 47910acdfa82f5ae4916be0c53d04153f883ba84 (v4.6.0).
Semua dikerjakan lokal di VM ini. Tidak ada yang di-push dan tidak ada perubahan di VPS.

## Yang dibangun

- `original/` — clone repo Node, commit acuan dibekukan, daftar 84 route di `../routes-node.txt`.
- `yuki-rs/` — scaffold Rust (axum 0.8 + tokio): `config.rs` (env PORT/HOST + path dist/public),
  `error.rs` (bentuk error JSON sama seperti Node), `state.rs` (started_at untuk uptime),
  `routes/health.rs`, `routes/pages.rs`, penyajian statis dist->public di `main.rs`.
- Frontend dibangun dari sumber: `npm ci` (312 paket) + `npm run build` menghasilkan
  `original/dist` 1,2 MB (index.html 0,93 kB, JS 458 kB, CSS 70 kB).
- `check_parity.py` — harness pembanding Node (port 3101) vs Rust (port 3102).

## Bukti

- Unit test Rust: 3/3 lulus (kontrak health, redirect embed 302, ekstensi terlarang 404).
  Redirect awalnya gagal karena axum `Redirect::to()` memakai 303 sedangkan Express 302 —
  diperbaiki dengan membangun respons 302 manual, test kemudian hijau.
- Harness paritas: **13/13 PASS**.
  - /api/health: bentuk JSON sama (status ok + uptime integer).
  - /, /changelog, /docs, /admin, /privacy, /favicon.ico, /manifest.webmanifest:
    body byte-identik dengan Node.
  - /embed.html dan /embed.html?foo=bar: 302 dengan Location identik.
  - /backup.db, path tak dikenal, endpoint API tak dikenal: 404 di keduanya
    (body 404 tidak dibandingkan — bawaan Express berupa HTML, Rust kosong).
- Binary release: 3,3 MB.
- RAM idle terukur: Rust Fase 0 sekitar 4,5 MB RSS; Node (aplikasi penuh) sekitar 304 MB RSS.
  Perbandingan ini TIDAK setara fitur — Rust baru memuat permukaan Fase 0, Node memuat
  seluruh aplikasi termasuk pemanasan model embedding. Angka final menunggu paritas fitur.

## Cara menjalankan ulang

```bash
# Node acuan (port 3101, data dibuang ke /tmp)
cd original && PORT=3101 HOST=127.0.0.1 MEMORY_DB=/tmp/yuki-parity/yuki.db node server.js

# Rust (port 3102)
PORT=3102 HOST=127.0.0.1 yuki-rs/target/release/yuki-rs

# Harness
python3 parity/check_parity.py
```

## Sisa Fase berikutnya

Fase 1 (provider router + LLM + /api/chat) belum dimulai — menunggu keputusan lanjut.
