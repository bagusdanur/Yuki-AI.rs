//! Route /api/memories* — port dari server.js (menu "Ingatan Yuki"):
//! daftar gabungan (agent + facts + structured), settings auto-capture,
//! patch/hapus agent memory, hapus per-sumber fact & structured.

use crate::auth::{check_rate_limit, SessionUser};
use crate::memory;
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn query_param(uri: &Uri, key: &str) -> String {
    uri.query()
        .unwrap_or("")
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
        .unwrap_or_default()
}

pub async fn list_memories(
    State(state): State<AppState>,
    user: SessionUser,
    uri: Uri,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories", 60_000, 60) {
        return res;
    }
    let (facts, structured) = memory::list_all_memories(&state.db, &user.0);
    let memories = memory::list_agent_memories(
        &state.db,
        &user.0,
        &query_param(&uri, "scopeType"),
        &query_param(&uri, "scopeId"),
        &query_param(&uri, "status"),
    );
    (
        StatusCode::OK,
        Json(json!({
            "memories": memories,
            "settings": memory::get_memory_settings(&state.db, &user.0),
            "facts": facts,
            "structured": structured,
        })),
    )
        .into_response()
}

pub async fn put_settings(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories/settings", 60_000, 20) {
        return res;
    }
    let auto = body["autoCapture"].as_bool() == Some(true);
    (
        StatusCode::OK,
        Json(json!({
            "settings": memory::set_memory_settings(&state.db, &user.0, auto),
        })),
    )
        .into_response()
}

pub async fn patch_memory(
    State(state): State<AppState>,
    user: SessionUser,
    Path(memory_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories/:memoryId", 60_000, 30) {
        return res;
    }
    let id: i64 = memory_id.parse().unwrap_or(-1);
    match memory::update_agent_memory(&state.db, id, &user.0, &body) {
        Some(updated) => (
            StatusCode::OK,
            Json(json!({ "success": true, "memory": updated })),
        )
            .into_response(),
        None => err(StatusCode::NOT_FOUND, "Memori tidak ditemukan atau bukan milikmu."),
    }
}

pub async fn delete_memory(
    State(state): State<AppState>,
    user: SessionUser,
    Path(memory_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories/:memoryId", 60_000, 20) {
        return res;
    }
    let id: i64 = memory_id.parse().unwrap_or(-1);
    if !state.db.delete_agent_memory(&user.0, id) {
        return err(StatusCode::NOT_FOUND, "Memori tidak ditemukan atau bukan milikmu.");
    }
    (StatusCode::OK, Json(json!({ "success": true }))).into_response()
}

pub async fn delete_fact(
    State(state): State<AppState>,
    user: SessionUser,
    Path(fact_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories/fact/:factId", 60_000, 20) {
        return res;
    }
    let id: i64 = fact_id.parse().unwrap_or(-1);
    if !state.db.delete_fact(&user.0, id) {
        return err(StatusCode::NOT_FOUND, "Fakta tidak ditemukan atau bukan milikmu.");
    }
    (StatusCode::OK, Json(json!({ "success": true }))).into_response()
}

pub async fn delete_structured(
    State(state): State<AppState>,
    user: SessionUser,
    Path(memory_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/memories/structured/:memoryId", 60_000, 20) {
        return res;
    }
    let id: i64 = memory_id.parse().unwrap_or(-1);
    if !state.db.delete_structured(&user.0, id) {
        return err(StatusCode::NOT_FOUND, "Memori tidak ditemukan atau bukan milikmu.");
    }
    (StatusCode::OK, Json(json!({ "success": true }))).into_response()
}
