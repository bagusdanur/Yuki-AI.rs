//! Route TTS Fase 6 (server.js): POST /api/tts + blok admin TTS
//! (settings, kontrol daemon VOICEVOX, uji engine).

use crate::auth::{check_rate_limit, AdminUser, SessionUser};
use crate::routes::auth_routes::client_ip;
use crate::state::AppState;
use crate::tts::{self, TtsFailure};
use crate::tts_settings as ts;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn rate(
    state: &AppState,
    key: &str,
    uri: &Uri,
    window_ms: i64,
    max: i64,
) -> Result<(), Response> {
    check_rate_limit(state, key, uri.path(), window_ms, max).map(|_| ())
}

fn admin_rate(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    max: i64,
) -> Result<(), Response> {
    rate(state, &client_ip(headers), uri, 60_000, max)
}

fn audio_response(buffer: Vec<u8>, mime: &str) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, mime.to_string())], buffer).into_response()
}

/// POST /api/tts {text, mood} -> audio mentah; VOICEVOX nonaktif = 204.
pub async fn tts_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user.0, "/api/tts", 60_000, 15) {
        return r;
    }
    let text = body["text"].as_str().unwrap_or("");
    // Node: typeof string, trim non-kosong, length (UTF-16) <= 2000.
    let invalid = body["text"].as_str().is_none()
        || text.trim().is_empty()
        || text.encode_utf16().count() > 2000;
    if invalid {
        return err(
            StatusCode::BAD_REQUEST,
            "Teks suara harus berisi 1-2000 karakter.",
        );
    }
    let mood = body["mood"].as_str().unwrap_or("");
    match tts::synthesize(&state.http, &state.config.tts_config_file, text.trim(), mood, None).await {
        Ok((buffer, mime)) => audio_response(buffer, &mime),
        Err(TtsFailure::Disabled) => StatusCode::NO_CONTENT.into_response(),
        Err(TtsFailure::Error(e)) => {
            tracing::error!("[tts] {e}");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Terjadi kesalahan internal pada server.",
            )
        }
    }
}

// === Admin TTS ===

pub async fn admin_tts_get(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 60) {
        return r;
    }
    let file = &state.config.tts_config_file;
    let settings = ts::get_tts_settings(file);
    let (voicevox, process) = tokio::join!(
        ts::probe_voicevox(&state.http, &settings),
        ts::get_voicevox_process_status(file)
    );
    Json(json!({
        "settings": settings,
        "engines": ts::TTS_ENGINES,
        "voicevox": voicevox,
        "process": process,
    }))
    .into_response()
}

pub async fn admin_tts_put(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 20) {
        return r;
    }
    let settings = ts::save_tts_settings(&state.config.tts_config_file, &body);
    Json(json!({ "ok": true, "settings": settings })).into_response()
}

pub async fn admin_voicevox_start(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 10) {
        return r;
    }
    let file = &state.config.tts_config_file;
    ts::save_tts_settings(file, &json!({ "voicevoxEnabled": true }));
    ts::set_voicevox_autorestart(file, true).await;
    let result = ts::start_voicevox(&state.http, file).await;
    let code = if result["ok"].as_bool() == Some(true) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(result)).into_response()
}

pub async fn admin_voicevox_stop(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 10) {
        return r;
    }
    let file = &state.config.tts_config_file;
    ts::save_tts_settings(file, &json!({ "voicevoxEnabled": false }));
    ts::set_voicevox_autorestart(file, false).await;
    let result = ts::stop_voicevox(file).await;
    let code = if result["ok"].as_bool() == Some(true) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(result)).into_response()
}

pub async fn admin_voicevox_status(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 120) {
        return r;
    }
    let file = &state.config.tts_config_file;
    let settings = ts::get_tts_settings(file);
    let (voicevox, process) = tokio::join!(
        ts::probe_voicevox(&state.http, &settings),
        ts::get_voicevox_process_status(file)
    );
    Json(json!({
        "voicevox": voicevox,
        "process": process,
        "engine": settings["engine"],
        "keepAlive": settings["voicevoxKeepAlive"],
        "enabled": settings["voicevoxEnabled"].as_bool() != Some(false),
    }))
    .into_response()
}

pub async fn admin_voicevox_speakers(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 30) {
        return r;
    }
    let file = &state.config.tts_config_file;
    let settings = ts::get_tts_settings(file);
    let probe = ts::probe_voicevox(&state.http, &settings).await;
    if probe["reachable"].as_bool() != Some(true) {
        let started = ts::start_voicevox(&state.http, file).await;
        if started["ok"].as_bool() != Some(true) {
            let message = started["error"]
                .as_str()
                .unwrap_or("VOICEVOX tidak aktif")
                .to_string();
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ok": false, "error": message })))
                .into_response();
        }
        let result = ts::list_voicevox_speakers(&state.http, file).await;
        if settings["voicevoxKeepAlive"].as_bool() == Some(false) {
            let file_clone = file.clone();
            tokio::spawn(async move {
                let _ = ts::stop_voicevox(&file_clone).await;
            });
        }
        return Json(result).into_response();
    }
    Json(ts::list_voicevox_speakers(&state.http, file).await).into_response()
}

/// POST /api/admin/tts/test — Node MENYIMPAN settings gabungan lebih
/// dulu (saveTtsSettings), baru sintesis dengan settings itu.
pub async fn admin_tts_test(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = admin_rate(&state, &headers, &uri, 10) {
        return r;
    }
    let file = &state.config.tts_config_file;
    let mut merged = ts::get_tts_settings(file);
    if let (Some(dst), Some(src)) = (merged.as_object_mut(), body["settings"].as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let settings = ts::save_tts_settings(file, &merged);
    let text_raw = body["text"].as_str().unwrap_or("Halo, ini contoh suara Yuki.");
    let text: String = text_raw.chars().take(300).collect();
    let mood = body["mood"].as_str().unwrap_or("tenang");
    match tts::synthesize(&state.http, file, &text, mood, Some(settings)).await {
        Ok((buffer, mime)) => audio_response(buffer, &mime),
        Err(TtsFailure::Disabled) => {
            err(StatusCode::BAD_REQUEST, tts::DISABLED_MESSAGE)
        }
        Err(TtsFailure::Error(e)) => {
            let msg: String = e.chars().take(240).collect();
            err(StatusCode::BAD_REQUEST, &msg)
        }
    }
}
