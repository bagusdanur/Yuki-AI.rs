//! `GET /api/health` — kontrak persis dari `server.js` baris 1566:
//! `{ "status": "ok", "uptime": <detik sejak start, dibulatkan> }`

use crate::state::AppState;
use axum::{extract::State, Json};
use serde_json::{json, Value};

pub async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "uptime": state.uptime_seconds(),
    }))
}
