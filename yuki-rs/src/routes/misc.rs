//! Route pengguna Fase 5 (server.js): relationship, feedback, hapus
//! akun, dan analyze-url (bukti situs via lib/web-evidence.js).

use crate::auth::{check_rate_limit, session_cookie_value, SessionUser, SESSION_COOKIE_NAME};
use crate::emotion::Emotion;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// GET /api/relationship/{userId} — hanya untuk diri sendiri.
pub async fn relationship_route(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    Path(target): Path<String>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/relationship", 60_000, 30) {
        return r;
    }
    if target != user_id {
        return err(StatusCode::FORBIDDEN, "Akses ditolak.");
    }
    let emotion = Emotion::from_state(state.db.load_emotion_state(&user_id));
    let bond = emotion.bond;
    Json(json!({
        "bond": crate::llm::bond_level_name(bond),
        "bondValue": bond,
        "milestones": state.db.sync_bond_milestones(&user_id, bond),
    }))
    .into_response()
}

/// POST /api/feedback {messageId, rating(-1|1), reason}.
pub async fn feedback_route(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/feedback", 60_000, 40) {
        return r;
    }
    let rating = body["rating"].as_f64();
    let valid = matches!(rating, Some(r) if r == 1.0 || r == -1.0);
    if !valid {
        return err(StatusCode::BAD_REQUEST, "Feedback tidak valid.");
    }
    let message_id = body["messageId"].as_i64();
    let reason = body["reason"].as_str().unwrap_or("");
    state
        .db
        .save_response_feedback(&user_id, message_id, rating.unwrap() as i64, reason);
    Json(json!({ "ok": true })).into_response()
}

/// DELETE /api/account — hapus SEMUA data user (urutan Node):
/// jadwal, langganan push, data agent, workflow, run, data inti,
/// workspace, lalu sesi & cookie dibersihkan.
pub async fn account_delete_route(
    State(state): State<AppState>,
    headers: HeaderMap,
    SessionUser(user_id): SessionUser,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/account", 3_600_000, 3) {
        return r;
    }
    let _ = state.db.delete_user_tasks(&user_id);
    let _ = state.db.delete_user_subscriptions(&user_id);
    // deleteAgentMemoryData tercakup delete_user_data (kedua tabel
    // berkolom user_id); sisanya eksplisit seperti Node.
    state.db.delete_user_dynamic_skills(&user_id);
    state.db.delete_user_workflows(&user_id);
    state.db.delete_user_agent_runs(&user_id);
    let _ = state.db.delete_user_data(&user_id);
    crate::agent::tools_workspace::delete_workspace_data(&user_id);
    state.turns.lock().unwrap().remove(&user_id);
    let host = crate::auth::host_of(&headers);
    let clear = format!(
        "{}; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        session_cookie_value(&host, "", 0, SESSION_COOKIE_NAME, true)
    );
    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, clear)],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/// POST /api/analyze-url {url} — bukti techstack situs eksternal.
pub async fn analyze_url_route(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/analyze-url", 60_000, 5) {
        return r;
    }
    let url = body["url"].as_str().unwrap_or("");
    let evidence = crate::evidence::fetch_site_evidence(&state.http, url).await;
    Json(evidence).into_response()
}
