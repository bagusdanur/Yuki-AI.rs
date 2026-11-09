//! Route agenda & pengingat terjadwal — port dari server.js:
//! GET /api/agenda, PATCH /api/agenda/{taskId}, GET /api/chat/reminders.

use crate::auth::{check_rate_limit, SessionUser};
use crate::scheduler;
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

pub async fn chat_reminders(
    State(state): State<AppState>,
    user: SessionUser,
    uri: Uri,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/chat/reminders", 60_000, 30) {
        return res;
    }
    let after: i64 = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find(|(k, _)| *k == "after")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0)
        .max(0);
    (
        StatusCode::OK,
        Json(json!({ "reminders": state.db.get_scheduled_reminder_messages(&user.0, after) })),
    )
        .into_response()
}

pub async fn get_agenda_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agenda", 60_000, 60) {
        return res;
    }
    (StatusCode::OK, Json(scheduler::get_agenda(&state.db, &user.0))).into_response()
}

pub async fn patch_agenda(
    State(state): State<AppState>,
    user: SessionUser,
    Path(task_id_raw): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agenda/:taskId", 60_000, 30) {
        return res;
    }
    let task_id: i64 = match task_id_raw.parse() {
        Ok(v) if v >= 1 => v,
        _ => return err(StatusCode::BAD_REQUEST, "ID pengingat tidak valid."),
    };
    let action = body["action"].as_str().unwrap_or("");
    let result = match action {
        "pause" => scheduler::set_task_status(&state.db, task_id, &user.0, "paused"),
        "resume" => scheduler::set_task_status(&state.db, task_id, &user.0, "active"),
        "cancel" => scheduler::set_task_status(&state.db, task_id, &user.0, "cancelled"),
        "snooze" => scheduler::snooze_task(
            &state.db,
            task_id,
            &user.0,
            body["minutes"].as_i64().unwrap_or(10),
        ),
        "edit" => scheduler::update_task(
            &state.db,
            task_id,
            &user.0,
            body["title"].as_str(),
            body["description"].as_str(),
            body["schedule"].as_str(),
            body["timezone"].as_str(),
        ),
        _ => return err(StatusCode::BAD_REQUEST, "Aksi agenda tidak dikenali."),
    };
    if result["success"].as_bool() != Some(true) {
        let message = result["error"]
            .as_str()
            .unwrap_or("Pengingat tidak ditemukan.");
        return err(StatusCode::NOT_FOUND, message);
    }
    (StatusCode::OK, Json(result)).into_response()
}
