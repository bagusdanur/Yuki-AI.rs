# Optimasi pasca-Fase 6 — Hasil (2026-11-09, lokal di VM Muse saja)

Atas persetujuan fhr atas saran nomor 1 (jalur embedding) dan nomor 3
(profil rilis) dari perbandingan performa.

## Perubahan

1. `src/semantic.rs`:
   - Thread inferensi ONNX sekarang mengikuti jumlah core mesin
     (`available_parallelism`), sebelumnya dikunci 2.
   - Pool 2 sesi ONNX (round-robin, masing-masing Mutex sendiri)
     supaya inferensi bersamaan tidak mengantre di satu sesi;
     ukuran pool bisa diatur env `YUKI_EMBED_POOL` (1..=4).
   - Cache embedding teks persis (maks 512 entri, dibersihkan saat
     penuh) — fakta/query berulang tidak diinferensi ulang.
2. `Cargo.toml` profil rilis: `lto = "fat"`, `codegen-units = 1`,
   `strip = true`.

## Bukti

- Build bersih (satu error `use of moved value` pada draf pertama
  diperbaiki dengan kunci cache terpisah). `cargo test --release`
  **31/31 PASS**.
- Regresi paritas Fase 2 (`check_parity2.py`, server disiapkan
  manual seperti lari aslinya): **21/21 PASS**, cosine vektor
  Rust vs Node tetap **1.0000**, himpunan recall identik — semantik
  embedding tidak berubah.
- Binary: 52.976.656 → **39.578.832 byte (53,0 → 39,6 MB, −25%)**.
  Paket deploy diperbarui: total 140 → **127 MB**.

## Benchmark (skrip sama persis seperti pengukuran sebelumnya;
## provider LLM mock lokal; VM bersama, ada variansi antar-lari)

| Ukuran | Node (lari ini) | Rust sebelum | Rust sesudah |
|---|---|---|---|
| Chat /api/chat rata-rata | 59,1 ms | 96,9 ms | **40,0 ms** |
| Chat median | 77,0 ms | 96,8 ms | 45,1 ms |
| Health rata-rata | 3,1 ms | 0,2 ms | 0,3 ms |
| Statis / rata-rata | 1,4 ms | 0,2 ms | 0,4 ms |
| RSS idle | 243 MB | 18 MB | 18 MB |
| RSS setelah kerja | 325 MB | 102 MB | 161 MB |

Kesimpulan jujur:
- Latensi chat Rust turun ±59% (97 → 40 ms) dan kini setara/lebih
  cepat dari Node pada sesi yang sama; angka Node sendiri bergerak
  45–59 ms antar-lari di VM bersama ini, jadi klaimnya "setara,
  cenderung lebih cepat", bukan mutlak.
- Harga pool sesi kedua: RSS kerja naik 102 → 161 MB (sesi ONNX
  kedua tidak berbagi semua halaman bobot). Masih ±2× lebih ringan
  dari Node. Bila RAM VPS sempit, `YUKI_EMBED_POOL=1` mengembalikan
  pemakaian lama dengan cache + thread penuh tetap aktif.
