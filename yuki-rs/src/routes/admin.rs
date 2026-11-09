//! Route admin Fase 5 (server.js blok admin): login/logout/password,
//! blokir IP, statistik, provider, overview, penjelajah DB & workspace.
//! Semua di belakang extractor AdminUser (401 seperti requireAdmin
//! Node); rate-limit di-key IP + path penuh seperti middleware Node.
//! Route TTS admin SENGAJA belum ada — itu Fase 6.

use crate::admin_data as ad;
use crate::auth::{
    check_rate_limit, host_of, session_cookie_value, sign_admin_session, store_admin_password,
    verify_admin_password, AdminUser, ADMIN_COOKIE_MAX_AGE, ADMIN_COOKIE_NAME,
};
use crate::ipblock;
use crate::provider::settings;
use crate::routes::auth_routes::client_ip;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn rate(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
    window_ms: i64,
    max: i64,
) -> Result<(), Response> {
    let ip = client_ip(headers);
    check_rate_limit(state, &ip, uri.path(), window_ms, max).map(|_| ())
}

fn admin_cookie(headers: &HeaderMap, state: &AppState) -> String {
    let token = sign_admin_session(state);
    session_cookie_value(&host_of(headers), &token, ADMIN_COOKIE_MAX_AGE, ADMIN_COOKIE_NAME, false)
}

fn clear_admin_cookie(headers: &HeaderMap) -> String {
    format!(
        "{}; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        session_cookie_value(&host_of(headers), "", 0, ADMIN_COOKIE_NAME, false)
    )
}

fn qi(params: &HashMap<String, String>, key: &str, default: i64) -> i64 {
    params
        .get(key)
        .and_then(|v| v.parse::<f64>().ok())
        .map(|n| n as i64)
        .unwrap_or(default)
}

fn qs(params: &HashMap<String, String>, key: &str) -> String {
    params.get(key).cloned().unwrap_or_default()
}

// === Sesi admin ===

pub async fn login_route(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 15 * 60_000, 5) {
        return r;
    }
    let password = body["password"].as_str().unwrap_or("");
    if !verify_admin_password(&state, password) {
        return err(StatusCode::UNAUTHORIZED, "Password admin salah.");
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, admin_cookie(&headers, &state))],
        Json(json!({ "expiresIn": ADMIN_COOKIE_MAX_AGE })),
    )
        .into_response()
}

pub async fn logout_route(_admin: AdminUser, headers: HeaderMap) -> Response {
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear_admin_cookie(&headers))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

pub async fn password_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 15 * 60_000, 5) {
        return r;
    }
    let password = body["password"].as_str().unwrap_or("");
    let len = password.chars().count();
    if !(8..=128).contains(&len) {
        return err(StatusCode::BAD_REQUEST, "Password harus 8-128 karakter.");
    }
    if store_admin_password(&state, password).is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Gagal menyimpan password admin.");
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, admin_cookie(&headers, &state))],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

// === Blokir IP ===

pub async fn blocked_ips_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 30) {
        return r;
    }
    Json(json!({ "blocked": ipblock::list_blocked_ips(&state.db) })).into_response()
}

pub async fn block_ip_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 20) {
        return r;
    }
    let ip = body["ip"].as_str().unwrap_or("").trim().to_string();
    if ip.is_empty() {
        return err(StatusCode::BAD_REQUEST, "IP wajib diisi.");
    }
    let hours = body["hours"].as_f64().unwrap_or(1.0);
    let reason = body["reason"].as_str().unwrap_or("manual-admin");
    match ipblock::block_ip(&state.db, &ip, hours, reason) {
        Some(result) => {
            let mut out = json!({ "ok": true });
            if let Value::Object(m) = result {
                for (k, v) in m {
                    out[k] = v;
                }
            }
            Json(out).into_response()
        }
        None => err(StatusCode::BAD_REQUEST, "IP wajib diisi."),
    }
}

pub async fn unblock_ip_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Path(ip): Path<String>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 20) {
        return r;
    }
    let result = ipblock::unblock_ip(&state.db, &ip);
    let mut out = json!({ "ok": true });
    if let Value::Object(m) = result {
        for (k, v) in m {
            out[k] = v;
        }
    }
    Json(out).into_response()
}

// === Statistik & overview ===

fn runtime(state: &AppState) -> Value {
    state
        .metrics
        .snapshot(state.started_at_ms, state.started_at.elapsed().as_secs())
}

pub async fn stats_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 30) {
        return r;
    }
    let file = state.config.memory_db.to_string_lossy().to_string();
    let mut out = ad::get_admin_stats(&state.db, &file);
    out["runtime"] = runtime(&state);
    out["providers"] = state.provider_router.operations();
    Json(out).into_response()
}

pub async fn overview_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 60) {
        return r;
    }
    let ops = state.provider_router.operations();
    let health: Vec<Value> = ops["providers"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            json!({
                "id": p["id"], "name": p["name"], "state": p["state"], "calls": p["calls"],
                "successRate": p["successRate"], "averageLatencyMs": p["averageLatencyMs"],
                "totalTokens": p["totalTokens"], "inputTokens": p["inputTokens"],
                "outputTokens": p["outputTokens"], "estimatedCostUsd": p["estimatedCostUsd"],
            })
        })
        .collect();
    Json(json!({
        "database": ad::get_database_stats(&state.db),
        "games": crate::games::game_analytics(&state.db),
        "chatTrend": ad::get_chat_trend(&state.db, 30),
        "hourly": ad::get_hourly_activity(&state.db),
        "topUsers": ad::get_top_users(&state.db, 8),
        "recentUsers": ad::get_recent_users(&state.db, 6),
        "emotions": ad::get_emotion_distribution(&state.db),
        "storage": ad::get_storage_info(&state.db),
        "runtime": runtime(&state),
        "providerTotals": ops["totals"],
        "providerDaily": ops["daily"],
        "providerHealth": health,
        "alerts": ops["alerts"],
    }))
    .into_response()
}

// === Provider ===

pub async fn providers_get_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 30) {
        return r;
    }
    let current = settings::load_settings(&state.config.provider_config_file);
    Json(json!({ "providers": settings::public_settings(&current) })).into_response()
}

pub async fn providers_put_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 10) {
        return r;
    }
    match settings::save_settings(&state.config.provider_config_file, &body) {
        Ok(providers) => Json(json!({ "ok": true, "providers": providers })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

pub async fn providers_test_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 10) {
        return r;
    }
    match settings::test_provider_connection(&state.http, &state.config.provider_config_file, &body)
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => {
            let msg: String = e.to_string().chars().take(240).collect();
            err(StatusCode::BAD_REQUEST, &msg)
        }
    }
}

pub async fn providers_reset_stats_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 3) {
        return r;
    }
    state.provider_router.reset();
    Json(json!({ "ok": true, "providers": state.provider_router.operations() })).into_response()
}

// === Penjelajah DB ===

pub async fn db_tables_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 60) {
        return r;
    }
    Json(json!({ "tables": ad::list_tables(&state.db) })).into_response()
}

pub async fn db_table_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 120) {
        return r;
    }
    match ad::read_table(
        &state.db,
        &name,
        qi(&params, "page", 1),
        qi(&params, "pageSize", 25),
        &qs(&params, "search"),
        params.get("orderBy").map(|s| s.as_str()),
        &qs(&params, "order"),
    ) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn users_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 120) {
        return r;
    }
    Json(ad::list_users(
        &state.db,
        &qs(&params, "search"),
        qi(&params, "page", 1),
        qi(&params, "pageSize", 24),
    ))
    .into_response()
}

pub async fn lookup_key_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 60) {
        return r;
    }
    Json(ad::find_memory_keys(&state.db, &qs(&params, "q"), qi(&params, "limit", 25))).into_response()
}

pub async fn reset_key_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 20) {
        return r;
    }
    match state.db.regenerate_access_code(&user_id) {
        Some(result) => Json(result).into_response(),
        None => err(StatusCode::NOT_FOUND, "User tidak ditemukan."),
    }
}

pub async fn user_tables_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 120) {
        return r;
    }
    Json(ad::list_user_tables(&state.db, &user_id)).into_response()
}

pub async fn user_table_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Path((user_id, name)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 180) {
        return r;
    }
    match ad::read_user_table(
        &state.db,
        &user_id,
        &name,
        qi(&params, "page", 1),
        qi(&params, "pageSize", 25),
        &qs(&params, "search"),
    ) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

// === Browser workspace ===

pub async fn workspace_route(
    State(_state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = rate(&_state, &headers, &uri, 60_000, 30) {
        return r;
    }
    Json(ad::workspace_summary(&ad::workspace_root())).into_response()
}

pub async fn workspace_list_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 120) {
        return r;
    }
    match ad::list_directory(&ad::workspace_root(), &qs(&params, "path")) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}

pub async fn workspace_file_route(
    State(state): State<AppState>,
    _admin: AdminUser,
    uri: Uri,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    if let Err(r) = rate(&state, &headers, &uri, 60_000, 60) {
        return r;
    }
    match ad::read_text_file(&ad::workspace_root(), &qs(&params, "path")) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, &e),
    }
}
