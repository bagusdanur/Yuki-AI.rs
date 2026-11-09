//! State aplikasi bersama: konfigurasi, waktu mulai (uptime), DB,
//! router provider, klien LLM, dan penghitung giliran sesi chat
//! (meniru `turnCounters` di server.js: reset setelah jeda 6 jam).

use crate::config::Config;
use crate::db::Db;
use crate::llm::Llm;
use crate::provider::router::ProviderRouter;
use crate::push::PushConfig;
use crate::semantic::Embedder;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Client HTTP bersama. Env YUKI_EXTRA_CA_CERT (path PEM) menambahkan root
/// CA ekstra — untuk CA privat/self-hosted di belakang proxy perusahaan.
fn build_http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Ok(path) = std::env::var("YUKI_EXTRA_CA_CERT") {
        if let Ok(pem) = std::fs::read(&path) {
            for cert in reqwest::Certificate::from_pem_bundle(&pem).unwrap_or_default() {
                builder = builder.add_root_certificate(cert);
            }
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

pub const SESSION_GAP_MS: i64 = 6 * 60 * 60 * 1000;

/// Metrics runtime server.js (requests/errors/chats/chatFailures/
/// latencyTotal) untuk /api/admin/stats & overview.
pub struct Metrics {
    pub requests: std::sync::atomic::AtomicI64,
    pub errors: std::sync::atomic::AtomicI64,
    pub chats: std::sync::atomic::AtomicI64,
    pub chat_failures: std::sync::atomic::AtomicI64,
    pub latency_total_ms: std::sync::atomic::AtomicI64,
}

impl Metrics {
    pub fn new() -> Self {
        use std::sync::atomic::AtomicI64;
        Self {
            requests: AtomicI64::new(0),
            errors: AtomicI64::new(0),
            chats: AtomicI64::new(0),
            chat_failures: AtomicI64::new(0),
            latency_total_ms: AtomicI64::new(0),
        }
    }

    pub fn snapshot(&self, started_at_ms: i64, uptime_seconds: u64) -> serde_json::Value {
        use std::sync::atomic::Ordering::Relaxed;
        let requests = self.requests.load(Relaxed);
        let latency = self.latency_total_ms.load(Relaxed);
        serde_json::json!({
            "startedAt": started_at_ms,
            "requests": requests,
            "errors": self.errors.load(Relaxed),
            "chats": self.chats.load(Relaxed),
            "chatFailures": self.chat_failures.load(Relaxed),
            "latencyTotal": latency,
            "uptime": uptime_seconds,
            "averageLatency": if requests > 0 { latency / requests } else { 0 },
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub started_at: Instant,
    pub started_at_ms: i64,
    pub db: Arc<Db>,
    pub provider_router: Arc<ProviderRouter>,
    pub llm: Arc<Llm>,
    pub embedder: Arc<Embedder>,
    pub push: Arc<PushConfig>,
    pub http: reqwest::Client,
    pub turns: Arc<Mutex<HashMap<String, (u64, i64)>>>,
    pub metrics: Arc<Metrics>,
    /// Strike auto-block per IP: (count, window_start_ms, level).
    pub strikes: Arc<Mutex<HashMap<String, (i64, i64, i64)>>>,
}

impl AppState {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let pepper = config.access_pepper();
        let db = Arc::new(Db::open(&config.memory_db, pepper)?);
        let router = Arc::new(ProviderRouter::new(config.provider_stats_file.clone()));
        let llm = Arc::new(Llm::new(router.clone(), config.provider_config_file.clone()));
        Ok(Self {
            config: Arc::new(config),
            started_at: Instant::now(),
            started_at_ms: crate::emotion::now_ms(),
            db,
            provider_router: router,
            llm,
            embedder: Arc::new(Embedder::new()),
            push: Arc::new(PushConfig::from_env()),
            http: build_http_client(),
            turns: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(Metrics::new()),
            strikes: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// State untuk unit test: DB in-memory, file provider di /tmp.
    pub fn for_test(config: Config) -> Self {
        let pepper = config.access_pepper();
        let db = Arc::new(Db::open_memory(pepper).expect("db memori"));
        let router = Arc::new(ProviderRouter::new(std::path::PathBuf::from(
            "/tmp/yuki-rs-test-stats.json",
        )));
        let llm = Arc::new(Llm::new(router.clone(), config.provider_config_file.clone()));
        Self {
            config: Arc::new(config),
            started_at: Instant::now(),
            started_at_ms: crate::emotion::now_ms(),
            db,
            provider_router: router,
            llm,
            embedder: Arc::new(Embedder::new()),
            push: Arc::new(PushConfig::from_env()),
            http: build_http_client(),
            turns: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(Metrics::new()),
            strikes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Hitung giliran sesi seperti Node: jeda > 6 jam = sesi baru.
    pub fn next_turn(&self, user_id: &str, is_idle: bool) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let mut turns = self.turns.lock().unwrap();
        let session_start = match turns.get(user_id) {
            Some((count, last)) if now - *last <= SESSION_GAP_MS => *count,
            _ => 0,
        };
        let current = if is_idle { session_start } else { session_start + 1 };
        if !is_idle {
            turns.insert(user_id.to_string(), (current, now));
        }
        current
    }
}
