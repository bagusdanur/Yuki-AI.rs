//! Port `lib/semantic.js`: embedding lokal all-MiniLM-L6-v2 (ONNX) untuk
//! recall semantik. Model & tokenizer yang sama dengan yang dipakai
//! @huggingface/transformers di Node (FP32 `onnx/model.onnx`), mean-pooling +
//! normalisasi yang sama, sehingga vektor di DB kompatibel dua arah.
//!
//! Bila model tidak bisa dimuat/diunduh, embedder nonaktif dan `embed`
//! mengembalikan None — persis fallback Node (recall jatuh ke jalur terbaru).

use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tokenizers::Tokenizer;
use tokio::sync::OnceCell;

fn ort_env() -> &'static ort::environment::Environment {
    static ENV: OnceLock<ort::environment::Environment> = OnceLock::new();
    ENV.get_or_init(|| ort::init().build().expect("ONNX environment"))
}

/// Jumlah sesi ONNX paralel. Satu sesi dikunci Mutex per inferensi,
/// jadi pool kecil (default 2) menghilangkan antrean antar-request
/// tanpa menggandakan banyak memori (bobot model berbagi mmap).
/// Bisa diatur lewat YUKI_EMBED_POOL (1..=4).
fn pool_size() -> usize {
    std::env::var("YUKI_EMBED_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|n| n.clamp(1, 4))
        .unwrap_or(2)
}

/// Batas entri cache embedding teks persis (FIFO sederhana).
const CACHE_MAX: usize = 512;

pub struct Embedder {
    model_id: String,
    ready: OnceCell<bool>,
    slots: std::sync::Arc<Vec<Mutex<Option<(Session, Tokenizer)>>>>,
    next_slot: std::sync::atomic::AtomicUsize,
    cache: std::sync::Arc<Mutex<std::collections::HashMap<String, Vec<f32>>>>,
}

impl Embedder {
    pub fn new() -> Self {
        let slots = (0..pool_size()).map(|_| Mutex::new(None)).collect();
        Self {
            model_id: std::env::var("EMBED_MODEL")
                .unwrap_or_else(|_| "onnx-community/all-MiniLM-L6-v2-ONNX".to_string()),
            ready: OnceCell::new(),
            slots: std::sync::Arc::new(slots),
            next_slot: std::sync::atomic::AtomicUsize::new(0),
            cache: std::sync::Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    fn candidate_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Ok(dir) = std::env::var("YUKI_EMBED_DIR") {
            dirs.push(PathBuf::from(dir));
        }
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(
                PathBuf::from(home)
                    .join(".cache/yuki-rs/embed")
                    .join(&self.model_id),
            );
        }
        // Cache milik Node di repo original (layout workspace lokal):
        // dicoba relatif terhadap cwd maupun terhadap $HOME/workspace,
        // karena server bisa dijalankan dari direktori mana pun.
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(
                cwd.join("../original/node_modules/@huggingface/transformers/.cache")
                    .join(&self.model_id),
            );
        }
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(
                PathBuf::from(home)
                    .join("workspace/yuki-ai-rust/original/node_modules/@huggingface/transformers/.cache")
                    .join(&self.model_id),
            );
        }
        dirs
    }

    fn find_model_dir(&self) -> Option<PathBuf> {
        self.candidate_dirs()
            .into_iter()
            .find(|dir| dir.join("onnx/model.onnx").exists() && dir.join("tokenizer.json").exists())
    }

    async fn download_model(&self, target: &PathBuf) -> Result<(), String> {
        let base = format!("https://huggingface.co/{}/resolve/main", self.model_id);
        let files = ["onnx/model.onnx", "onnx/model.onnx_data", "tokenizer.json"];
        let client = reqwest::Client::new();
        for rel in files {
            let dest = target.join(rel);
            if dest.exists() {
                continue;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let resp = client
                .get(format!("{base}/{rel}"))
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {} untuk {rel}", resp.status()));
            }
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
            std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    async fn init(&self) -> bool {
        let dir = match self.find_model_dir() {
            Some(dir) => dir,
            None => {
                // Unduh sekali ke cache Rust, seperti transformers.js mengunduh
                // ke cache-nya sendiri.
                let target = self.candidate_dirs().into_iter().nth(1);
                match target {
                    Some(target) => match self.download_model(&target).await {
                        Ok(()) => target,
                        Err(err) => {
                            eprintln!("[semantic] embedding nonaktif (unduh model gagal): {err}");
                            return false;
                        }
                    },
                    None => return false,
                }
            }
        };
        let model_path = dir.join("onnx/model.onnx");
        let tokenizer_path = dir.join("tokenizer.json");
        let pool = self.slots.len();
        let loaded = tokio::task::spawn_blocking(move || {
            let mut tokenizer = Tokenizer::from_file(&tokenizer_path).ok()?;
            let _ = tokenizer.with_truncation(Some(tokenizers::TruncationParams {
                max_length: 512,
                strategy: tokenizers::TruncationStrategy::LongestFirst,
                stride: 0,
                direction: tokenizers::TruncationDirection::Right,
            }));
            // Thread inferensi mengikuti jumlah core mesin (dulu
            // dikunci 2, membuat satu inferensi jauh lebih lambat dari
            // pipeline Node). Antar-sesi berbagi core hanya saat
            // beberapa inferensi berjalan bersamaan.
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2);
            let mut slots = Vec::with_capacity(pool);
            for _ in 0..pool {
                let session = Session::builder(ort_env())
                    .ok()?
                    .with_intra_threads(cores)
                    .ok()?
                    .commit_from_file(&model_path)
                    .ok()?;
                slots.push((session, tokenizer.clone()));
            }
            Some(slots)
        })
        .await
        .ok()
        .flatten();
        match loaded {
            Some(slots) => {
                for (slot, pair) in self.slots.iter().zip(slots.into_iter()) {
                    *slot.lock().unwrap() = Some(pair);
                }
                true
            }
            None => {
                eprintln!("[semantic] embedding nonaktif (model gagal di-load)");
                false
            }
        }
    }

    pub async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        if let Some(hit) = self.cache.lock().unwrap().get(&text) {
            return Some(hit.clone());
        }
        let ready = *self.ready.get_or_init(|| async { self.init().await }).await;
        if !ready {
            return None;
        }
        let slot_idx = self
            .next_slot
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.slots.len();
        let slots = self.slots.clone();
        let cache = self.cache.clone();
        let cache_key = text.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut guard = slots[slot_idx].lock().unwrap();
            let (session, tokenizer) = guard.as_mut()?;
            let encoding = tokenizer.encode(text, true).ok()?;
            let ids: Vec<i64> = encoding.get_ids().iter().map(|v| *v as i64).collect();
            let mask: Vec<i64> = encoding
                .get_attention_mask()
                .iter()
                .map(|v| *v as i64)
                .collect();
            let types: Vec<i64> = encoding.get_type_ids().iter().map(|v| *v as i64).collect();
            let seq = ids.len();
            if seq == 0 {
                return None;
            }
            let inputs = ort::inputs![
                "input_ids" => Tensor::from_array(([1usize, seq], ids)).ok()?,
                "attention_mask" => Tensor::from_array(([1usize, seq], mask.clone())).ok()?,
                "token_type_ids" => Tensor::from_array(([1usize, seq], types)).ok()?,
            ];
            let outputs = session.run(inputs).ok()?;
            let hidden = outputs["last_hidden_state"]
                .try_extract_array::<f32>()
                .ok()?;
            let shape = hidden.shape().to_vec();
            if shape.len() != 3 {
                return None;
            }
            let dim = shape[2];
            let mut pooled = vec![0f32; dim];
            let mut total_mask = 0f32;
            for (i, m) in mask.iter().enumerate() {
                if *m == 0 {
                    continue;
                }
                total_mask += 1.0;
                for d in 0..dim {
                    pooled[d] += hidden[[0, i, d]];
                }
            }
            if total_mask == 0.0 {
                return None;
            }
            for v in pooled.iter_mut() {
                *v /= total_mask;
            }
            let norm = pooled.iter().map(|v| v * v).sum::<f32>().sqrt();
            if norm == 0.0 {
                return None;
            }
            for v in pooled.iter_mut() {
                *v /= norm;
            }
            Some(pooled)
        })
        .await
        .ok()
        .flatten();
        if let Some(vec) = &result {
            let mut cache = cache.lock().unwrap();
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
            cache.insert(cache_key, vec.clone());
        }
        result
    }

    pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
        crate::memory::cosine_sim(a, b)
    }
}
