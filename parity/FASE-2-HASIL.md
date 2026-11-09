# Hasil Fase 2 — Memory dan semantic recall (2026-11-09, lokal di VM Muse)

## Yang diporting

- `src/semantic.rs` — embedding all-MiniLM-L6-v2 lewat crate `ort` +
  `tokenizers`, memakai file model FP32 dan tokenizer.json yang SAMA dengan
  cache @huggingface/transformers milik Node (mean-pooling + normalisasi
  identik). Pencarian model: YUKI_EMBED_DIR → cache unduhan Rust →
  cache Node (relatif cwd maupun $HOME/workspace); unduhan HF sekali bila
  absen; gagal muat = embedder nonaktif (fallback Node).
- `src/memory.rs` — port memory-store.js + recall memory.js: settings
  auto-capture, agent memories (CRUD + dedupe tampilan + filter
  sampah/injeksi), captureMemoryCandidates, captureStructuredMemory,
  recordConversationEvent, addFacts/addEvent ber-embedding, recallMemory
  (top-K cosine > 0.25, event terbaru, profil, ringkasan, structured
  scoped+legacy, event hubungan, feedback), buildMemoryContext (tanggal
  WIB), summarizeAndTrimHistory (S-4, tugas latar), konsolidasi profil.
- `src/db.rs` — DDL kini lengkap seperti memory.js + memory-store.js
  (events, user_profile, chat_summary, structured_memory, agent_memories,
  memory_settings, response_feedback, game_*, comic_bookmarks, user_notes,
  user_artifacts) agar file DB dapat ditukar Node<->Rust, plus deleteUserData
  dinamis.
- Route: GET /api/memories, PUT /api/memories/settings,
  PATCH/DELETE /api/memories/{id}, DELETE /api/memories/fact/{id},
  DELETE /api/memories/structured/{id}.
- /api/chat kini: recall sebelum menjawab (memoryContext masuk system
  prompt) dan sesudahnya extractFacts → addFacts, capture structured +
  agent, event hubungan, event emosional untuk mood kuat, ringkasan S-4
  dan konsolidasi profil di latar. Register menyimpan fakta benih lewat
  addFacts (ter-embedding).

## Bukti

- Unit test Rust: **21/21 PASS** (17 Fase 1 + 4 memory: filter
  sampah/injeksi, kunci dedupe, cosine/vektor, tanggal WIB).
- Paritas `parity/check_parity2.py` (DB bersama, mock provider yang
  merekam system prompt): **21/21 PASS**, termasuk:
  - recall lintas arah dua-duanya: prompt Rust memuat fakta yang ditulis
    Node, prompt Node memuat fakta yang ditulis Rust;
  - **cosine vektor Rust vs Node untuk teks identik = 1.0000**;
  - himpunan fakta hasil recall untuk query identik sama persis di kedua
    engine (MiniLM memang memberi skor 0.27–0.50 untuk kalimat pendek tak
    berkaitan — terukur dengan embedder Node sendiri — jadi ambang 0.25
    Node memang longgar dan Rust menirunya apa adanya);
  - /api/memories konsisten antar server (daftar fakta sama, settings
    ditulis Rust terbaca Node, DELETE fakta via Rust hilang di Node);
  - structured promise/open_loop dari chat Rust tertangkap.
- Ukuran binary release: 45 MB (naik dari 12 MB karena runtime ONNX).

## Catatan / penyimpangan sadar

- Unduhan model dari HuggingFace di VM ini gagal (egress harus lewat
  proxy; reqwest belum memakai proxy env) — embedder menemukan model dari
  cache Node lokal. Di mesin dengan egress langsung unduhan bekerja;
  YUKI_EMBED_DIR selalu bisa menunjuk folder model secara eksplisit.
- Konsolidasi profil dipanggil sebagai tugas latar dari route (di Node ia
  terpicu dari dalam addFacts, juga tanpa ditunggu).
- deleteUserData belum terekspos route (route /api/account Fase 5).
