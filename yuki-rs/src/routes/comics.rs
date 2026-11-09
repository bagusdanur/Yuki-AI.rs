//! Route komik Fase 5 (server.js): bookmark komik user + proxy cover
//! /api/comic-cover. Pencarian memakai port searchComics/latestComics
//! Fase 4 (lib/ryukomik.js).

use crate::agent::tools::ToolContext;
use crate::agent::tools_web::{latest_comics, search_comics};
use crate::auth::{check_rate_limit, SessionUser};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

pub async fn bookmarks_get(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/comics/bookmarks", 60_000, 40) {
        return r;
    }
    Json(json!({ "bookmarks": state.db.get_comic_bookmarks(&user_id) })).into_response()
}

pub async fn bookmarks_post(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/comics/bookmarks", 60_000, 40) {
        return r;
    }
    let comic = if body["comic"].is_object() {
        body["comic"].clone()
    } else {
        body.clone()
    };
    let title = comic["title"].as_str().unwrap_or("").trim().to_string();
    let url = comic["url"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() || url.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Data komik tidak valid.");
    }
    let bookmarks = state.db.save_comic_bookmark(&user_id, &comic);
    Json(json!({ "ok": true, "bookmarks": bookmarks })).into_response()
}

pub async fn bookmarks_delete(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    Query(params): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Response {
    if let Err(r) = check_rate_limit(&state, &user_id, "/api/comics/bookmarks", 60_000, 40) {
        return r;
    }
    let url = params
        .get("url")
        .cloned()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            body.as_ref()
                .and_then(|Json(b)| b["url"].as_str().map(|s| s.to_string()))
        })
        .unwrap_or_default();
    if url.is_empty() {
        return err(StatusCode::BAD_REQUEST, "URL komik tidak valid.");
    }
    let bookmarks = state.db.delete_comic_bookmark(&user_id, &url);
    Json(json!({ "ok": true, "bookmarks": bookmarks })).into_response()
}

/// GET /api/comic-cover?url=... — TANPA sesi, persis Node: hanya URL
/// /komik/(komiku|doujindesu)/<slug> yang diproses; gagal -> {image:''}.
pub async fn comic_cover_route(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let empty = || Json(json!({ "image": "" })).into_response();
    let url = params.get("url").cloned().unwrap_or_default();
    let re = regex::Regex::new(r"/komik/(komiku|doujindesu)/([^/?#]+)").unwrap();
    let Some(caps) = re.captures(&url) else {
        return empty();
    };
    let sumber = caps[1].to_string();
    let slug = caps[2].to_string();
    let ctx = ToolContext::new(
        state.db.clone(),
        state.http.clone(),
        state.llm.clone(),
        "cover",
    );
    let adult = sumber == "doujindesu";
    let hasil = search_comics(&ctx, &slug, adult).await;
    let found = hasil
        .iter()
        .find(|c| c["url"].as_str().unwrap_or("").contains(&slug));
    if let Some(c) = found {
        return Json(json!({ "image": c["image"].as_str().unwrap_or("") })).into_response();
    }
    let terbaru = latest_comics(&ctx, "").await;
    let found = terbaru
        .iter()
        .find(|c| c["url"].as_str().unwrap_or("").contains(&slug));
    match found {
        Some(c) => Json(json!({ "image": c["image"].as_str().unwrap_or("") })).into_response(),
        None => empty(),
    }
}
