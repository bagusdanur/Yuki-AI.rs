//! Route otentikasi: /api/register, /api/login-code,
//! /api/session/refresh, /api/session/reset — port dari server.js.

use crate::auth::{
    check_rate_limit, host_of, session_cookie_value, sign_session, SessionUser,
    ADMIN_COOKIE_MAX_AGE, ADMIN_COOKIE_NAME, SESSION_COOKIE_MAX_AGE, SESSION_COOKIE_NAME,
};
use crate::state::AppState;
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

/// IP klien untuk bucket rate-limit: X-Forwarded-For pertama (di belakang
/// proxy seperti Node), fallback "unknown" (socket addr tidak diekstrak
/// di Fase 1; bucket tetap konsisten per nilai header).
pub fn client_ip(headers: &HeaderMap) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            let first = first.trim();
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    "unknown".to_string()
}

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn set_session_cookie(headers: &HeaderMap, token: &str) -> (header::HeaderName, String) {
    (
        header::SET_COOKIE,
        session_cookie_value(&host_of(headers), token, SESSION_COOKIE_MAX_AGE, SESSION_COOKIE_NAME, true),
    )
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let ip = client_ip(&headers);
    if let Err(res) = check_rate_limit(&state, &ip, "/api/register", 60_000, 10) {
        return res;
    }
    let username = body["username"].as_str().unwrap_or("").trim().to_string();
    if username.is_empty() || username.chars().count() > 50 {
        return err(StatusCode::BAD_REQUEST, "Nama harus berisi 1-50 karakter.");
    }
    let (allowed, _, _) = state
        .db
        .consume_rate_limit(&format!("register-ip:{ip}"), 60 * 60 * 1000, 3);
    if !allowed {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "Terlalu banyak pendaftaran dari jaringan ini. Coba lagi nanti.",
        );
    }
    if state.db.username_taken_recently(&username) {
        return err(StatusCode::CONFLICT, "Nama tidak tersedia, coba nama lain.");
    }
    let (user_id, access_code) = match state.db.register_user(&username) {
        Ok(v) => v,
        Err(_) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Terjadi kesalahan internal pada server.",
            )
        }
    };
    // Fakta benih seperti registerUser di Node (lewat addFacts -> embedding).
    crate::memory::add_facts(
        &state.db,
        &state.embedder,
        &user_id,
        &[format!("Nama user adalah {username}")],
    )
    .await;
    let welcome = format!("*menatapmu sebentar, masih agak menjaga jarak*\n\nJadi namamu {username}? Aku Yuki. Salam kenal. Untuk sekarang kita kenalan dulu saja—jangan langsung merasa sudah dekat.\n\nKalau nanti kita cocok, mungkin aku bisa jadi teman dekatmu... atau sesuatu yang lebih. Itu tergantung bagaimana kamu memperlakukanku.\n\nKamu datang karena butuh teman ngobrol, atau cuma penasaran?");
    state.db.save_chat_message(&user_id, "assistant", &welcome);
    let milestones = state.db.sync_bond_milestones(&user_id, 0.0);
    let session_token = sign_session(&state, &user_id);
    (
        StatusCode::OK,
        [set_session_cookie(&headers, &session_token)],
        Json(json!({
            "userId": user_id,
            "accessCode": access_code,
            "sessionToken": session_token,
            "welcome": welcome,
            "milestones": milestones,
        })),
    )
        .into_response()
}

pub async fn login_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let ip = client_ip(&headers);
    if let Err(res) = check_rate_limit(&state, &ip, "/api/login-code", 60_000, 15) {
        return res;
    }
    let access_code = body["accessCode"].as_str().unwrap_or("").trim().to_string();
    if access_code.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Kode akses tidak boleh kosong.");
    }
    let Some((user_id, username)) = state.db.get_user_by_access_code(&access_code) else {
        // Node server.js: login gagal ikut dihitung strike auto-block.
        crate::ipblock::record_strike(&state, &ip, "login-gagal");
        return err(StatusCode::NOT_FOUND, "Kode akses tidak ditemukan atau salah.");
    };
    let history = state.db.get_chat_history(&user_id);
    let bond = state.db.load_bond(&user_id);
    let milestones = state.db.sync_bond_milestones(&user_id, bond);
    let session_token = sign_session(&state, &user_id);
    (
        StatusCode::OK,
        [set_session_cookie(&headers, &session_token)],
        Json(json!({
            "userId": user_id,
            "username": username,
            "sessionToken": session_token,
            "history": history,
            "bondValue": bond,
            "milestones": milestones,
        })),
    )
        .into_response()
}

pub async fn session_refresh(
    State(state): State<AppState>,
    user: SessionUser,
    headers: HeaderMap,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/session/refresh", 60_000, 10) {
        return res;
    }
    let session_token = sign_session(&state, &user.0);
    (
        StatusCode::OK,
        [set_session_cookie(&headers, &session_token)],
        Json(json!({ "sessionToken": session_token })),
    )
        .into_response()
}

pub async fn session_reset(
    State(state): State<AppState>,
    user: SessionUser,
    headers: HeaderMap,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/session/reset", 60_000, 5) {
        return res;
    }
    state.db.bump_session_version(&user.0);
    let clear = format!(
        "{}; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        session_cookie_value(&host_of(&headers), "", 0, SESSION_COOKIE_NAME, true)
    );
    (
        StatusCode::OK,
        [(header::SET_COOKIE, clear)],
        Json(json!({ "success": true })),
    )
        .into_response()
}

#[allow(dead_code)]
fn admin_cookie(headers: &HeaderMap, token: &str) -> (header::HeaderName, String) {
    (
        header::SET_COOKIE,
        session_cookie_value(&host_of(headers), token, ADMIN_COOKIE_MAX_AGE, ADMIN_COOKIE_NAME, false),
    )
}
