//! Port `lib/tts.js` (Fase 6): pembersih teks dialog, engine VOICEVOX
//! (HTTP), Kokoro (HTTP), dan Edge TTS (protokol WS natif di
//! edge_tts.rs), plus pemilih engine synthesize() dengan saklar master
//! VOICEVOX dan fallback ke Edge persis Node. TTSQuest tidak punya
//! implementasi di Node (env yatim) — run_engine-nya jatuh ke Edge,
//! sama seperti sumbernya.

use crate::tts_settings as ts;
use serde_json::{json, Value};
use std::path::Path;

pub enum TtsFailure {
    /// VOICEVOX dimatikan dari dashboard (kode VOICEVOX_DISABLED Node).
    Disabled,
    Error(String),
}

pub const DISABLED_MESSAGE: &str = "VOICEVOX dimatikan dari dashboard — suara nonaktif.";

fn re(pattern: &str) -> regex::Regex {
    regex::Regex::new(pattern).unwrap()
}

/// Port cleanDialogueText.
pub fn clean_dialogue_text(text: &str) -> String {
    let mut s = text.to_string();
    s = re(r"\*[^*\n]{1,150}\*").replace_all(&s, "").to_string();
    s = re(r"(?i)\[emosi:\s*[^\]]+\]").replace_all(&s, "").to_string();
    s = re(r"(?i)https?://\S+").replace_all(&s, "").to_string();
    s = re(r"(?s)```[\s\S]*?```").replace_all(&s, "").to_string();
    s = re(r"`[^`]+`").replace_all(&s, "").to_string();
    s = re(r"[*_#~>]").replace_all(&s, "").to_string();
    s = re(r"\s+").replace_all(&s, " ").trim().to_string();
    if s.is_empty() {
        "Halo".to_string()
    } else {
        s
    }
}

fn mood_of(mood: &str) -> String {
    if mood.is_empty() {
        "tenang".to_string()
    } else {
        mood.to_lowercase()
    }
}

fn map_voicevox_style(mood: &str) -> (f64, f64, f64) {
    let m = mood_of(mood);
    match m.as_str() {
        "ceria" | "senang" => (1.08, 0.03, 1.25),
        "sayang/manja" => (0.95, 0.01, 1.22),
        "malu" | "cemburu" | "kesal" => (1.05, 0.05, 1.28),
        "sedih" | "lesu" | "kecewa" => (0.9, -0.03, 1.1),
        _ => (1.0, 0.0, 1.15),
    }
}

fn map_kokoro_preset(mood: &str) -> (&'static str, f64) {
    let m = mood_of(mood);
    match m.as_str() {
        "ceria" | "senang" => ("vtuber_cute", 1.08),
        "sayang/manja" => ("vtuber_manja", 0.98),
        "malu" | "cemburu" | "kesal" => ("vtuber_tsundere", 1.05),
        "sedih" | "lesu" => ("vtuber_calm", 0.92),
        _ => ("vtuber_cute", 1.04),
    }
}

async fn voicevox_tts(
    http: &reqwest::Client,
    text: &str,
    mood: &str,
    settings: &Value,
) -> Result<(Vec<u8>, String), String> {
    let cleaned = clean_dialogue_text(text);
    let (speed, pitch, intonation) = map_voicevox_style(mood);
    let speaker = settings["voicevoxSpeaker"].as_f64().unwrap_or(14.0) as i64;
    let base = settings["voicevoxUrl"].as_str().unwrap_or("http://127.0.0.1:50021");
    let query_url = format!(
        "{base}/audio_query?text={}&speaker={speaker}",
        urlencoding(&cleaned)
    );
    let query_res = http
        .post(&query_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !query_res.status().is_success() {
        return Err(format!("VOICEVOX audio_query gagal (HTTP {})", query_res.status().as_u16()));
    }
    let mut audio_query: Value = query_res.json().await.map_err(|e| e.to_string())?;
    audio_query["speedScale"] = json!(speed);
    audio_query["pitchScale"] = json!(pitch);
    audio_query["intonationScale"] = json!(intonation);
    let synth_res = http
        .post(format!("{base}/synthesis?speaker={speaker}"))
        .json(&audio_query)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !synth_res.status().is_success() {
        return Err(format!("VOICEVOX synthesis gagal (HTTP {})", synth_res.status().as_u16()));
    }
    let buffer = synth_res.bytes().await.map_err(|e| e.to_string())?;
    Ok((buffer.to_vec(), "audio/wav".to_string()))
}

fn urlencoding(s: &str) -> String {
    // encodeURIComponent JS.
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn kokoro_tts(
    http: &reqwest::Client,
    text: &str,
    mood: &str,
) -> Result<(Vec<u8>, String), String> {
    let cleaned = clean_dialogue_text(text);
    let (preset, speed) = map_kokoro_preset(mood);
    let base = std::env::var("KOKORO_URL").unwrap_or_else(|_| "http://127.0.0.1:50030".to_string());
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        http.post(format!("{base}/synthesize"))
            .json(&json!({ "text": cleaned, "preset": preset, "speed": speed, "lang": "id" }))
            .send(),
    )
    .await
    .map_err(|_| "Kokoro timeout".to_string())?
    .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("Kokoro service HTTP error {}", res.status().as_u16()));
    }
    let buffer = res.bytes().await.map_err(|e| e.to_string())?;
    Ok((buffer.to_vec(), "audio/wav".to_string()))
}

fn parse_number_prefix(v: &Value, fallback: i64) -> i64 {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    re(r"-?\d+")
        .find(&s)
        .and_then(|m| m.as_str().parse::<i64>().ok())
        .unwrap_or(fallback)
}

async fn edge_tts(text: &str, mood: &str, settings: &Value) -> Result<(Vec<u8>, String), String> {
    let cleaned = clean_dialogue_text(text);
    let voice = settings["edgeVoice"].as_str().unwrap_or("id-ID-GadisNeural");
    let mut pitch = parse_number_prefix(&settings["edgePitch"], 3);
    let mut rate = parse_number_prefix(&settings["edgeRate"], 0);
    if !mood.is_empty() {
        let m = mood.to_lowercase();
        match m.as_str() {
            "ceria" | "senang" => {
                pitch += 2;
                rate += 4;
            }
            "sayang/manja" => {
                pitch += 1;
                rate -= 2;
            }
            "malu" | "cemburu" => {
                pitch += 2;
                rate += 2;
            }
            "kesal" => {
                rate += 6;
            }
            "sedih" | "kecewa" => {
                pitch -= 3;
                rate -= 6;
            }
            _ => {}
        }
    }
    let pitch_str = format!("{}{}Hz", if pitch >= 0 { "+" } else { "" }, pitch);
    let rate_str = format!("{}{}%", if rate >= 0 { "+" } else { "" }, rate);
    let audio = crate::edge_tts::synthesize_edge(&cleaned, voice, &pitch_str, &rate_str).await?;
    Ok((audio, "audio/mpeg".to_string()))
}

async fn run_engine(
    http: &reqwest::Client,
    engine: &str,
    text: &str,
    mood: &str,
    settings: &Value,
) -> Result<(Vec<u8>, String), String> {
    match engine {
        "voicevox" => voicevox_tts(http, text, mood, settings).await,
        "kokoro" => kokoro_tts(http, text, mood).await,
        _ => edge_tts(text, mood, settings).await,
    }
}

/// Port synthesize(): saklar master VOICEVOX, auto-start daemon,
/// keepAlive, dan fallback [engine, edge].
pub async fn synthesize(
    http: &reqwest::Client,
    file: &Path,
    text: &str,
    mood: &str,
    settings_override: Option<Value>,
) -> Result<(Vec<u8>, String), TtsFailure> {
    let settings = settings_override.unwrap_or_else(|| ts::get_tts_settings(file));
    let engine = settings["engine"]
        .as_str()
        .unwrap_or("edge")
        .to_lowercase();

    if engine == "voicevox" && settings["voicevoxEnabled"].as_bool() == Some(false) {
        return Err(TtsFailure::Disabled);
    }

    if engine == "voicevox" {
        let probe = ts::probe_voicevox(http, &settings).await;
        if probe["reachable"].as_bool() != Some(true) {
            let started = ts::start_voicevox(http, file).await;
            if started["ok"].as_bool() != Some(true) {
                let reason = started["error"]
                    .as_str()
                    .unwrap_or("tidak diketahui")
                    .to_string();
                return Err(TtsFailure::Error(format!(
                    "VOICEVOX tidak bisa dihidupkan: {reason}"
                )));
            }
        }
        let result = voicevox_tts(http, text, mood, &settings).await;
        if settings["voicevoxKeepAlive"].as_bool() == Some(false) {
            let http_clone = http.clone();
            let file_clone = file.to_path_buf();
            tokio::spawn(async move {
                // Hormati keepAlive=false: matikan lagi setelah selesai.
                let _ = ts::stop_voicevox(&file_clone).await;
                let _ = http_clone;
            });
        }
        return result.map_err(TtsFailure::Error);
    }

    let mut order: Vec<String> = vec![engine.clone()];
    if engine != "edge" {
        order.push("edge".to_string());
    }
    let mut last_error: Option<String> = None;
    for candidate in &order {
        match run_engine(http, candidate, text, mood, &settings).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::warn!("[TTS] engine \"{candidate}\" gagal: {e}");
                last_error = Some(e);
            }
        }
    }
    Err(TtsFailure::Error(
        last_error.unwrap_or_else(|| "Semua engine TTS gagal.".to_string()),
    ))
}
