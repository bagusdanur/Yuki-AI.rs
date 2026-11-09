//! Port `lib/push.js`: Web Push dengan VAPID.
//!
//! Validasi subscription (allowlist host push resmi, anti-hijack endpoint)
//! sama seperti Node. Pengiriman ditulis native mengikuti RFC 8030/8291:
//! enkripsi aes128gcm (ECDH P-256 + HKDF-SHA256 + AES-128-GCM) dan JWT
//! VAPID ES256 — kompatibel dengan subscription yang dibuat versi Node
//! (web-push) dan sebaliknya.

use crate::db::Db;
use crate::state::AppState;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, Key, KeyInit, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use rand::rngs::OsRng;
use rand::RngCore;
use regex::Regex;
use serde_json::{json, Value};
use sha2::Sha256;

#[derive(Clone)]
pub struct PushConfig {
    pub public_key: String,
    pub private_key: String,
    pub subject: String,
    pub configured: bool,
}

impl PushConfig {
    pub fn from_env() -> Self {
        let public_key = std::env::var("VAPID_PUBLIC_KEY").unwrap_or_default();
        let private_key = std::env::var("VAPID_PRIVATE_KEY").unwrap_or_default();
        let subject = std::env::var("VAPID_SUBJECT")
            .unwrap_or_else(|_| "mailto:admin@ryukomik.web.id".to_string());
        Self {
            configured: !public_key.is_empty() && !private_key.is_empty(),
            public_key,
            private_key,
            subject,
        }
    }

    pub fn public_view(&self) -> Value {
        json!({
            "enabled": self.configured,
            "publicKey": if self.configured { self.public_key.clone() } else { String::new() },
        })
    }
}

fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text).ok()
}

fn b64url_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn allowed_push_host(hostname: &str) -> bool {
    let host = hostname.to_lowercase();
    let patterns = [
        r"^fcm\.googleapis\.com$",
        r"^updates\.push\.services\.mozilla\.com$",
        r"^updates\.push\.services\.mozilla\.net$",
        r"^push\.services\.mozilla\.com$",
        r"(?:^|\.)windows\.com$",
        r"(?:^|\.)windows\.net$",
        r"(?:^|\.)notify\.windows\.com$",
        r"(?:^|\.)webpush\.apple\.com$",
        r"(?:^|\.)push\.apple\.com$",
        r"(?:^|\.)web\.push\.apple\.com$",
        r"(?:^|\.)webpush\.us\.east1\.gcp\.cert\.google\.com$",
        r"(?:^|\.)webpush\.googleusercontent\.com$",
        r"(?:^|\.)wns2-[a-z0-9]+\.notify\.windows\.com$",
        r"(?:^|\.)wns2-[a-z0-9]+\.notify\.windows\.net$",
    ];
    patterns.iter().any(|p| Regex::new(p).unwrap().is_match(&host))
}

fn is_valid_base64url(s: &str) -> bool {
    !s.is_empty()
        && s.len() >= 20
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn parse_https_url(endpoint: &str) -> Option<(String, String)> {
    // (origin, hostname) — cukup untuk validasi & aud VAPID.
    let rest = endpoint.strip_prefix("https://")?;
    if rest.contains('@') {
        return None; // userinfo tidak diizinkan
    }
    let host_port = rest.split('/').next()?;
    let hostname = host_port.split(':').next()?.to_lowercase();
    if hostname.is_empty() {
        return None;
    }
    Some((format!("https://{host_port}"), hostname))
}

pub enum SubError {
    Invalid,
    HostNotAllowed,
    OwnedByOther,
}

pub fn validate_subscription(subscription: &Value) -> Result<Value, SubError> {
    let endpoint = subscription["endpoint"].as_str().unwrap_or("");
    let p256dh = subscription["keys"]["p256dh"].as_str().unwrap_or("");
    let auth = subscription["keys"]["auth"].as_str().unwrap_or("");
    let (_origin, hostname) = parse_https_url(endpoint).ok_or(SubError::Invalid)?;
    if !allowed_push_host(&hostname) {
        return Err(SubError::HostNotAllowed);
    }
    if !is_valid_base64url(p256dh) || !is_valid_base64url(auth) {
        return Err(SubError::Invalid);
    }
    let point = b64url_decode(p256dh).ok_or(SubError::Invalid)?;
    if point.len() != 65 || point[0] != 0x04 {
        return Err(SubError::Invalid);
    }
    let auth_bytes = b64url_decode(auth).ok_or(SubError::Invalid)?;
    if auth_bytes.len() < 12 {
        return Err(SubError::Invalid);
    }
    let expiration = subscription.get("expirationTime").cloned().unwrap_or(Value::Null);
    if !expiration.is_null() {
        let value = expiration.as_f64().ok_or(SubError::Invalid)?;
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        if !value.is_finite() || value < 0.0 || value > now_ms + 400.0 * 24.0 * 3600_000.0 {
            return Err(SubError::Invalid);
        }
    }
    Ok(json!({
        "endpoint": endpoint,
        "expirationTime": expiration,
        "keys": { "p256dh": p256dh, "auth": auth },
    }))
}

pub fn save_push_subscription(db: &Db, user_id: &str, subscription: &Value) -> Result<Value, SubError> {
    let normalized = validate_subscription(subscription)?;
    let endpoint = normalized["endpoint"].as_str().unwrap_or("");
    if let Some(owner) = db.subscription_owner(endpoint) {
        if owner != user_id {
            eprintln!("[push] hijack ditolak: endpoint milik user lain");
            return Err(SubError::OwnedByOther);
        }
    }
    db.save_subscription(user_id, endpoint, &normalized.to_string());
    Ok(json!({ "success": true }))
}

pub fn remove_push_subscription(db: &Db, user_id: &str, endpoint: &str) -> Value {
    db.remove_subscription(user_id, endpoint);
    json!({ "success": true })
}

// ===== Pengiriman Web Push (RFC 8291 aes128gcm + VAPID RFC 8292) =====
fn hkdf_expand(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Option<Vec<u8>> {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = vec![0u8; len];
    hk.expand(info, &mut out).ok()?;
    Some(out)
}

fn encrypt_payload(
    ua_public: &[u8],
    auth_secret: &[u8],
    payload: &[u8],
) -> Option<(Vec<u8>, Vec<u8>)> {
    // Kunci ephemeral P-256 untuk sesi ini.
    let ephemeral_secret = SecretKey::random(&mut OsRng);
    let ephemeral_public = ephemeral_secret
        .public_key()
        .as_affine()
        .to_encoded_point(false);
    let ephemeral_public = ephemeral_public.as_bytes();
    let ua_key = PublicKey::from_sec1_bytes(ua_public).ok()?;
    let shared = p256::ecdh::diffie_hellman(
        ephemeral_secret.to_nonzero_scalar(),
        ua_key.as_affine(),
    );
    let shared_bytes = shared.raw_secret_bytes();

    // IKM = HKDF(auth_secret as salt, shared, "WebPush: info\0" || ua || eph)
    let mut key_info = Vec::new();
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(ephemeral_public);
    let ikm = hkdf_expand(auth_secret, shared_bytes, &key_info, 32)?;

    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let cek = hkdf_expand(&salt, &ikm, b"Content-Encoding: aes128gcm\0", 16)?;
    let nonce = hkdf_expand(&salt, &ikm, b"Content-Encoding: nonce\0", 12)?;

    let mut plaintext = payload.to_vec();
    plaintext.push(0x02); // delimiter record terakhir (tanpa padding)
    let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&cek));
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
        .ok()?;

    let mut body = Vec::new();
    body.extend_from_slice(&salt);
    body.extend_from_slice(&4096u32.to_be_bytes()); // record size
    body.push(ephemeral_public.len() as u8);
    body.extend_from_slice(ephemeral_public);
    body.extend_from_slice(&ciphertext);
    Some((body, ephemeral_public.to_vec()))
}

fn vapid_jwt(config: &PushConfig, audience: &str) -> Option<String> {
    let private_bytes = b64url_decode(&config.private_key)?;
    let secret = SecretKey::from_slice(&private_bytes).ok()?;
    let signing = SigningKey::from(&secret);
    let header = b64url_encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let exp = chrono::Utc::now().timestamp() + 12 * 3600;
    let claims = json!({ "aud": audience, "exp": exp, "sub": config.subject });
    let claims_b64 = b64url_encode(claims.to_string().as_bytes());
    let signing_input = format!("{header}.{claims_b64}");
    let signature: p256::ecdsa::Signature = signing.sign(signing_input.as_bytes());
    let sig_b64 = b64url_encode(&signature.to_bytes());
    Some(format!("{signing_input}.{sig_b64}"))
}

pub struct SendSummary {
    pub enabled: bool,
    pub sent: usize,
    pub failed: usize,
}

pub async fn send_push_to_user(state: &AppState, user_id: &str, payload: &Value) -> SendSummary {
    let config = &state.push;
    if !config.configured {
        return SendSummary { enabled: false, sent: 0, failed: 0 };
    }
    let rows = state.db.list_subscriptions(user_id);
    let payload_text = payload.to_string();
    let mut sent = 0usize;
    let mut failed = 0usize;
    for (endpoint, subscription_json, failure_count) in rows {
        let subscription: Value =
            serde_json::from_str(&subscription_json).unwrap_or(Value::Null);
        let result = send_one(state, config, &endpoint, &subscription, &payload_text).await;
        match result {
            Ok(()) => {
                state.db.subscription_mark_success(&endpoint);
                sent += 1;
            }
            Err(status) => {
                failed += 1;
                let gone = status == Some(404) || status == Some(410);
                if gone || failure_count >= 4 {
                    state.db.delete_subscription_by_endpoint(&endpoint);
                } else {
                    state.db.subscription_mark_failure(&endpoint);
                }
            }
        }
    }
    SendSummary { enabled: true, sent, failed }
}

async fn send_one(
    state: &AppState,
    config: &PushConfig,
    endpoint: &str,
    subscription: &Value,
    payload_text: &str,
) -> Result<(), Option<u16>> {
    let ua_public = subscription["keys"]["p256dh"]
        .as_str()
        .and_then(b64url_decode)
        .ok_or(None)?;
    let auth_secret = subscription["keys"]["auth"]
        .as_str()
        .and_then(b64url_decode)
        .ok_or(None)?;
    let (body, _eph) = encrypt_payload(&ua_public, &auth_secret, payload_text.as_bytes()).ok_or(None)?;
    let (origin, _host) = parse_https_url(endpoint).ok_or(None)?;
    let jwt = vapid_jwt(config, &origin).ok_or(None)?;
    let response = state
        .http
        .post(endpoint)
        .header("Authorization", format!("vapid t={jwt}, k={}", config.public_key))
        .header("TTL", "3600")
        .header("Urgency", "high")
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .map_err(|_| None)?;
    let status = response.status().as_u16();
    if (200..300).contains(&(status as i32)) {
        Ok(())
    } else {
        Err(Some(status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist() {
        assert!(allowed_push_host("fcm.googleapis.com"));
        assert!(allowed_push_host("webpush.apple.com"));
        assert!(!allowed_push_host("127.0.0.1"));
        assert!(!allowed_push_host("evil.com"));
    }

    #[test]
    fn validate_rejects_bad_point() {
        let sub = json!({
            "endpoint": "https://fcm.googleapis.com/fcm/send/abc",
            "keys": { "p256dh": "AAAAAAAAAAAAAAAAAAAAAA", "auth": "AAAAAAAAAAAAAAAA" }
        });
        assert!(validate_subscription(&sub).is_err());
    }
}
