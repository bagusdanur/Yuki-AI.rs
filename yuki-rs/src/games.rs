//! Port `lib/games/` (Fase 5): store sesi game (DB), mesin tic-tac-toe
//! murni (minimax + slip mengikuti bond), dan batu-kertas-gunting.
//! State disimpan sebagai JSON di game_sessions — format sama dengan
//! Node sehingga sesi bisa dilanjutkan lintas engine.

use crate::db::Db;
use rusqlite::params;
use serde_json::{json, Value};

pub const GAME_TTT: &str = "tic-tac-toe";
pub const GAME_RPS: &str = "batu-kertas-gunting";

// === Store (lib/games/store.js) ===

pub struct LoadedSession {
    pub state: Option<Value>,
    pub result: Option<String>,
    pub stats: Value,
    pub updated_at: Option<String>,
}

pub fn load_session(db: &Db, user_id: &str, game: &str) -> Option<LoadedSession> {
    let conn = db.conn.lock().unwrap();
    let row = conn
        .query_row(
            "SELECT state, result, wins, losses, draws, updated_at FROM game_sessions WHERE user_id = ? AND game = ?",
            params![crate::db::uid(user_id), game],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .ok()?;
    let (state_s, result, wins, losses, draws, updated_at) = row;
    Some(LoadedSession {
        state: state_s.and_then(|s| serde_json::from_str(&s).ok()),
        result,
        stats: json!({ "wins": wins, "losses": losses, "draws": draws }),
        updated_at,
    })
}

pub fn save_session(
    db: &Db,
    user_id: &str,
    state: &Value,
    result: Option<&str>,
    game: &str,
) -> Value {
    let u = crate::db::uid(user_id);
    let existing = load_session(db, &u, game);
    let (mut wins, mut losses, mut draws) = match &existing {
        Some(e) => (
            e.stats["wins"].as_i64().unwrap_or(0),
            e.stats["losses"].as_i64().unwrap_or(0),
            e.stats["draws"].as_i64().unwrap_or(0),
        ),
        None => (0, 0, 0),
    };
    // Hasil hanya dihitung sekali per sesi (state + result sama = sudah).
    let already_counted = match (&existing, result) {
        (Some(e), Some(r)) => {
            e.result.as_deref() == Some(r) && e.state.as_ref() == Some(state)
        }
        _ => false,
    };
    if let Some(r) = result {
        if !already_counted {
            match r {
                "user_win" => wins += 1,
                "yuki_win" => losses += 1,
                "draw" => draws += 1,
                _ => {}
            }
        }
    }
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO game_sessions (user_id, game, state, result, wins, losses, draws, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now')) \
         ON CONFLICT(user_id, game) DO UPDATE SET state = excluded.state, result = excluded.result, \
         wins = excluded.wins, losses = excluded.losses, draws = excluded.draws, updated_at = excluded.updated_at",
        params![u, game, state.to_string(), result, wins, losses, draws],
    )
    .ok();
    json!({ "wins": wins, "losses": losses, "draws": draws })
}

pub fn clear_session(db: &Db, user_id: &str) {
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "DELETE FROM game_sessions WHERE user_id = ? AND game = ?",
        params![crate::db::uid(user_id), GAME_TTT],
    )
    .ok();
}

pub fn get_stats(db: &Db, user_id: &str) -> Value {
    match load_session(db, user_id, GAME_TTT) {
        Some(s) => s.stats,
        None => json!({ "wins": 0, "losses": 0, "draws": 0 }),
    }
}

pub fn log_game_event(db: &Db, user_id: &str, game: &str, event: &str, result: Option<&str>) {
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO game_events (user_id, game, event, result, created_at) VALUES (?, ?, ?, ?, datetime('now'))",
        params![crate::db::uid(user_id), game, event, result],
    )
    .ok();
}

fn hari_wib_from_ms(ms: i64) -> String {
    let shifted = ms + 7 * 3600 * 1000;
    let dt = chrono::DateTime::from_timestamp_millis(shifted).unwrap_or_default();
    dt.format("%Y-%m-%d").to_string()
}

pub fn game_memory_text(db: &Db, user_id: &str) -> String {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(
        "SELECT game, result, wins, losses, draws, updated_at FROM game_sessions WHERE user_id = ?",
    ) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let rows: Vec<(String, Option<String>, i64, i64, i64, Option<String>)> = stmt
        .query_map(params![crate::db::uid(user_id)], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    drop(stmt);
    drop(conn);
    if rows.is_empty() {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    for (game, result, wins, losses, draws, updated_at) in rows {
        if result.is_none() && wins == 0 && losses == 0 && draws == 0 {
            continue;
        }
        let mut kapan = String::new();
        if let Some(updated) = &updated_at {
            // updated_at SQLite UTC "YYYY-MM-DD HH:MM:SS" -> ms epoch.
            if let Some(ms) = parse_sqlite_utc_ms(updated) {
                let lalu = hari_wib_from_ms(ms);
                let kini = hari_wib_from_ms(crate::emotion::now_ms());
                let parse_day = |s: &str| {
                    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis())
                        .unwrap_or(0)
                };
                let selisih = ((parse_day(&kini) - parse_day(&lalu)) as f64 / 86_400_000.0).round() as i64;
                kapan = if selisih <= 0 {
                    " hari ini".to_string()
                } else if selisih == 1 {
                    " kemarin".to_string()
                } else {
                    format!(" {selisih} hari lalu")
                };
            }
        }
        let hasil = match result.as_deref() {
            Some("user_win") => "dia menang",
            Some("yuki_win") => "kamu yang menang",
            Some("draw") => "seri",
            _ => "sedang berjalan",
        };
        let nama = match game.as_str() {
            GAME_TTT => "Tic-tac-toe",
            GAME_RPS => "Batu-kertas-gunting",
            _ => &game,
        };
        parts.push(format!(
            "{nama} terakhir{kapan}: {hasil} (rekor: dia {wins}, kamu {losses}, seri {draws})."
        ));
    }
    parts.join(" ")
}

fn parse_sqlite_utc_ms(s: &str) -> Option<i64> {
    let naive = chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    Some(naive.and_utc().timestamp_millis())
}

/// Ringkasan pemakaian game untuk dashboard admin (gameAnalytics).
pub fn game_analytics(db: &Db) -> Value {
    let conn = db.conn.lock().unwrap();
    let one = |sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
    };
    let hari_ini = "date(created_at, '+7 hours') = date('now', '+7 hours')";
    let total_starts = one("SELECT COUNT(*) FROM game_events WHERE event = 'start'");
    let total_rounds = one("SELECT COUNT(*) FROM game_events WHERE event IN ('move','round')");
    let finishes = one("SELECT COUNT(*) FROM game_events WHERE event = 'finish'");
    let players = one("SELECT COUNT(DISTINCT user_id) FROM game_events");
    let today_starts = one(&format!(
        "SELECT COUNT(*) FROM game_events WHERE event = 'start' AND {hari_ini}"
    ));
    let today_players = one(&format!(
        "SELECT COUNT(DISTINCT user_id) FROM game_events WHERE {hari_ini}"
    ));
    let mut by_game: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT game, SUM(CASE WHEN event = 'start' THEN 1 ELSE 0 END), \
         SUM(CASE WHEN event IN ('move','round') THEN 1 ELSE 0 END), \
         SUM(CASE WHEN event = 'finish' THEN 1 ELSE 0 END), COUNT(DISTINCT user_id) \
         FROM game_events GROUP BY game ORDER BY 2 DESC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({
                "game": r.get::<_, String>(0)?,
                "starts": r.get::<_, i64>(1)?,
                "rounds": r.get::<_, i64>(2)?,
                "finishes": r.get::<_, i64>(3)?,
                "players": r.get::<_, i64>(4)?,
            }))
        }) {
            by_game = rows.filter_map(|r| r.ok()).collect();
        }
    }
    let mut results = json!({ "user_win": 0, "yuki_win": 0, "draw": 0 });
    if let Ok(mut stmt) = conn
        .prepare("SELECT result, COUNT(*) FROM game_events WHERE result IS NOT NULL GROUP BY result")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            for (result, n) in rows.filter_map(|r| r.ok()) {
                if results.get(&result).is_some() {
                    results[&result] = json!(n);
                }
            }
        }
    }
    let mut daily: Vec<Value> = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT date(created_at, '+7 hours'), SUM(CASE WHEN event = 'start' THEN 1 ELSE 0 END), \
         COUNT(DISTINCT user_id) FROM game_events \
         WHERE date(created_at, '+7 hours') >= date('now', '+7 hours', '-6 days') \
         GROUP BY 1 ORDER BY 1",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(json!({
                "hari": r.get::<_, String>(0)?,
                "starts": r.get::<_, i64>(1)?,
                "players": r.get::<_, i64>(2)?,
            }))
        }) {
            daily = rows.filter_map(|r| r.ok()).collect();
        }
    }
    drop(conn);
    let completion = by_game
        .iter()
        .find(|g| g["game"] == json!(GAME_TTT))
        .map(|ttt| {
            let starts = ttt["starts"].as_i64().unwrap_or(0);
            if starts == 0 {
                0
            } else {
                ((ttt["finishes"].as_i64().unwrap_or(0) as f64 / starts as f64) * 100.0)
                    .round()
                    .min(100.0) as i64
            }
        })
        .unwrap_or(0);
    json!({
        "totalStarts": total_starts,
        "totalRounds": total_rounds,
        "finishes": finishes,
        "players": players,
        "todayStarts": today_starts,
        "todayPlayers": today_players,
        "completionRate": completion,
        "byGame": by_game,
        "results": results,
        "daily": daily,
    })
}

// === Tic-Tac-Toe (lib/games/tic-tac-toe.js) ===

pub const MARK_USER: &str = "X";
pub const MARK_YUKI: &str = "O";
const LINES: [[usize; 3]; 8] = [
    [0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 3, 6], [1, 4, 7], [2, 5, 8], [0, 4, 8], [2, 4, 6],
];

pub fn ttt_board() -> Value {
    json!({
        "board": ["", "", "", "", "", "", "", "", ""],
        "turn": MARK_USER,
        "status": "playing",
        "winnerLine": null,
        "moves": 0,
    })
}

fn board_of(state: &Value) -> Vec<String> {
    state["board"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_else(|| vec!["".to_string(); 9])
}

fn legal_moves(board: &[String]) -> Vec<usize> {
    (0..9).filter(|&i| board[i].is_empty()).collect()
}

fn find_winner(board: &[String]) -> Option<(String, [usize; 3])> {
    for line in LINES {
        let [a, b, c] = line;
        if !board[a].is_empty() && board[a] == board[b] && board[a] == board[c] {
            return Some((board[a].clone(), line));
        }
    }
    None
}

fn minimax(board: &mut Vec<String>, is_yuki: bool, depth: i64) -> i64 {
    if let Some((mark, _)) = find_winner(board) {
        return if mark == MARK_YUKI { 10 - depth } else { depth - 10 };
    }
    let moves = legal_moves(board);
    if moves.is_empty() {
        return 0;
    }
    let mut best = if is_yuki { i64::MIN } else { i64::MAX };
    for cell in moves {
        board[cell] = if is_yuki { MARK_YUKI } else { MARK_USER }.to_string();
        let value = minimax(board, !is_yuki, depth + 1);
        board[cell] = String::new();
        best = if is_yuki { best.max(value) } else { best.min(value) };
    }
    best
}

fn rank_moves(board: &[String]) -> Vec<(usize, i64)> {
    let mut hasil: Vec<(usize, i64)> = legal_moves(board)
        .into_iter()
        .map(|cell| {
            let mut uji = board.to_vec();
            uji[cell] = MARK_YUKI.to_string();
            (cell, minimax(&mut uji, false, 1))
        })
        .collect();
    hasil.sort_by(|a, b| b.1.cmp(&a.1));
    hasil
}

pub fn slip_chance(bond_value: f64) -> f64 {
    if bond_value >= 78.0 {
        0.55
    } else if bond_value >= 50.0 {
        0.35
    } else if bond_value >= 24.0 {
        0.15
    } else {
        0.0
    }
}

pub fn ttt_yuki_move(board: &[String], bond_value: f64) -> Option<usize> {
    let ranked = rank_moves(board);
    if ranked.is_empty() {
        return None;
    }
    let slip = slip_chance(bond_value);
    if slip > 0.0 && rand::random::<f64>() < slip {
        let pilihan = &ranked[1..ranked.len().min(3)];
        if !pilihan.is_empty() {
            let idx = (rand::random::<f64>() * pilihan.len() as f64).floor() as usize;
            return Some(pilihan[idx.min(pilihan.len() - 1)].0);
        }
    }
    Some(ranked[0].0)
}

/// Terapkan langkah; (state baru, error).
pub fn ttt_move(state: &Value, cell: i64, mark: &str) -> (Value, Option<String>) {
    if state["status"].as_str() != Some("playing") {
        return (state.clone(), Some("Permainan sudah selesai.".to_string()));
    }
    if state["turn"].as_str() != Some(mark) {
        return (state.clone(), Some("Bukan giliran itu.".to_string()));
    }
    if !(0..=8).contains(&cell) {
        return (state.clone(), Some("Kotak tidak valid.".to_string()));
    }
    let mut board = board_of(state);
    if !board[cell as usize].is_empty() {
        return (state.clone(), Some("Kotak itu sudah terisi.".to_string()));
    }
    board[cell as usize] = mark.to_string();
    let win = find_winner(&board);
    let status = if let Some((ref mark_win, _)) = win {
        if mark_win == MARK_USER { "user_win" } else { "yuki_win" }
    } else if legal_moves(&board).is_empty() {
        "draw"
    } else {
        "playing"
    };
    let winner_line: Value = match &win {
        Some((_, line)) => json!(line),
        None => Value::Null,
    };
    (
        json!({
            "board": board,
            "turn": if mark == MARK_USER { MARK_YUKI } else { MARK_USER },
            "status": status,
            "winnerLine": winner_line,
            "moves": state["moves"].as_i64().unwrap_or(0) + 1,
        }),
        None,
    )
}

// === Batu-Kertas-Gunting (lib/games/rps.js) ===

pub fn rps_normalisasi(x: &str) -> String {
    let s = x.to_lowercase();
    let s = s.trim();
    if s.contains("batu") || s == "rock" || s == "r" {
        "batu".to_string()
    } else if s.contains("kertas") || s == "paper" || s == "p" {
        "kertas".to_string()
    } else if s.contains("gunting") || s == "scissors" || s == "s" {
        "gunting".to_string()
    } else {
        String::new()
    }
}

/// 'user_win' | 'yuki_win' | 'draw' | '' (tidak valid)
pub fn rps_hasil(pemain: &str, yuki: &str) -> String {
    let a = rps_normalisasi(pemain);
    let b = rps_normalisasi(yuki);
    if a.is_empty() || b.is_empty() {
        return String::new();
    }
    if a == b {
        return "draw".to_string();
    }
    let kalahkan = |x: &str| match x {
        "batu" => "gunting",
        "gunting" => "kertas",
        _ => "batu",
    };
    if kalahkan(&a) == b {
        "user_win".to_string()
    } else {
        "yuki_win".to_string()
    }
}

pub fn rps_acak() -> String {
    const PILIHAN: [&str; 3] = ["batu", "kertas", "gunting"];
    PILIHAN[(rand::random::<f64>() * 3.0).floor() as usize % 3].to_string()
}

pub fn rps_state_baru() -> Value {
    json!({ "score": { "you": 0, "yuki": 0 }, "round": 0, "history": [] })
}

pub fn rps_catat(state: &Value, pilihan_pemain: &str, pilihan_yuki: &str, hasil: &str) -> Value {
    let s = if state.get("score").is_some() { state.clone() } else { rps_state_baru() };
    let mut history = s["history"].as_array().cloned().unwrap_or_default();
    history.push(json!({ "you": pilihan_pemain, "yuki": pilihan_yuki, "hasil": hasil }));
    if history.len() > 5 {
        history = history.split_off(history.len() - 5);
    }
    json!({
        "score": {
            "you": s["score"]["you"].as_i64().unwrap_or(0) + if hasil == "user_win" { 1 } else { 0 },
            "yuki": s["score"]["yuki"].as_i64().unwrap_or(0) + if hasil == "yuki_win" { 1 } else { 0 },
        },
        "round": s["round"].as_i64().unwrap_or(0) + 1,
        "history": history,
    })
}
