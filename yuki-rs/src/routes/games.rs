//! Route game (Fase 5): tic-tac-toe + batu-kertas-gunting — port dari
//! blok game server.js. Route selain /status hanya terdaftar bila
//! GAMES_ENABLED=true, dan hanya untuk user beta (GAMES_PUBLIC /
//! GAMES_BETA_IDS), sama seperti Node.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use regex::Regex;
use serde_json::{json, Value};
use std::time::Duration;

use crate::auth::{check_rate_limit, SessionUser};
use crate::games::*;
use crate::llm::CallOptions;
use crate::state::AppState;

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn ok(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

pub fn games_enabled() -> bool {
    std::env::var("GAMES_ENABLED").map(|v| v == "true").unwrap_or(false)
}

fn game_eligible(user_id: &str) -> bool {
    if std::env::var("GAMES_PUBLIC").map(|v| v == "true").unwrap_or(false) {
        return true;
    }
    let ids = std::env::var("GAMES_BETA_IDS").unwrap_or_default();
    ids.split(',').map(|s| s.trim()).any(|s| !s.is_empty() && s == user_id)
}

/// khususBeta di Node: user tidak eligible -> 404 'Tidak tersedia.'
fn beta_gate(state: &AppState, user: &SessionUser, path: &str) -> Option<Response> {
    if let Err(res) = check_rate_limit(state, &user.0, path, 60_000, 120) {
        return Some(res);
    }
    if !game_eligible(&user.0) {
        return Some(err(StatusCode::NOT_FOUND, "Tidak tersedia."));
    }
    None
}

fn clean_line(raw: &str, max: usize) -> String {
    let no_think = Regex::new(r"(?s)<think[\s\S]*?</think>").unwrap().replace_all(raw, "");
    let first = no_think.trim().lines().filter(|l| !l.trim().is_empty()).next().unwrap_or("");
    let unquoted = first.trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”'));
    unquoted.trim().chars().take(max).collect()
}

fn clean_line_ttt(raw: &str) -> String {
    let no_think = Regex::new(r"(?s)<think[\s\S]*?</think>").unwrap().replace_all(raw, "");
    let no_brackets = Regex::new(r"\[[^\]]*\]").unwrap().replace_all(&no_think, "");
    let first = no_brackets.trim().lines().filter(|l| !l.trim().is_empty()).next().unwrap_or("");
    let unquoted = first.trim_matches(|c| matches!(c, '"' | '\'' | '“' | '”'));
    unquoted.trim().chars().take(140).collect()
}

fn extract_json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

async fn call_llm_timeout(
    state: &AppState,
    messages: &[Value],
    temperature: f64,
    max_tokens: u32,
    timeout_ms: u64,
) -> Option<String> {
    let options = CallOptions {
        temperature: Some(temperature),
        max_tokens: Some(max_tokens as u64),
        ..Default::default()
    };
    let fut = state.llm.call_llm(messages, &options);
    match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
        Ok(Ok(outcome)) => Some(outcome.content),
        _ => None,
    }
}

// ===== Status (selalu terdaftar) =====

pub async fn status_route(user: SessionUser) -> Response {
    ok(json!({ "eligible": game_eligible(&user.0) }))
}

// ===== Tic-Tac-Toe =====

pub async fn ttt_line_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/ttt/line") {
        return res;
    }
    let bond = state.db.load_bond(&user.0).round() as i64;
    let hindari: Vec<String> = body["avoid"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .filter(|x| !x.trim().is_empty())
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
        })
        .map(|v| {
            let skip = v.len().saturating_sub(4);
            v.into_iter().skip(skip).collect()
        })
        .unwrap_or_default();
    let mut aturan = vec![
        "Kamu Yuki, perempuan 19 tahun, tsundere, bicara santai bahasa Indonesia sehari-hari (aku/kamu).".to_string(),
        format!("Kedekatanmu dengan pemain: bond {bond}/100 (makin tinggi makin luluh, tapi tetap tidak mau terlihat kalah)."),
        "Kalian sedang main Tic-Tac-Toe; kamu memegang O.".to_string(),
        "Balas HANYA satu kalimat pendek, maksimal 12 kata, tanpa tanda kutip dan tanpa emoji.".to_string(),
        "Jangan pernah menyebut dirimu AI, bot, asisten, atau model. Jangan menjelaskan aturan permainan.".to_string(),
        "Variasikan pembuka kalimat — jangan mengulang kata pembuka dari komentar sebelumnya.".to_string(),
    ];
    if !hindari.is_empty() {
        aturan.push(format!(
            "Jangan ulangi kalimat ini: {}",
            hindari.iter().map(|x| format!("\"{x}\"")).collect::<Vec<_>>().join("; ")
        ));
    }
    let event = body["event"].as_str().unwrap_or("");
    let konteks = if event == "start" {
        "Permainan baru dimulai. Ledek dia dengan santai, jangan panjang.".to_string()
    } else if event == "result" {
        match body["result"].as_str() {
            Some("user_win") => "Dia menang dan kamu kalah. Jengkel sedikit, jangan berlebihan.".to_string(),
            Some("yuki_win") => "Kamu menang. Cukup puas, jangan sombong berlebihan.".to_string(),
            _ => "Permainan seri. Komentari singkat.".to_string(),
        }
    } else {
        let move_count = body["moveCount"].as_i64().unwrap_or(1);
        format!("Dia baru menaruh X untuk langkah ke-{move_count}. Komentari langkahnya, singkat.")
    };
    let messages = vec![
        json!({ "role": "system", "content": aturan.join("\n") }),
        json!({ "role": "user", "content": konteks }),
    ];
    let content = call_llm_timeout(&state, &messages, 0.95, 64, 4500).await;
    let bersih = content.map(|c| clean_line_ttt(&c)).unwrap_or_default();
    if bersih.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }
    ok(json!({ "line": bersih }))
}

pub async fn ttt_get_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/ttt") {
        return res;
    }
    let sesi = load_session(&state.db, &user.0, GAME_TTT);
    ok(json!({
        "game": sesi.as_ref().and_then(|s| s.state.clone()).unwrap_or(Value::Null),
        "result": sesi.as_ref().and_then(|s| s.result.clone()).map(Value::from).unwrap_or(Value::Null),
        "stats": sesi.map(|s| s.stats).unwrap_or_else(|| json!({ "wins": 0, "losses": 0, "draws": 0 })),
    }))
}

pub async fn ttt_new_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/ttt/new") {
        return res;
    }
    let board = ttt_board();
    save_session(&state.db, &user.0, &board, None, GAME_TTT);
    log_game_event(&state.db, &user.0, GAME_TTT, "start", None);
    ok(json!({ "game": board, "stats": get_stats(&state.db, &user.0) }))
}

pub async fn ttt_move_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/ttt/move") {
        return res;
    }
    let cell_value = &body["cell"];
    let cell: Option<i64> = match cell_value {
        Value::Number(n) => n.as_f64().filter(|v| v.fract() == 0.0).map(|v| v as i64),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.fract() == 0.0)
            .map(|v| v as i64),
        _ => None,
    };
    let Some(cell) = cell else {
        return err(StatusCode::BAD_REQUEST, "Kotak tidak valid.");
    };
    let sesi = load_session(&state.db, &user.0, GAME_TTT);
    let Some(mut game_state) = sesi.as_ref().and_then(|s| s.state.clone()) else {
        return err(StatusCode::BAD_REQUEST, "Belum ada permainan. Mulai dulu.");
    };
    let bond = state.db.load_bond(&user.0);
    let (after_user, error) = ttt_move(&game_state, cell, MARK_USER);
    if let Some(e) = error {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e, "game": game_state })),
        )
            .into_response();
    }
    game_state = after_user;

    let mut yuki_move: Option<i64> = None;
    let mut baris = String::new();
    let mut dari_model = false;
    if game_state["status"].as_str() == Some("playing") {
        let board: Vec<String> = game_state["board"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        let kosong: Vec<usize> = (0..9).filter(|&i| board[i].is_empty()).collect();
        let papan_teks = board
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{i}:{}", if v.is_empty() { "-" } else { v }))
            .collect::<Vec<_>>()
            .join(" ");
        let aturan = [
            "Kamu Yuki, perempuan 19 tahun, tsundere, bicara santai bahasa Indonesia sehari-hari (aku/kamu).".to_string(),
            format!("Kedekatanmu dengan pemain: bond {}/100.", bond.round() as i64),
            "Kalian sedang main Tic-Tac-Toe. Kamu memegang O, dia X.".to_string(),
            "Pilih SATU kotak kosong untuk langkahmu, lalu satu kalimat pendek maksimal 12 kata.".to_string(),
            "Jangan pernah menyebut dirimu AI, bot, asisten, atau model.".to_string(),
            "Balas HANYA JSON tanpa penjelasan: {\"cell\": <angka>, \"line\": \"<kalimat>\"}".to_string(),
        ]
        .join("\n");
        let messages = vec![
            json!({ "role": "system", "content": aturan }),
            json!({
                "role": "user",
                "content": format!(
                    "Papan (indeks:isi): {papan_teks}. Kotak kosong: {}.",
                    kosong.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
                )
            }),
        ];
        let content = call_llm_timeout(&state, &messages, 0.9, 90, 4500).await;
        if let Some(text) = content {
            if let Some(obj) = extract_json_object(&text) {
                if let Some(pilihan) = obj["cell"].as_i64() {
                    if (0..=8).contains(&pilihan) && board[pilihan as usize].is_empty() {
                        yuki_move = Some(pilihan);
                        dari_model = true;
                    }
                }
                baris = clean_line(obj["line"].as_str().unwrap_or(""), 140);
            }
        }
        if yuki_move.is_none() {
            yuki_move = ttt_yuki_move(&board, bond).map(|c| c as i64);
        }
        if let Some(ym) = yuki_move {
            let (after_yuki, err2) = ttt_move(&game_state, ym, MARK_YUKI);
            if err2.is_none() {
                game_state = after_yuki;
            }
        }
    }

    let result: Option<String> = match game_state["status"].as_str() {
        Some("playing") | None => None,
        Some(s) => Some(s.to_string()),
    };
    let stats = save_session(&state.db, &user.0, &game_state, result.as_deref(), GAME_TTT);
    log_game_event(&state.db, &user.0, GAME_TTT, "move", None);
    if let Some(r) = &result {
        log_game_event(&state.db, &user.0, GAME_TTT, "finish", Some(r));
    }
    ok(json!({
        "game": game_state,
        "yukiMove": yuki_move.map(Value::from).unwrap_or(Value::Null),
        "result": result.map(Value::from).unwrap_or(Value::Null),
        "stats": stats,
        "line": baris,
        "source": if dari_model { "yuki" } else { "cadangan" },
        "bond": bond.round() as i64,
    }))
}

pub async fn ttt_reset_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/ttt/reset") {
        return res;
    }
    let board = ttt_board();
    save_session(&state.db, &user.0, &board, None, GAME_TTT);
    log_game_event(&state.db, &user.0, GAME_TTT, "start", None);
    ok(json!({ "ok": true, "game": board, "stats": get_stats(&state.db, &user.0) }))
}

// ===== Batu-Kertas-Gunting =====

pub async fn rps_get_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/rps") {
        return res;
    }
    let sesi = load_session(&state.db, &user.0, GAME_RPS);
    ok(json!({
        "game": sesi.as_ref().and_then(|s| s.state.clone()).unwrap_or(Value::Null),
        "stats": sesi.as_ref().map(|s| s.stats.clone()).unwrap_or_else(|| json!({ "wins": 0, "losses": 0, "draws": 0 })),
        "last": sesi.and_then(|s| s.result).map(Value::from).unwrap_or(Value::Null),
    }))
}

pub async fn rps_play_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/rps/play") {
        return res;
    }
    let pilihan = rps_normalisasi(body["choice"].as_str().unwrap_or(""));
    if pilihan.is_empty() {
        return err(StatusCode::BAD_REQUEST, "Pilih batu, kertas, atau gunting.");
    }
    let sesi = load_session(&state.db, &user.0, GAME_RPS);
    let sesi_baru = sesi
        .as_ref()
        .and_then(|s| s.state.as_ref())
        .map(|st| st.get("score").is_none())
        .unwrap_or(true);
    let game_state = if sesi_baru {
        rps_state_baru()
    } else {
        sesi.as_ref().and_then(|s| s.state.clone()).unwrap_or_else(rps_state_baru)
    };
    let bond = state.db.load_bond(&user.0).round() as i64;
    let riwayat = game_state["history"]
        .as_array()
        .map(|h| {
            let tail: Vec<String> = h
                .iter()
                .rev()
                .take(3)
                .rev()
                .map(|r| format!("dia {}, kamu {}", r["you"].as_str().unwrap_or(""), r["yuki"].as_str().unwrap_or("")))
                .collect();
            if tail.is_empty() { "belum ada".to_string() } else { tail.join("; ") }
        })
        .unwrap_or_else(|| "belum ada".to_string());
    let aturan = [
        "Kamu Yuki, perempuan 19 tahun, tsundere, bicara santai bahasa Indonesia sehari-hari (aku/kamu).".to_string(),
        format!("Kedekatanmu dengan pemain: bond {bond}/100."),
        "Kalian main batu-kertas-gunting. Pilih satu: batu, kertas, atau gunting.".to_string(),
        "Riwayat hanya untuk membaca pola — JANGAN meniru langkah terakhirnya, dan jangan memilih pilihan yang sama terus.".to_string(),
        format!("Kamu boleh sesekali lengah kalau bond tinggi (bond {bond})."),
        "Jangan pernah menyebut dirimu AI, bot, asisten, atau model.".to_string(),
        "Balas HANYA JSON: {\"pick\":\"batu|kertas|gunting\",\"line\":\"satu kalimat maks 10 kata\"}".to_string(),
    ]
    .join("\n");
    let messages = vec![
        json!({ "role": "system", "content": aturan }),
        json!({ "role": "user", "content": format!("Riwayat ronde sebelumnya: {riwayat}. Pilih langkahmu sekarang.") }),
    ];
    let content = call_llm_timeout(&state, &messages, 0.9, 80, 3500).await;
    let mut pick = String::new();
    let mut baris = String::new();
    if let Some(text) = content {
        if let Some(obj) = extract_json_object(&text) {
            pick = rps_normalisasi(obj["pick"].as_str().unwrap_or(""));
            baris = clean_line(obj["line"].as_str().unwrap_or(""), 120);
        }
    }
    let dari_model = !pick.is_empty();
    if pick.is_empty() {
        pick = rps_acak();
    }
    let hasil = rps_hasil(&pilihan, &pick);
    let next = rps_catat(&game_state, &pilihan, &pick, &hasil);
    let stats = save_session(&state.db, &user.0, &next, Some(&hasil), GAME_RPS);
    if sesi_baru {
        log_game_event(&state.db, &user.0, GAME_RPS, "start", None);
    }
    log_game_event(&state.db, &user.0, GAME_RPS, "round", Some(&hasil));
    // Jeda buatan supaya tidak terasa instan seperti mesin.
    tokio::time::sleep(Duration::from_millis(500)).await;
    ok(json!({
        "you": pilihan,
        "yuki": pick,
        "hasil": hasil,
        "line": baris,
        "state": next,
        "stats": stats,
        "source": if dari_model { "yuki" } else { "cadangan" },
    }))
}

pub async fn rps_reset_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Some(res) = beta_gate(&state, &user, "/api/game/rps/reset") {
        return res;
    }
    let baru = rps_state_baru();
    let stats = save_session(&state.db, &user.0, &baru, None, GAME_RPS);
    log_game_event(&state.db, &user.0, GAME_RPS, "start", None);
    ok(json!({ "game": baru, "stats": stats }))
}
