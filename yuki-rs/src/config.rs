//! Konfigurasi dari environment variable.
//!
//! Nama variabel dan nilai default meniru server Node (`server.js`,
//! `lib/memory.js`, `lib/provider-settings.js`, `lib/provider-router.js`):
//! `PORT` (3000), `HOST` (127.0.0.1), `MEMORY_DB` (./data/yuki.db),
//! `LLM_PROVIDER_CONFIG_FILE` (./.runtime-secrets/llm-providers.json),
//! `YUKI_PROVIDER_STATS_FILE` (./data/provider-stats.json).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    /// Hasil `vite build` dari repo Node (folder `dist`).
    pub dist_dir: PathBuf,
    /// Folder `public` repo Node (halaman statis + aset).
    pub public_dir: PathBuf,
    /// File SQLite bersama dengan versi Node (skema sama).
    pub memory_db: PathBuf,
    /// File konfigurasi provider LLM (format sama seperti Node).
    pub provider_config_file: PathBuf,
    /// File statistik provider (format sama seperti Node).
    pub provider_stats_file: PathBuf,
    /// File password admin ter-hash (scrypt, format sama seperti Node).
    pub admin_password_file: PathBuf,
    /// File pengaturan TTS runtime (TTS_CONFIG_FILE, format sama seperti Node).
    pub tts_config_file: PathBuf,
    /// Rahasia HMAC sesi. Bila env kosong, dibuat acak per boot
    /// (meniru Node: sesi lama invalid setelah restart).
    pub auth_secret: String,
    pub admin_token: String,
    pub is_production: bool,
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3000);
        let host = env_or("HOST", "127.0.0.1");
        // Default: folder sibling dari proyek ini (../original/{dist,public}).
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../original");
        let dist_dir = std::env::var("YUKI_DIST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base.join("dist"));
        let public_dir = std::env::var("YUKI_PUBLIC_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| base.join("public"));
        let memory_db = std::env::var("MEMORY_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data/yuki.db"));
        let provider_config_file = std::env::var("LLM_PROVIDER_CONFIG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./.runtime-secrets/llm-providers.json"));
        let provider_stats_file = std::env::var("YUKI_PROVIDER_STATS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data/provider-stats.json"));
        let admin_password_file = std::env::var("ADMIN_PASSWORD_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./.runtime-secrets/admin-password.json"));
        let tts_config_file = std::env::var("TTS_CONFIG_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./.runtime-secrets/tts-settings.json"));
        let auth_secret = std::env::var("AUTH_SECRET").unwrap_or_else(|_| {
            use rand::RngCore;
            let mut bytes = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut bytes);
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        });
        let is_production = std::env::var("NODE_ENV").as_deref() == Ok("production")
            || std::env::var("ENVIRONMENT").as_deref() == Ok("production");
        Ok(Self {
            host,
            port,
            dist_dir,
            public_dir,
            memory_db,
            provider_config_file,
            provider_stats_file,
            admin_password_file,
            tts_config_file,
            auth_secret,
            admin_token: env_or("ADMIN_TOKEN", ""),
            is_production,
        })
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Pepper hash kode akses (lib/memory.js): env AUTH_SECRET asli,
    /// lalu ACCESS_CODE_PEPPER, lalu default dev Node.
    pub fn access_pepper(&self) -> String {
        if let Ok(v) = std::env::var("AUTH_SECRET") {
            return v;
        }
        if let Ok(v) = std::env::var("ACCESS_CODE_PEPPER") {
            return v;
        }
        "yuki-dev-pepper-change-me".to_string()
    }
}
