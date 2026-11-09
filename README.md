# Yuki AI — versi Rust

Penulisan ulang backend [Yuki AI](https://github.com/bagusdanur/Yuki---AI)
(Node.js/Express) ke satu binary Rust (axum + tokio), dikerjakan sebagai
migrasi strangler per fase dengan uji paritas melawan implementasi Node
di setiap fase. Frontend tetap React/Vite dari repo asli dan dilayani
sebagai file statis.

Semua kontrak dipertahankan: path rute, bentuk JSON, skema `yuki.db`,
format `.runtime-secrets`, dan nama env var — database yang sama bisa
dipakai dua engine bergantian.

## Status

- Fase 0–6 selesai: fondasi, provider/LLM/chat, memori + embedding ONNX
  (all-MiniLM-L6-v2, vektor kompatibel dua arah dengan Node), scheduler
  + Web Push, agent (40 tool, 16 skill), layanan pinggiran + game +
  admin, dan TTS (VOICEVOX/Kokoro/Edge native).
- Fase 7: paket deploy siap di `deploy/` (lihat `README-DEPLOY.md`),
  eksekusi cutover di VPS.
- Fase 8 selesai: SSE streaming chat, sandbox JS tertanam (QuickJS —
  tanpa proses Node), browser agent Chromium asli lewat CDP.
- Bukti paritas per fase ada di `parity/FASE-*-HASIL.md`
  (terakhir: Fase 8, 45/45; unit test 31/31).

## Isi repo

| Folder | Isi |
|---|---|
| `yuki-rs/` | Kode sumber Rust (`src/`, `Cargo.toml`, `assets/`) |
| `parity/` | Harness uji paritas Node vs Rust + catatan hasil per fase |
| `deploy/` | README deploy, unit systemd, template env |
| `RENCANA-MIGRASI-RUST.md` | Rencana migrasi + ruling tiap fase |

Binary hasil build, model embedding, dan frontend ter-build tidak
disimpan di repo ini (lihat `.gitignore`); ketiganya dirangkai saat
deploy mengikuti `deploy/README-DEPLOY.md`.

## Build

```bash
cd yuki-rs
cargo build --release   # menghasilkan target/release/yuki-rs
cargo test --release
```

Runtime membutuhkan folder model embedding (`YUKI_EMBED_DIR`) dan file
env seperti di `deploy/yuki.env.example`. Node hanya dibutuhkan oleh
satu tool agent, `run_workspace_tests`; Chromium bersifat opsional
untuk tool browser agent (tanpanya dipakai fallback fetch).
