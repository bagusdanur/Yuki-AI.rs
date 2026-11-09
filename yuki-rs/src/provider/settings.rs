//! Port `lib/provider-settings.js`.
//!
//! Format file `.runtime-secrets/llm-providers.json` dipertahankan persis:
//! `{ primary: {...}, backup: {...} }` dengan apiKey di dalamnya; tampilan
//! publik tidak pernah mengembalikan apiKey (diganti `hasApiKey`).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSlot {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "baseURL")]
    pub base_url: String,
    pub model: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    #[serde(rename = "inputCostPerMillion", default)]
    pub input_cost_per_million: f64,
    #[serde(rename = "outputCostPerMillion", default)]
    pub output_cost_per_million: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub primary: ProviderSlot,
    pub backup: ProviderSlot,
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

pub fn defaults() -> ProviderSettings {
    let base = env_or("NINEROUTER_BASE_URL", "http://127.0.0.1:20128/v1");
    let key = env_or("NINEROUTER_API_KEY", "local-proxy");
    ProviderSettings {
        primary: ProviderSlot {
            id: "primary".into(),
            name: "9Router · Harbor DeepSeek".into(),
            base_url: base.clone(),
            model: env_or("NEBULA_MODEL", "harbor/deepseek-v4.1-flash:free"),
            api_key: key.clone(),
            input_cost_per_million: 0.0,
            output_cost_per_million: 0.0,
        },
        backup: ProviderSlot {
            id: "backup".into(),
            name: "9Router · Gemini Flash".into(),
            base_url: base,
            model: env_or("NINEROUTER_MODEL", "ag/gemini-3-flash"),
            api_key: key,
            input_cost_per_million: 0.0,
            output_cost_per_million: 0.0,
        },
    }
}

/// Meniru validateBaseURL Node: HTTPS wajib; HTTP hanya localhost;
/// jaringan privat non-lokal ditolak.
pub fn validate_base_url(value: &str) -> anyhow::Result<String> {
    let raw = value.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| anyhow::anyhow!("Base URL wajib HTTPS; HTTP hanya boleh untuk localhost."))?;
    let hostport = rest.split('/').next().unwrap_or("");
    let host = hostport
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(hostport)
        .trim_matches(['[', ']']);
    let local = matches!(host, "127.0.0.1" | "localhost" | "::1");
    if scheme != "https" && !(local && scheme == "http") {
        anyhow::bail!("Base URL wajib HTTPS; HTTP hanya boleh untuk localhost.");
    }
    if !local {
        let private = host.starts_with("10.")
            || host.starts_with("192.168.")
            || (172..=31).any(|_| false)
            || {
                let parts: Vec<i64> = host.split('.').filter_map(|p| p.parse().ok()).collect();
                parts.len() == 4 && parts[0] == 172 && (16..=31).contains(&parts[1])
            };
        if private {
            anyhow::bail!("Private network tidak diizinkan.");
        }
    }
    Ok(raw.trim_end_matches('/').to_string())
}

fn clean_slot(
    value: &serde_json::Value,
    current: &ProviderSlot,
    id: &str,
) -> anyhow::Result<ProviderSlot> {
    let get_str = |key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let get_num = |key: &str| value.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let api_key = {
        let candidate = get_str("apiKey").trim();
        if candidate.is_empty() {
            current.api_key.clone()
        } else {
            candidate.to_string()
        }
    };
    if api_key.is_empty() {
        anyhow::bail!("API key {id} belum diisi.");
    }
    let model: String = get_str("model").trim().chars().take(160).collect();
    if model.is_empty() {
        anyhow::bail!("Model {id} wajib diisi.");
    }
    // Node: baseURL selalu dari value; kosong -> validateBaseURL melempar
    // dan getProviderSettings jatuh ke defaults.
    let base_url = validate_base_url(get_str("baseURL"))?;
    Ok(ProviderSlot {
        id: id.to_string(),
        name: {
            let n = get_str("name").trim();
            let fallback = if n.is_empty() { id } else { n };
            fallback.chars().take(80).collect()
        },
        base_url,
        model,
        api_key,
        input_cost_per_million: get_num("inputCostPerMillion").max(0.0),
        output_cost_per_million: get_num("outputCostPerMillion").max(0.0),
    })
}

/// Port `testProviderConnection` (provider-settings.js): POST chat
/// completion mungil ke slot yang dipilih (cleanSlot atas input admin).
pub async fn test_provider_connection(
    http: &reqwest::Client,
    file: &Path,
    input: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let current = load_settings(file);
    let id = if input["id"].as_str() == Some("backup") { "backup" } else { "primary" };
    let cur = if id == "backup" { &current.backup } else { &current.primary };
    let slot = clean_slot(input, cur, id)?;
    let started = std::time::Instant::now();
    let response = http
        .post(format!("{}/chat/completions", slot.base_url))
        .bearer_auth(&slot.api_key)
        .timeout(std::time::Duration::from_secs(15))
        .json(&serde_json::json!({
            "model": slot.model,
            "messages": [{ "role": "user", "content": "Balas tepat: OK" }],
            "max_tokens": 8,
            "temperature": 0,
        }))
        .send()
        .await?;
    let data: serde_json::Value = response.json().await.unwrap_or(serde_json::json!({}));
    let content = data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if content.is_empty() && data.pointer("/choices/0").is_none() {
        anyhow::bail!("Provider tidak mengembalikan completion.");
    }
    Ok(serde_json::json!({
        "ok": true,
        "latencyMs": started.elapsed().as_millis() as i64,
        "model": slot.model,
        "reply": content.chars().take(80).collect::<String>(),
    }))
}

pub fn load_settings(file: &Path) -> ProviderSettings {
    let defaults = defaults();
    if !file.exists() {
        return defaults;
    }
    match std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    {
        Some(stored) => {
            let empty = serde_json::json!({});
            let primary = clean_slot(stored.get("primary").unwrap_or(&empty), &defaults.primary, "primary");
            let backup = clean_slot(stored.get("backup").unwrap_or(&empty), &defaults.backup, "backup");
            match (primary, backup) {
                (Ok(primary), Ok(backup)) => ProviderSettings { primary, backup },
                _ => {
                    tracing::error!("[provider-settings] config invalid, memakai environment defaults");
                    defaults
                }
            }
        }
        None => defaults,
    }
}

pub fn public_settings(settings: &ProviderSettings) -> serde_json::Value {
    let slot = |s: &ProviderSlot| {
        serde_json::json!({
            "id": s.id,
            "name": s.name,
            "baseURL": s.base_url,
            "model": s.model,
            "hasApiKey": !s.api_key.is_empty(),
            "inputCostPerMillion": s.input_cost_per_million,
            "outputCostPerMillion": s.output_cost_per_million,
        })
    };
    serde_json::json!({ "primary": slot(&settings.primary), "backup": slot(&settings.backup) })
}

pub fn save_settings(file: &Path, input: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let current = load_settings(file);
    let empty = serde_json::json!({});
    let next = ProviderSettings {
        primary: clean_slot(input.get("primary").unwrap_or(&empty), &current.primary, "primary")?,
        backup: clean_slot(input.get("backup").unwrap_or(&empty), &current.backup, "backup")?,
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&next)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, file)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
    }
    Ok(public_settings(&next))
}

pub fn runtime_providers(file: &Path) -> Vec<ProviderSlot> {
    let settings = load_settings(file);
    vec![settings.primary, settings.backup]
}

/// Urutan provider bisa diminta pemanggil (mode agent memilih slot tertentu
/// lebih dulu). Id tak dikenal diabaikan — port `orderProviders` di llm.js.
pub fn order_providers(providers: Vec<ProviderSlot>, order: &[String]) -> Vec<ProviderSlot> {
    if order.is_empty() {
        return providers;
    }
    let mut picked: Vec<ProviderSlot> = Vec::new();
    for id in order {
        if let Some(p) = providers.iter().find(|p| &p.id == id) {
            if !picked.iter().any(|x| x.id == p.id) {
                picked.push(p.clone());
            }
        }
    }
    for p in providers {
        if !picked.iter().any(|x| x.id == p.id) {
            picked.push(p);
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert!(validate_base_url("https://api.example.com/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:20128/v1").is_ok());
        assert!(validate_base_url("http://api.example.com/v1").is_err());
        assert!(validate_base_url("https://192.168.1.10/v1").is_err());
        assert_eq!(
            validate_base_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1"
        );
    }

    #[test]
    fn defaults_and_public_redaction() {
        let settings = defaults();
        let public = public_settings(&settings);
        assert!(public["primary"].get("apiKey").is_none());
        assert_eq!(public["primary"]["hasApiKey"], true);
        assert_eq!(settings.primary.id, "primary");
        assert_eq!(settings.backup.id, "backup");
    }
}
