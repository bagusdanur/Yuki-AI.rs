//! yuki-rs — port Rust bertahap dari Yuki Agent (bagusdanur/Yuki---AI).
//!
//! Fase 0: fondasi. Config env, /api/health dengan kontrak yang sama,
//! halaman khusus (embed/changelog/docs/admin/privacy/favicon), dan
//! penyajian file statis dengan urutan yang sama seperti Node:
//! `dist` (hasil vite build) lebih dulu, lalu `public`.

mod admin_data;
mod agent;
mod auth;
mod config;
mod db;
mod edge_tts;
mod emotion;
mod error;
mod evidence;
mod games;
mod ipblock;
mod llm;
mod memory;
mod provider;
mod push;
mod reminder;
mod routes;
mod scheduler;
mod semantic;
mod state;
mod timeutil;
mod tts;
mod tts_settings;

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use config::Config;
use state::AppState;
use std::path::{Component, Path, PathBuf};
use tower_http::trace::TraceLayer;

/// Ekstensi yang di Node tidak pernah tersaji sebagai statis (audit Nov 2026).
const BLOCKED_EXT: &[&str] = &[
    "bak", "db", "sqlite", "sqlite3", "env", "log", "pem", "key", "p12", "pfx", "sql", "dump",
    "tar", "tgz", "zip",
];

async fn blocked_static(req: Request, next: Next) -> Response {
    if req.method() == Method::GET {
        if let Some(ext) = Path::new(req.uri().path())
            .extension()
            .and_then(|e| e.to_str())
        {
            if BLOCKED_EXT.iter().any(|b| b.eq_ignore_ascii_case(ext)) {
                return (StatusCode::NOT_FOUND, "").into_response();
            }
        }
    }
    next.run(req).await
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// Fallback statis: coba `dist`, lalu `public`. Meniru express.static
/// berurutan di Node, termasuk `dotfiles: 'ignore'` dan tanpa fallback SPA
/// (Node memang tidak punya fallback SPA; path tak dikenal = 404).
/// Middleware global Fase 5 (server.js): blokir IP lebih dulu, lalu
/// catat metrics runtime; respons 429 memicu recordStrike.
async fn observability(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    use std::sync::atomic::Ordering::Relaxed;
    let ip = routes::auth_routes::client_ip(req.headers());
    if ipblock::is_ip_blocked(&state.db, &ip) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "Akses dari jaringan ini diblokir sementara."
            })),
        )
            .into_response();
    }
    let started = std::time::Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    state.metrics.requests.fetch_add(1, Relaxed);
    state
        .metrics
        .latency_total_ms
        .fetch_add(started.elapsed().as_millis() as i64, Relaxed);
    if status.as_u16() >= 500 {
        state.metrics.errors.fetch_add(1, Relaxed);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        ipblock::record_strike(&state, &ip, "rate-limit");
    }
    response
}

async fn static_fallback(State(state): State<AppState>, uri: Uri) -> Response {
    let raw = uri.path();
    // Node: rute /api/* tak dikenal -> 404 JSON dengan path-nya.
    if raw.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": format!("Rute API \"{raw}\" tidak ditemukan.")
            })),
        )
            .into_response();
    }
    let rel = raw.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    // Tolak traversal dan dotfile, seperti static Node.
    let rel_path = Path::new(rel);
    for comp in rel_path.components() {
        match comp {
            Component::Normal(os) => {
                if os.to_string_lossy().starts_with('.') {
                    return (StatusCode::NOT_FOUND, "").into_response();
                }
            }
            _ => return (StatusCode::NOT_FOUND, "").into_response(),
        }
    }
    for base in [&state.config.dist_dir, &state.config.public_dir] {
        let candidate: PathBuf = base.join(rel_path);
        if candidate.is_file() {
            match tokio::fs::read(&candidate).await {
                Ok(body) => {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, content_type(&candidate))],
                        body,
                    )
                        .into_response()
                }
                Err(_) => continue,
            }
        }
        // Direktori: Node menyajikan index.html di dalamnya.
        let index = candidate.join("index.html");
        if candidate.is_dir() && index.is_file() {
            if let Ok(body) = tokio::fs::read(&index).await {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, content_type(&index))],
                    body,
                )
                    .into_response();
            }
        }
    }
    (StatusCode::NOT_FOUND, "").into_response()
}

fn build_router(state: AppState) -> Router {
    let router = Router::new()
        .route("/api/health", get(routes::health::health))
        .route(
            "/api/register",
            axum::routing::post(routes::auth_routes::register),
        )
        .route(
            "/api/login-code",
            axum::routing::post(routes::auth_routes::login_code),
        )
        .route(
            "/api/session/refresh",
            axum::routing::post(routes::auth_routes::session_refresh),
        )
        .route(
            "/api/session/reset",
            axum::routing::post(routes::auth_routes::session_reset),
        )
        .route("/api/chat/history", get(routes::chat::chat_history))
        .route("/api/chat", axum::routing::post(routes::chat::chat))
        .route("/api/memories", get(routes::memories::list_memories))
        .route(
            "/api/memories/settings",
            axum::routing::put(routes::memories::put_settings),
        )
        .route(
            "/api/memories/fact/{factId}",
            axum::routing::delete(routes::memories::delete_fact),
        )
        .route(
            "/api/memories/structured/{memoryId}",
            axum::routing::delete(routes::memories::delete_structured),
        )
        .route(
            "/api/memories/{memoryId}",
            axum::routing::patch(routes::memories::patch_memory)
                .delete(routes::memories::delete_memory),
        )
        .route("/api/chat/reminders", get(routes::agenda::chat_reminders))
        .route("/api/agenda", get(routes::agenda::get_agenda_route))
        .route(
            "/api/agenda/{taskId}",
            axum::routing::patch(routes::agenda::patch_agenda),
        )
        .route("/api/push/config", get(routes::push_routes::push_config))
        .route(
            "/api/push/subscriptions",
            axum::routing::post(routes::push_routes::post_subscription)
                .delete(routes::push_routes::delete_subscription),
        )
        .route("/api/agent/skills", get(routes::agent::list_skills_route))
        .route(
            "/api/agent/dynamic-skills",
            get(routes::agent::list_dynamic_skills_route)
                .post(routes::agent::create_dynamic_skill_route),
        )
        .route(
            "/api/agent/dynamic-skills/{skillId}/validate",
            axum::routing::post(routes::agent::validate_dynamic_skill_route),
        )
        .route(
            "/api/agent/dynamic-skills/{skillId}/approve",
            axum::routing::post(routes::agent::approve_dynamic_skill_route),
        )
        .route(
            "/api/agent/dynamic-skills/{skillId}/disable",
            axum::routing::post(routes::agent::disable_dynamic_skill_route),
        )
        .route(
            "/api/agent/progress/{requestId}",
            get(routes::agent::progress_route),
        )
        .route("/api/agent/runs/active", get(routes::agent::runs_active_route))
        .route(
            "/api/agent/runs/{requestId}/cancel",
            axum::routing::post(routes::agent::cancel_run_route),
        )
        .route(
            "/api/agent/workspace/import",
            axum::routing::post(routes::agent::workspace_import_route),
        )
        .route(
            "/api/agent/workflows/pending",
            get(routes::agent::workflows_pending_route),
        )
        .route(
            "/api/agent/approvals/{approvalId}",
            axum::routing::post(routes::agent::approvals_route),
        )
        .route("/embed.html", get(routes::pages::embed))
        .route("/changelog", get(routes::pages::changelog))
        .route("/docs", get(routes::pages::docs))
        .route("/admin", get(routes::pages::admin))
        .route("/privacy", get(routes::pages::privacy))
        .route("/favicon.ico", get(routes::pages::favicon))
        // ===== Fase 5: layanan pinggiran =====
        .route("/api/game/status", get(routes::games::status_route))
        .route(
            "/api/comics/bookmarks",
            get(routes::comics::bookmarks_get)
                .post(routes::comics::bookmarks_post)
                .delete(routes::comics::bookmarks_delete),
        )
        .route("/api/comic-cover", get(routes::comics::comic_cover_route))
        .route(
            "/api/relationship/{userId}",
            get(routes::misc::relationship_route),
        )
        .route("/api/feedback", axum::routing::post(routes::misc::feedback_route))
        .route(
            "/api/account",
            axum::routing::delete(routes::misc::account_delete_route),
        )
        .route(
            "/api/analyze-url",
            axum::routing::post(routes::misc::analyze_url_route),
        )
        .route(
            "/api/admin/login",
            axum::routing::post(routes::admin::login_route),
        )
        .route(
            "/api/admin/logout",
            axum::routing::post(routes::admin::logout_route),
        )
        .route(
            "/api/admin/password",
            axum::routing::post(routes::admin::password_route),
        )
        .route("/api/admin/blocked-ips", get(routes::admin::blocked_ips_route))
        .route(
            "/api/admin/block-ip",
            axum::routing::post(routes::admin::block_ip_route),
        )
        .route(
            "/api/admin/blocked-ips/{ip}",
            axum::routing::delete(routes::admin::unblock_ip_route),
        )
        .route("/api/admin/stats", get(routes::admin::stats_route))
        .route("/api/admin/overview", get(routes::admin::overview_route))
        .route(
            "/api/admin/providers",
            get(routes::admin::providers_get_route).put(routes::admin::providers_put_route),
        )
        .route(
            "/api/admin/providers/test",
            axum::routing::post(routes::admin::providers_test_route),
        )
        .route(
            "/api/admin/providers/reset-stats",
            axum::routing::post(routes::admin::providers_reset_stats_route),
        )
        .route("/api/admin/db/tables", get(routes::admin::db_tables_route))
        .route("/api/admin/db/table/{name}", get(routes::admin::db_table_route))
        .route("/api/admin/users", get(routes::admin::users_route))
        .route("/api/admin/lookup-key", get(routes::admin::lookup_key_route))
        .route(
            "/api/admin/users/{userId}/reset-key",
            axum::routing::post(routes::admin::reset_key_route),
        )
        .route(
            "/api/admin/users/{userId}/tables",
            get(routes::admin::user_tables_route),
        )
        .route(
            "/api/admin/users/{userId}/table/{name}",
            get(routes::admin::user_table_route),
        )
        .route("/api/admin/workspace", get(routes::admin::workspace_route))
        .route(
            "/api/admin/workspace/list",
            get(routes::admin::workspace_list_route),
        )
        .route(
            "/api/admin/workspace/file",
            get(routes::admin::workspace_file_route),
        )
        // ===== Fase 6: TTS =====
        .route("/api/tts", axum::routing::post(routes::tts::tts_route))
        .route(
            "/api/admin/tts",
            get(routes::tts::admin_tts_get).put(routes::tts::admin_tts_put),
        )
        .route(
            "/api/admin/tts/voicevox/start",
            axum::routing::post(routes::tts::admin_voicevox_start),
        )
        .route(
            "/api/admin/tts/voicevox/stop",
            axum::routing::post(routes::tts::admin_voicevox_stop),
        )
        .route(
            "/api/admin/tts/voicevox/status",
            get(routes::tts::admin_voicevox_status),
        )
        .route(
            "/api/admin/tts/voicevox/speakers",
            get(routes::tts::admin_voicevox_speakers),
        )
        .route(
            "/api/admin/tts/test",
            axum::routing::post(routes::tts::admin_tts_test),
        );
    // Route game selain /status hanya ada bila GAMES_ENABLED=true (Node).
    let router = if routes::games::games_enabled() {
        router
            .route(
                "/api/game/ttt/line",
                axum::routing::post(routes::games::ttt_line_route),
            )
            .route("/api/game/ttt", get(routes::games::ttt_get_route))
            .route(
                "/api/game/ttt/new",
                axum::routing::post(routes::games::ttt_new_route),
            )
            .route(
                "/api/game/ttt/move",
                axum::routing::post(routes::games::ttt_move_route),
            )
            .route(
                "/api/game/ttt/reset",
                axum::routing::post(routes::games::ttt_reset_route),
            )
            .route("/api/game/rps", get(routes::games::rps_get_route))
            .route(
                "/api/game/rps/play",
                axum::routing::post(routes::games::rps_play_route),
            )
            .route(
                "/api/game/rps/reset",
                axum::routing::post(routes::games::rps_reset_route),
            )
    } else {
        router
    };
    let state_for_mw = state.clone();
    router
        .fallback(static_fallback)
        .layer(middleware::from_fn(blocked_static))
        .layer(middleware::from_fn_with_state(state_for_mw, observability))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "yuki_rs=info,tower_http=warn".into()),
        )
        .init();

    let config = Config::from_env()?;
    let addr = config.addr();
    if config.is_production && std::env::var("AUTH_SECRET").is_err() {
        eprintln!("[security] AUTH_SECRET belum disetel; sesi akan invalid setelah restart.");
    }
    let state = AppState::new(config)?;
    // Agent (Fase 4): run yang tertinggal di state aktif saat proses
    // sebelumnya mati ditandai gagal (PROCESS_RESTARTED), seperti Node.
    let recovered = state.db.recover_interrupted_agent_runs();
    if recovered > 0 {
        tracing::info!("[agent] {recovered} run terinterupsi ditandai failed");
    }
    // Scheduler engine (Fase 3): restore + ticker 15 detik.
    scheduler::spawn_ticker(state.clone());
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("✨ AI Anime Chat (Rust) jalan di http://{addr}");
    tracing::info!("listening on {addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request as HttpRequest};
    use tower::ServiceExt;

    fn test_state() -> AppState {
        AppState::for_test(Config {
            host: "127.0.0.1".into(),
            port: 0,
            dist_dir: PathBuf::from("/nonexistent-dist"),
            public_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../original/public"),
            memory_db: PathBuf::from(":memory:"),
            provider_config_file: PathBuf::from("/tmp/yuki-rs-test-providers.json"),
            provider_stats_file: PathBuf::from("/tmp/yuki-rs-test-stats.json"),
            admin_password_file: PathBuf::from("/tmp/yuki-rs-test-admin.json"),
            tts_config_file: PathBuf::from("/tmp/yuki-rs-test-tts.json"),
            auth_secret: "secret-uji".into(),
            admin_token: String::new(),
            is_production: false,
        })
    }

    #[tokio::test]
    async fn health_contract() {
        let app = build_router(test_state());
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v["uptime"].as_u64().is_some());
    }

    #[tokio::test]
    async fn embed_redirect_forces_embed_param() {
        let app = build_router(test_state());
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/embed.html?foo=bar")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(
            res.headers().get(header::LOCATION).unwrap(),
            "/?foo=bar&embed=1"
        );
    }

    #[tokio::test]
    async fn blocked_extension_is_404() {
        let app = build_router(test_state());
        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/backup.db")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
