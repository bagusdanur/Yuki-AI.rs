//! Port `lib/tts-settings.js` (Fase 6): pengaturan TTS runtime
//! (.runtime-secrets/tts-settings.json, format sama seperti Node)
//! + kontrol proses VOICEVOX lewat pm2 (jlist/start/stop) dan probe
//! HTTP engine. Settings dibawa sebagai serde_json::Value agar
//! keunikan Node awet: file yang ada -> clean() (tanpa fallbackEngine),
//! tanpa file -> DEFAULTS utuh (dengan fallbackEngine).

use serde_json::{json, Map, Value};
use std::path::Path;

pub const TTS_ENGINES: [&str; 4] = ["edge", "voicevox", "kokoro", "ttsquest"];

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn env_bool(key: &str, fallback: &str) -> bool {
    env_or(key, fallback).to_lowercase() == "true"
}

/// Angka utuh diserialkan sebagai integer (14, bukan 14.0) seperti
/// JSON.stringify Node atas Number yang bulat.
fn num_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

pub fn defaults() -> Value {
    json!({
        "engine": env_or("TTS_ENGINE", "edge").to_lowercase(),
        "voicevoxSpeaker": num_value(std::env::var("VOICEVOX_SPEAKER").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(14.0)),
        "voicevoxUrl": env_or("VOICEVOX_URL", "http://127.0.0.1:50021"),
        "voicevoxEnabled": env_bool("VOICEVOX_ENABLED", "true"),
        "voicevoxKeepAlive": env_bool("VOICEVOX_KEEP_ALIVE", "true"),
        "voicevoxPm2Name": env_or("VOICEVOX_PM2_NAME", "voicevox"),
        "fallbackEngine": env_or("TTS_FALLBACK_ENGINE", "edge").to_lowercase(),
        "edgeVoice": env_or("EDGE_VOICE", "id-ID-GadisNeural"),
        "edgePitch": env_or("EDGE_PITCH", "+3Hz"),
        "edgeRate": env_or("EDGE_RATE", "+0%"),
    })
}

fn clamp_speaker(value: &Value, fallback: f64) -> f64 {
    let n = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    };
    match n {
        Some(n) if n.is_finite() && (0.0..=200.0).contains(&n) => n.trunc(),
        _ => fallback,
    }
}

fn as_f64(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

/// Port clean(): validasi + normalisasi input terhadap current.
pub fn clean(input: &Value, current: &Value) -> Value {
    let d = defaults();
    let engine_in = input["engine"].as_str().unwrap_or("").to_lowercase();
    let engine = if TTS_ENGINES.contains(&engine_in.as_str()) {
        engine_in
    } else {
        current["engine"].as_str().unwrap_or("edge").to_string()
    };
    let raw_url = input
        .get("voicevoxUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| current["voicevoxUrl"].as_str().map(|s| s.to_string()))
        .unwrap_or_default();
    let trimmed = raw_url.trim();
    let url_in = if trimmed.is_empty() {
        d["voicevoxUrl"].as_str().unwrap().to_string()
    } else {
        trimmed.to_string()
    };
    let url = normalize_voicevox_url(&url_in).unwrap_or_else(|| d["voicevoxUrl"].as_str().unwrap().to_string());
    let bool_or = |key: &str| -> bool {
        // Node: hanya `undefined` (kunci absen) yang jatuh ke current;
        // null eksplisit -> Boolean(null) = false.
        match input.get(key) {
            None => current[key].as_bool().unwrap_or(false),
            Some(v) => js_truthy(v),
        }
    };
    let str_or = |key: &str, current_v: &Value, default_v: &Value, max: usize| -> String {
        let candidate = input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| current_v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| default_v.as_str().unwrap_or("").to_string());
        let t: String = candidate.trim().chars().take(max).collect();
        if t.is_empty() {
            default_v.as_str().unwrap_or("").to_string()
        } else {
            t
        }
    };
    json!({
        "engine": engine,
        "voicevoxSpeaker": num_value(clamp_speaker(
            if input.get("voicevoxSpeaker").is_some() { &input["voicevoxSpeaker"] } else { &Value::Null },
            as_f64(&current["voicevoxSpeaker"]),
        )),
        "voicevoxUrl": url,
        "voicevoxEnabled": bool_or("voicevoxEnabled"),
        "voicevoxKeepAlive": bool_or("voicevoxKeepAlive"),
        "voicevoxPm2Name": str_or("voicevoxPm2Name", &current["voicevoxPm2Name"], &json!("voicevox"), 60),
        "edgeVoice": str_or("edgeVoice", &current["edgeVoice"], &d["edgeVoice"], 120),
        "edgePitch": str_or("edgePitch", &current["edgePitch"], &d["edgePitch"], 20),
        "edgeRate": str_or("edgeRate", &current["edgeRate"], &d["edgeRate"], 20),
    })
}

fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Hanya loopback http yang diizinkan; hasil dinormalisasi
/// scheme//host (path dibuang), persis clean() Node.
fn normalize_voicevox_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    let host = parsed.host_str()?;
    if !["127.0.0.1", "localhost", "::1"].contains(&host) {
        return None;
    }
    if parsed.scheme() != "http" {
        return None;
    }
    // Node memakai URL.host (IPv6 dalam kurung siku, port bila ada).
    let display_host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let host_part = match parsed.port() {
        Some(p) => format!("{display_host}:{p}"),
        None => display_host,
    };
    Some(format!("{}://{}", parsed.scheme(), host_part))
}

pub fn get_tts_settings(file: &Path) -> Value {
    if !file.exists() {
        return defaults();
    }
    match std::fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    {
        Some(parsed) => clean(&parsed, &defaults()),
        None => defaults(),
    }
}

pub fn save_tts_settings(file: &Path, input: &Value) -> Value {
    let current = get_tts_settings(file);
    let mut merged = match current.as_object() {
        Some(m) => m.clone(),
        None => Map::new(),
    };
    if let Some(obj) = input.as_object() {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    }
    let next = clean(&Value::Object(merged), &current);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let tmp = std::path::PathBuf::from(format!("{}.tmp", file.display()));
    let body = serde_json::to_string_pretty(&next).unwrap_or_default();
    if std::fs::write(&tmp, &body).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, file).ok();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600));
        }
    }
    next
}

// === Kontrol proses VOICEVOX via pm2 ===

struct Pm2Result {
    ok: bool,
    stdout: String,
    error: Option<String>,
}

async fn pm2(args: &[&str], timeout_ms: u64) -> Pm2Result {
    let fut = tokio::process::Command::new("pm2")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), fut).await {
        Ok(Ok(out)) => {
            if out.status.success() {
                Pm2Result { ok: true, stdout: String::from_utf8_lossy(&out.stdout).to_string(), error: None }
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let msg = if stderr.trim().is_empty() {
                    format!("pm2 exited with code {}", out.status)
                } else {
                    stderr
                };
                Pm2Result { ok: false, stdout: String::new(), error: Some(msg.chars().take(400).collect()) }
            }
        }
        Ok(Err(e)) => Pm2Result { ok: false, stdout: String::new(), error: Some(e.to_string().chars().take(400).collect()) },
        Err(_) => Pm2Result { ok: false, stdout: String::new(), error: Some("pm2 timeout".to_string()) },
    }
}

pub async fn get_voicevox_process_status(file: &Path) -> Value {
    let name = get_tts_settings(file)["voicevoxPm2Name"].as_str().unwrap_or("voicevox").to_string();
    let res = pm2(&["jlist"], 12_000).await;
    if !res.ok {
        return json!({ "state": "unknown", "error": res.error, "pm2Name": name });
    }
    let parsed: Value = match serde_json::from_str(&res.stdout) {
        Ok(v) => v,
        Err(e) => {
            return json!({ "state": "unknown", "error": e.to_string().chars().take(200).collect::<String>(), "pm2Name": name });
        }
    };
    let proc = parsed
        .as_array()
        .and_then(|list| list.iter().find(|p| p["name"].as_str() == Some(name.as_str())))
        .cloned();
    let Some(proc) = proc else {
        return json!({ "state": "not_found", "pm2Name": name });
    };
    let status = proc["pm2_env"]["status"].as_str().unwrap_or("unknown").to_string();
    let memory = proc["monit"]["memory"].as_f64().map(|m| (m / 1024.0 / 1024.0).round() as i64);
    let uptime = proc["pm2_env"]["pm_uptime"].as_f64().map(|t| crate::emotion::now_ms() - t as i64);
    json!({
        "state": status,
        "pm2Name": name,
        "pid": proc["pid"],
        "restarts": proc["pm2_env"]["restart_time"],
        "memoryMb": memory,
        "cpu": proc["monit"]["cpu"],
        "uptimeMs": uptime,
    })
}

async fn fetch_json(http: &reqwest::Client, url: &str, timeout_ms: u64) -> Result<Value, String> {
    let res = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        http.get(url).send(),
    )
    .await
    .map_err(|_| "timeout".to_string())?
    .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status().as_u16()));
    }
    res.json::<Value>().await.map_err(|e| e.to_string())
}

/// Seperti probeVoicevox Node: reachable ditentukan status HTTP saja;
/// body JSON yang gagal diparse menghasilkan version null, bukan error.
async fn fetch_version(http: &reqwest::Client, url: &str, timeout_ms: u64) -> Result<Value, String> {
    let res = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        http.get(url).send(),
    )
    .await
    .map_err(|_| "The operation timed out".to_string())?
    .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status().as_u16()));
    }
    Ok(res.json::<Value>().await.unwrap_or(Value::Null))
}

pub async fn probe_voicevox(http: &reqwest::Client, settings: &Value) -> Value {
    let base = settings["voicevoxUrl"].as_str().unwrap_or("http://127.0.0.1:50021");
    match fetch_version(http, &format!("{base}/version"), 2500).await {
        Ok(version) => json!({ "reachable": true, "version": version }),
        Err(e) => json!({ "reachable": false, "error": e.chars().take(160).collect::<String>() }),
    }
}

pub async fn start_voicevox(http: &reqwest::Client, file: &Path) -> Value {
    start_voicevox_ms(http, file, 40_000).await
}

pub async fn start_voicevox_ms(http: &reqwest::Client, file: &Path, timeout_ms: u64) -> Value {
    let settings = get_tts_settings(file);
    let name = settings["voicevoxPm2Name"].as_str().unwrap_or("voicevox").to_string();
    let before = get_voicevox_process_status(file).await;
    if before["state"].as_str() != Some("online") {
        let started = pm2(&["start", &name], 20_000).await;
        if !started.ok {
            return json!({
                "ok": false,
                "state": before["state"],
                "error": started.error.unwrap_or_else(|| "gagal start via pm2".to_string()),
            });
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut last_error: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let base = settings["voicevoxUrl"].as_str().unwrap_or("http://127.0.0.1:50021");
        match fetch_version(http, &format!("{base}/version"), 3000).await {
            Ok(version) => {
                return json!({ "ok": true, "state": "online", "version": version, "pm2Name": name });
            }
            Err(e) => {
                last_error = Some(if e.starts_with("HTTP ") { e } else { e.chars().take(160).collect() });
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    }
    json!({
        "ok": false,
        "state": "starting",
        "error": format!("timeout menunggu VOICEVOX siap: {}", last_error.unwrap_or_else(|| "tidak ada respons".to_string())),
        "pm2Name": name,
    })
}

pub async fn stop_voicevox(file: &Path) -> Value {
    let name = get_tts_settings(file)["voicevoxPm2Name"].as_str().unwrap_or("voicevox").to_string();
    let res = pm2(&["stop", &name], 20_000).await;
    if !res.ok {
        return json!({ "ok": false, "error": res.error, "pm2Name": name });
    }
    let after = get_voicevox_process_status(file).await;
    json!({ "ok": after["state"].as_str() != Some("online"), "state": after["state"], "pm2Name": name })
}

pub async fn set_voicevox_autorestart(file: &Path, enabled: bool) -> Value {
    let name = get_tts_settings(file)["voicevoxPm2Name"].as_str().unwrap_or("voicevox").to_string();
    let res = pm2(&["startup-autorestart", &name, if enabled { "true" } else { "false" }], 15_000).await;
    if !res.ok {
        let alt = pm2(&[if enabled { "start" } else { "stop" }, &name], 15_000).await;
        let mut out = json!({ "ok": alt.ok, "pm2Name": name, "mode": "fallback" });
        if let Some(e) = alt.error {
            out["error"] = json!(e);
        }
        return out;
    }
    json!({ "ok": true, "pm2Name": name, "autorestart": enabled })
}

pub async fn list_voicevox_speakers(http: &reqwest::Client, file: &Path) -> Value {
    let settings = get_tts_settings(file);
    let base = settings["voicevoxUrl"].as_str().unwrap_or("http://127.0.0.1:50021");
    match fetch_json(http, &format!("{base}/speakers"), 6000).await {
        Ok(raw) => {
            let mut speakers = Vec::new();
            if let Some(arr) = raw.as_array() {
                for sp in arr {
                    let char_name = sp["name"].as_str().unwrap_or("");
                    if let Some(styles) = sp["styles"].as_array() {
                        for style in styles {
                            let style_name = style["name"].as_str().unwrap_or("");
                            speakers.push(json!({
                                "id": style["id"],
                                "name": format!("{char_name} · {style_name}"),
                                "character": char_name,
                                "style": style_name,
                            }));
                        }
                    }
                }
            }
            json!({ "ok": true, "speakers": speakers })
        }
        Err(e) => json!({ "ok": false, "error": e.chars().take(160).collect::<String>() }),
    }
}
