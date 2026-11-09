//! Port sesi dari `server.js`: token `payload.signature` dengan payload
//! JSON base64url {sub, sv, jti, iat, exp} dan signature HMAC-SHA256
//! (kunci AUTH_SECRET; admin memakai kunci `AUTH_SECRET:admin`).
//! Verifikasi juga memeriksa userExists + session_version + jti aktif
//! di DB, sama seperti Node — token yang diterbitkan Node valid di Rust
//! dan sebaliknya selama AUTH_SECRET dan DB-nya sama.

use crate::state::AppState;
use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const SESSION_COOKIE_NAME: &str = "yuki_session";
pub const ADMIN_COOKIE_NAME: &str = "yuki_admin_session";
pub const SESSION_COOKIE_MAX_AGE: i64 = 30 * 86400;
pub const ADMIN_COOKIE_MAX_AGE: i64 = 12 * 3600;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hmac_base64url(key: &str, payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC menerima kunci berapa pun");
    mac.update(payload.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn sign(secret: &str, payload_json: &serde_json::Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(payload_json.to_string());
    let signature = hmac_base64url(secret, &payload);
    format!("{payload}.{signature}")
}

fn verify_signature(secret: &str, token: &str) -> Option<serde_json::Value> {
    let (payload, signature) = token.split_once('.')?;
    let expected = hmac_base64url(secret, payload);
    // Perbandingan waktu-konstan seperti timingSafeEqual di Node.
    if signature.len() != expected.len()
        || !signature
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// signSession: menerbitkan jti baru dan menyimpannya (rotasi —
/// token lama mati seketika).
pub fn sign_session(state: &AppState, user_id: &str) -> String {
    let (version, _) = state.db.get_session_state(user_id).unwrap_or((1, None));
    let jti = uuid::Uuid::new_v4().to_string();
    let payload = json!({
        "sub": user_id,
        "sv": version,
        "jti": jti,
        "iat": now_ms(),
        "exp": now_ms() + 30 * 86400_000_i64,
    });
    let token = sign(&state.config.auth_secret, &payload);
    state.db.set_session_jti(user_id, &jti);
    token
}

pub fn verify_session(state: &AppState, token: &str) -> Option<String> {
    let data = verify_signature(&state.config.auth_secret, token)?;
    let exp = data["exp"].as_i64()?;
    if exp <= now_ms() {
        return None;
    }
    let sub = data["sub"].as_str()?;
    if !state.db.user_exists(sub) {
        return None;
    }
    let sv = data.get("sv")?.as_i64()?;
    let jti = data.get("jti")?.as_str()?;
    let (version, active_jti) = state.db.get_session_state(sub)?;
    if sv != version {
        return None;
    }
    if active_jti.as_deref() != Some(jti) {
        return None;
    }
    Some(sub.to_string())
}

pub fn sign_admin_session(state: &AppState) -> String {
    let payload = json!({ "role": "admin", "exp": now_ms() + 12 * 3600_000_i64 });
    sign(&format!("{}:admin", state.config.auth_secret), &payload)
}

pub fn verify_admin_session(state: &AppState, token: &str) -> bool {
    let Some(data) = verify_signature(&format!("{}:admin", state.config.auth_secret), token)
    else {
        return false;
    };
    data["role"].as_str() == Some("admin")
        && data["exp"].as_i64().map(|exp| exp > now_ms()).unwrap_or(false)
}

pub fn hash_password(password: &str, salt_hex: &str) -> String {
    // Node: scryptSync(password, salt, 64).toString('hex') dengan parameter
    // default Node (N=16384, r=8, p=1) dan salt sebagai STRING hex mentah.
    let params = scrypt_params();
    let mut out = vec![0u8; 64];
    scrypt::scrypt(password.as_bytes(), salt_hex.as_bytes(), &params, &mut out)
        .expect("parameter scrypt valid");
    crate::db::hex(&out)
}

fn scrypt_params() -> scrypt::Params {
    scrypt::Params::new(14, 8, 1, 64).expect("parameter scrypt default")
}

/// base64url dari N byte acak (setara crypto.randomBytes(n).toString('base64url')).
pub fn random_base64url(bytes: usize) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(&buf)
}

/// String alfanumerik acak (setara Math.random().toString(36).slice(2, 2+n)
/// secara bentuk; dipakai untuk suffix ID artifact).
pub fn random_alnum(n: usize) -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| CHARS[rng.gen_range(0..36) as usize] as char)
        .collect()
}

pub fn random_salt_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::db::hex(&bytes)
}

/// verifyAdminPassword: file hash lebih dulu, lalu ADMIN_PASSWORD env.
pub fn verify_admin_password(state: &AppState, password: &str) -> bool {
    let file = &state.config.admin_password_file;
    if file.exists() {
        let Ok(raw) = std::fs::read_to_string(file) else {
            return false;
        };
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return false;
        };
        let (Some(salt), Some(hash)) = (
            record["salt"].as_str(),
            record["hash"].as_str(),
        ) else {
            return false;
        };
        let candidate = hash_password(password, salt);
        return candidate.len() == hash.len()
            && candidate
                .bytes()
                .zip(hash.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
    }
    let env_password = if !state.config.admin_token.is_empty() {
        state.config.admin_token.clone()
    } else {
        std::env::var("ADMIN_PASSWORD").unwrap_or_default()
    };
    !env_password.is_empty()
        && password.len() == env_password.len()
        && password
            .bytes()
            .zip(env_password.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub fn store_admin_password(state: &AppState, password: &str) -> anyhow::Result<()> {
    let file = &state.config.admin_password_file;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let salt = random_salt_hex();
    let record = json!({ "salt": salt, "hash": hash_password(password, &salt) });
    let tmp = std::path::PathBuf::from(format!("{}.tmp", file.display()));
    std::fs::write(&tmp, record.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, file)?;
    Ok(())
}

// === Cookie ===
fn is_local_host(host: &str) -> bool {
    let re = regex::Regex::new(r"^(127\.0\.0\.1|localhost|\[::1\])(:\d+)?$").unwrap();
    re.is_match(host)
}

pub fn session_cookie_value(host: &str, token: &str, max_age: i64, name: &str, embeddable: bool) -> String {
    let mut parts = vec![
        format!("{name}={}", url_encode(token)),
        "Path=/".to_string(),
        "HttpOnly".to_string(),
        if embeddable {
            "SameSite=None".to_string()
        } else {
            "SameSite=Lax".to_string()
        },
        format!("Max-Age={max_age}"),
    ];
    if !is_local_host(host) || embeddable {
        parts.push("Secure".to_string());
    }
    parts.join("; ")
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

pub fn parse_cookies(header_value: &str) -> Vec<(String, String)> {
    header_value
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            let (name, rest) = part.split_once('=')?;
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), url_decode(rest)))
        })
        .collect()
}

pub fn token_from_request(headers: &axum::http::HeaderMap, cookie_name: &str) -> String {
    if let Some(auth) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let bearer = auth
            .strip_prefix("Bearer ")
            .or_else(|| auth.strip_prefix("bearer "))
            .unwrap_or(auth)
            .trim();
        if !bearer.is_empty() && bearer != "cookie" {
            return bearer.to_string();
        }
    }
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for (name, value) in parse_cookies(cookie) {
            if name == cookie_name {
                return value;
            }
        }
    }
    String::new()
}

/// Extractor axum pengganti requireSession. Gagal = 401 dengan pesan Node.
pub struct SessionUser(pub String);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = token_from_request(&parts.headers, SESSION_COOKIE_NAME);
        match verify_session(state, &token) {
            Some(user_id) => Ok(SessionUser(user_id)),
            None => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Sesi tidak valid atau sudah berakhir. Pulihkan dengan kunci ingatan." })),
            )
                .into_response()),
        }
    }
}

pub struct AdminUser;

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let token = token_from_request(&parts.headers, ADMIN_COOKIE_NAME);
        match verify_admin_session(state, &token) {
            true => Ok(AdminUser),
            false => Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Sesi admin tidak valid atau berakhir." })),
            )
                .into_response()),
        }
    }
}

/// Port rateLimit middleware Node untuk satu route: konsumsi bucket dan
/// kembalikan respons 429 bila habis. recordStrike berjalan di
/// middleware observability (main.rs) yang mengamati status 429,
/// seperti pemanggilan di middleware rateLimit Node (Fase 5).
pub fn check_rate_limit(
    state: &AppState,
    user_or_ip: &str,
    path: &str,
    window_ms: i64,
    max: i64,
) -> Result<i64, Response> {
    let key = format!("{user_or_ip}:{path}");
    let (allowed, remaining, reset_at) = state.db.consume_rate_limit(&key, window_ms, max);
    if allowed {
        Ok(remaining)
    } else {
        let retry_after = ((reset_at - now_ms()) / 1000).max(0);
        Err((
            StatusCode::TOO_MANY_REQUESTS,
            [
                ("X-RateLimit-Remaining", "0".to_string()),
                ("Retry-After", retry_after.to_string()),
            ],
            Json(json!({ "error": "Terlalu banyak permintaan. Tunggu sebentar lalu coba lagi." })),
        )
            .into_response())
    }
}

pub fn host_of(headers: &axum::http::HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

pub async fn require_session_state(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<String, Response> {
    let token = token_from_request(&headers, SESSION_COOKIE_NAME);
    verify_session(&state, &token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Sesi tidak valid atau sudah berakhir. Pulihkan dengan kunci ingatan." })),
        )
            .into_response()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::path::PathBuf;

    fn test_state() -> AppState {
        let config = Config {
            host: "127.0.0.1".into(),
            port: 0,
            dist_dir: PathBuf::from("/tmp/no-dist"),
            public_dir: PathBuf::from("/tmp/no-public"),
            memory_db: PathBuf::from(":memory:"),
            provider_config_file: PathBuf::from("/tmp/no-providers.json"),
            provider_stats_file: PathBuf::from("/tmp/no-stats.json"),
            admin_password_file: PathBuf::from("/tmp/no-admin.json"),
            tts_config_file: PathBuf::from("/tmp/no-tts.json"),
            auth_secret: "secret-uji-yang-panjang".into(),
            admin_token: String::new(),
            is_production: false,
        };
        AppState::for_test(config)
    }

    #[test]
    fn session_roundtrip_and_rotation() {
        let state = test_state();
        let (user_id, _) = state.db.register_user("Uji").unwrap();
        let token1 = sign_session(&state, &user_id);
        assert_eq!(verify_session(&state, &token1).as_deref(), Some(user_id.as_str()));
        // Rotasi: token baru membunuh token lama (jti berganti).
        let token2 = sign_session(&state, &user_id);
        assert!(verify_session(&state, &token1).is_none());
        assert_eq!(verify_session(&state, &token2).as_deref(), Some(user_id.as_str()));
        // Reset versi: semua token mati.
        state.db.bump_session_version(&user_id);
        assert!(verify_session(&state, &token2).is_none());
        // Token rusak ditolak.
        assert!(verify_session(&state, "abc.def").is_none());
    }

    #[test]
    fn admin_session_and_password() {
        let state = test_state();
        let token = sign_admin_session(&state);
        assert!(verify_admin_session(&state, &token));
        assert!(!verify_admin_session(&state, "x.y"));
        let hash = hash_password("rahasia", "0123456789abcdef0123456789abcdef");
        assert_eq!(hash.len(), 128);
        assert_eq!(hash, hash_password("rahasia", "0123456789abcdef0123456789abcdef"));
    }
}
