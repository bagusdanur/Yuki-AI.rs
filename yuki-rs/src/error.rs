//! Tipe error seragam. Bentuk JSON meniru handler 500 di `server.js`:
//! `{ "error": "Terjadi kesalahan internal pada server." }`

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Internal(err) => {
                tracing::error!("[api 500] {err:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "Terjadi kesalahan internal pada server." })),
                )
                    .into_response()
            }
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
