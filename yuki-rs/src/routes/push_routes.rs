//! Route Web Push — port dari server.js:
//! GET /api/push/config, POST/DELETE /api/push/subscriptions.

use crate::auth::{check_rate_limit, SessionUser};
use crate::push::{remove_push_subscription, save_push_subscription, SubError};
use crate::state::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub async fn push_config(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/push/config", 60_000, 10) {
        return res;
    }
    (StatusCode::OK, Json(state.push.public_view())).into_response()
}

pub async fn post_subscription(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/push/subscriptions", 60_000, 5) {
        return res;
    }
    match save_push_subscription(&state.db, &user.0, &body["subscription"]) {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(SubError::OwnedByOther) => {
            err(StatusCode::CONFLICT, "Perangkat ini sudah terdaftar ke akun lain.")
        }
        Err(SubError::HostNotAllowed) => {
            err(StatusCode::BAD_REQUEST, "Endpoint push tidak dikenal.")
        }
        Err(SubError::Invalid) => {
            err(StatusCode::BAD_REQUEST, "Subscription notifikasi tidak valid.")
        }
    }
}

pub async fn delete_subscription(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/push/subscriptions", 60_000, 10) {
        return res;
    }
    let endpoint = body["endpoint"].as_str().unwrap_or("");
    (
        StatusCode::OK,
        Json(remove_push_subscription(&state.db, &user.0, endpoint)),
    )
        .into_response()
}
