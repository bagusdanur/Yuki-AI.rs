//! Route halaman khusus dari `server.js` yang harus menang atas static:
//! - `GET /embed.html` -> redirect 302 ke `/?...&embed=1`
//! - `GET /changelog`, `/docs`, `/admin`, `/privacy` -> file HTML di `public/`
//! - `GET /favicon.ico` -> `public/yuki-icon.svg` sebagai image/svg+xml

use crate::error::AppResult;
use crate::state::AppState;
use axum::{
    extract::{RawQuery, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

async fn serve_public_html(state: &AppState, file: &str) -> AppResult<Response> {
    let path = state.config.public_dir.join(file);
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| anyhow::anyhow!("gagal membaca {}: {e}", path.display()))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response())
}

pub async fn embed(RawQuery(query): RawQuery) -> Response {
    // Express res.redirect() memakai 302; axum Redirect::to() memakai 303,
    // jadi status dibangun manual agar kontraknya sama.
    // Meniru URLSearchParams.set('embed', '1') di Node:
    // ganti di tempat bila sudah ada, tambahkan di akhir bila belum.
    let raw = query.unwrap_or_default();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut replaced = false;
    if !raw.is_empty() {
        for part in raw.split('&') {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            if k == "embed" && !replaced {
                pairs.push(("embed".to_string(), "1".to_string()));
                replaced = true;
            } else if k != "embed" {
                pairs.push((k.to_string(), v.to_string()));
            }
        }
    }
    if !replaced {
        pairs.push(("embed".to_string(), "1".to_string()));
    }
    let qs = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    (
        StatusCode::FOUND,
        [(header::LOCATION, format!("/?{qs}"))],
    )
        .into_response()
}

pub async fn changelog(State(state): State<AppState>) -> AppResult<Response> {
    serve_public_html(&state, "changelog.html").await
}

pub async fn docs(State(state): State<AppState>) -> AppResult<Response> {
    serve_public_html(&state, "docs.html").await
}

pub async fn admin(State(state): State<AppState>) -> AppResult<Response> {
    serve_public_html(&state, "admin.html").await
}

pub async fn privacy(State(state): State<AppState>) -> AppResult<Response> {
    serve_public_html(&state, "privacy.html").await
}

pub async fn favicon(State(state): State<AppState>) -> AppResult<Response> {
    let path = state.config.public_dir.join("yuki-icon.svg");
    let body = tokio::fs::read(&path)
        .await
        .map_err(|e| anyhow::anyhow!("gagal membaca {}: {e}", path.display()))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/svg+xml")],
        body,
    )
        .into_response())
}
