//! Port `lib/ip-block.js` + `recordStrike` server.js (Fase 5):
//! blocklist IP persisten di tabel ip_blocks + auto-block berbasis
//! strike (10 strike/10 menit -> blokir 1 jam; level berikutnya 24 jam).

use crate::db::Db;
use crate::state::AppState;
use serde_json::{json, Value};

pub fn clean_ip(ip: &str) -> String {
    let mut s: String = ip.trim().chars().take(64).collect::<String>().to_lowercase();
    if let Some(rest) = s.strip_prefix("::ffff:") {
        s = rest.to_string();
    }
    s
}

pub fn is_ip_blocked(db: &Db, ip: &str) -> bool {
    let key = clean_ip(ip);
    if key.is_empty() {
        return false;
    }
    match db.ip_block_until(&key) {
        Some(until) => {
            if until <= crate::emotion::now_ms() {
                db.ip_block_delete(&key);
                false
            } else {
                true
            }
        }
        None => false,
    }
}

pub fn block_ip(db: &Db, ip: &str, hours: f64, reason: &str) -> Option<Value> {
    let key = clean_ip(ip);
    if key.is_empty() {
        return None;
    }
    let h = hours.min(24.0 * 30.0).max(0.1);
    let until = crate::emotion::now_ms() + (h * 3_600_000.0) as i64;
    let reason_clean: String = reason.chars().take(120).collect();
    db.ip_block_upsert(&key, until, &reason_clean);
    Some(json!({ "ip": key, "blockedUntil": until }))
}

pub fn unblock_ip(db: &Db, ip: &str) -> Value {
    json!({ "removed": db.ip_block_delete(&clean_ip(ip)) })
}

pub fn list_blocked_ips(db: &Db) -> Vec<Value> {
    db.ip_blocks_list()
}

/// Kumpulkan strike dari pelanggaran rate-limit & login gagal.
pub fn record_strike(state: &AppState, ip: &str, reason: &str) {
    if ip.is_empty() {
        return;
    }
    let now = crate::emotion::now_ms();
    let mut strikes = state.strikes.lock().unwrap();
    let entry = strikes.get(ip).cloned();
    match entry {
        None => {
            strikes.insert(ip.to_string(), (1, now, 0));
        }
        Some((count, window_start, level)) => {
            if now - window_start > 10 * 60_000 {
                strikes.insert(ip.to_string(), (1, now, level));
            } else if count + 1 >= 10 {
                let new_level = level + 1;
                strikes.remove(ip);
                drop(strikes);
                block_ip(&state.db, ip, if new_level > 1 { 24.0 } else { 1.0 }, &format!("{reason} x{}", count + 1));
            } else {
                strikes.insert(ip.to_string(), (count + 1, window_start, level));
            }
        }
    }
}
