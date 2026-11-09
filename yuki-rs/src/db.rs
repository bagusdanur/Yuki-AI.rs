//! Lapisan SQLite — subset Fase 1 dari `lib/memory.js`.
//!
//! Memakai file `yuki.db` yang sama dengan versi Node dan DDL yang sama
//! untuk tabel: users, chat_history, rate_limits, emotion,
//! relationship_events, facts. Tabel lain dibuat di fasenya masing-masing.

use anyhow::Context;
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;

type HmacSha256 = Hmac<Sha256>;

/// DDL lengkap dari lib/memory.js + lib/agent/memory-store.js — Rust membuat
/// semua tabel yang sama agar file DB bisa ditukar dengan versi Node.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS facts (user_id TEXT NOT NULL, text TEXT NOT NULL, vec TEXT, created_at TEXT DEFAULT (datetime('now')), UNIQUE(user_id, text));
CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, summary TEXT NOT NULL, mood TEXT, vec TEXT, ts TEXT DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS emotion (user_id TEXT PRIMARY KEY, state TEXT, updated_at TEXT);
CREATE TABLE IF NOT EXISTS user_profile (user_id TEXT PRIMARY KEY, profile_summary TEXT, updated_at TEXT);
CREATE TABLE IF NOT EXISTS users (user_id TEXT PRIMARY KEY, username TEXT, access_code TEXT UNIQUE, created_at TEXT DEFAULT (datetime('now')), access_code_hash TEXT, session_version INTEGER DEFAULT 1, session_jti TEXT);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_access_code_hash ON users(access_code_hash) WHERE access_code_hash IS NOT NULL;
CREATE TABLE IF NOT EXISTS chat_history (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT, role TEXT, content TEXT, created_at TEXT DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS chat_summary (user_id TEXT PRIMARY KEY, summary TEXT, updated_at TEXT);
CREATE TABLE IF NOT EXISTS structured_memory (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, category TEXT NOT NULL, content TEXT NOT NULL, status TEXT DEFAULT 'active', importance INTEGER DEFAULT 1, updated_at TEXT DEFAULT (datetime('now')), UNIQUE(user_id, category, content));
CREATE TABLE IF NOT EXISTS agent_memories (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, scope_type TEXT NOT NULL DEFAULT 'user', scope_id TEXT NOT NULL DEFAULT '', category TEXT NOT NULL, content TEXT NOT NULL, importance INTEGER NOT NULL DEFAULT 1, confidence REAL NOT NULL DEFAULT 0.75, source_message_id INTEGER, evidence_json TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL DEFAULT 'active', expires_at TEXT, created_at TEXT DEFAULT (datetime('now')), updated_at TEXT DEFAULT (datetime('now')), UNIQUE(user_id, scope_type, scope_id, category, content));
CREATE INDEX IF NOT EXISTS idx_agent_memories_scope ON agent_memories(user_id, scope_type, scope_id, status);
CREATE TABLE IF NOT EXISTS memory_settings (user_id TEXT PRIMARY KEY, auto_capture INTEGER NOT NULL DEFAULT 1, updated_at TEXT DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS relationship_events (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, kind TEXT NOT NULL, title TEXT NOT NULL, detail TEXT, bond_value REAL DEFAULT 0, created_at TEXT DEFAULT (datetime('now')), UNIQUE(user_id, kind, title));
CREATE TABLE IF NOT EXISTS response_feedback (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, message_id INTEGER, rating INTEGER NOT NULL, reason TEXT, created_at TEXT DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS rate_limits (bucket_key TEXT PRIMARY KEY, hits INTEGER NOT NULL DEFAULT 0, reset_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ip_blocks (ip TEXT PRIMARY KEY, blocked_until INTEGER NOT NULL, reason TEXT DEFAULT '', created_at TEXT DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS idx_ip_blocks_until ON ip_blocks(blocked_until);
CREATE TABLE IF NOT EXISTS game_events (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, game TEXT NOT NULL, event TEXT NOT NULL, result TEXT, created_at TEXT DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS idx_game_events_created ON game_events(created_at);
CREATE TABLE IF NOT EXISTS game_sessions (user_id TEXT NOT NULL, game TEXT NOT NULL, state TEXT NOT NULL, result TEXT, wins INTEGER NOT NULL DEFAULT 0, losses INTEGER NOT NULL DEFAULT 0, draws INTEGER NOT NULL DEFAULT 0, updated_at TEXT DEFAULT (datetime('now')), PRIMARY KEY (user_id, game));
CREATE TABLE IF NOT EXISTS comic_bookmarks (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, title TEXT NOT NULL, url TEXT NOT NULL, format TEXT, genre TEXT, chapter TEXT, score TEXT, image TEXT, created_at TEXT DEFAULT (datetime('now')), UNIQUE(user_id, url));
CREATE TABLE IF NOT EXISTS user_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL, category TEXT DEFAULT 'general', idempotency_key TEXT, created_at TEXT DEFAULT (datetime('now')), updated_at TEXT DEFAULT (datetime('now')));
CREATE UNIQUE INDEX IF NOT EXISTS idx_user_notes_idempotency ON user_notes(user_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS user_artifacts (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, title TEXT NOT NULL, type TEXT DEFAULT 'html', content TEXT NOT NULL, version INTEGER DEFAULT 1, patch_note TEXT, created_at TEXT DEFAULT (datetime('now')), updated_at TEXT DEFAULT (datetime('now')));
CREATE TABLE IF NOT EXISTS scheduled_tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, title TEXT NOT NULL, description TEXT NOT NULL, cron_expr TEXT NOT NULL, human_schedule TEXT NOT NULL, run_once INTEGER DEFAULT 0, run_at TEXT, last_run TEXT, next_run TEXT, next_run_at_utc TEXT, timezone TEXT DEFAULT 'Asia/Jakarta', idempotency_key TEXT, status TEXT DEFAULT 'active', created_at TEXT DEFAULT (datetime('now')));
CREATE INDEX IF NOT EXISTS idx_scheduled_tasks_user ON scheduled_tasks(user_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_scheduled_tasks_idempotency ON scheduled_tasks(user_id, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS scheduled_occurrences (id TEXT PRIMARY KEY, task_id INTEGER NOT NULL, user_id TEXT NOT NULL, scheduled_for TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', attempt_count INTEGER NOT NULL DEFAULT 0, next_attempt_at TEXT, last_error TEXT, claimed_at TEXT, delivered_at TEXT, created_at TEXT DEFAULT (datetime('now')), UNIQUE(task_id, scheduled_for));
CREATE INDEX IF NOT EXISTS idx_occurrence_due ON scheduled_occurrences(status, next_attempt_at);
CREATE INDEX IF NOT EXISTS idx_occurrence_user ON scheduled_occurrences(user_id, created_at DESC);
CREATE TABLE IF NOT EXISTS push_subscriptions (endpoint TEXT PRIMARY KEY, user_id TEXT NOT NULL, subscription_json TEXT NOT NULL, created_at TEXT DEFAULT (datetime('now')), last_success_at TEXT, failure_count INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS idx_push_user ON push_subscriptions(user_id);
CREATE TABLE IF NOT EXISTS agent_runs (request_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, goal TEXT NOT NULL DEFAULT '', state TEXT NOT NULL DEFAULT 'queued', result_json TEXT, error_code TEXT, cancel_requested_at TEXT, lease_owner TEXT, lease_expires_at INTEGER, created_at TEXT DEFAULT (datetime('now')), updated_at TEXT DEFAULT (datetime('now')), completed_at TEXT);
CREATE TABLE IF NOT EXISTS agent_run_steps (request_id TEXT NOT NULL, step_id TEXT NOT NULL, sequence INTEGER NOT NULL DEFAULT 0, payload_json TEXT NOT NULL, updated_at TEXT DEFAULT (datetime('now')), PRIMARY KEY (request_id, step_id));
CREATE INDEX IF NOT EXISTS idx_agent_runs_user_state ON agent_runs(user_id, state, updated_at);
CREATE INDEX IF NOT EXISTS idx_agent_steps_run_sequence ON agent_run_steps(request_id, sequence);
CREATE TABLE IF NOT EXISTS agent_workflows (id TEXT PRIMARY KEY, request_id TEXT NOT NULL, user_id TEXT NOT NULL, state TEXT NOT NULL, idempotency_key TEXT NOT NULL UNIQUE, checkpoint_json TEXT NOT NULL, result_json TEXT, error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, expires_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_agent_workflows_user_state ON agent_workflows(user_id, state, updated_at);
CREATE INDEX IF NOT EXISTS idx_agent_workflows_request ON agent_workflows(request_id, user_id);
CREATE TABLE IF NOT EXISTS agent_skills (id TEXT PRIMARY KEY, owner_type TEXT NOT NULL DEFAULT 'user', owner_id TEXT NOT NULL, name TEXT NOT NULL, version TEXT NOT NULL DEFAULT '1.0.0', description TEXT NOT NULL, instructions TEXT NOT NULL, tool_allowlist_json TEXT NOT NULL DEFAULT '[]', input_schema_json TEXT NOT NULL DEFAULT '{}', validation_json TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'draft', created_at TEXT DEFAULT (datetime('now')), updated_at TEXT DEFAULT (datetime('now')), UNIQUE(owner_id, name));
CREATE INDEX IF NOT EXISTS idx_agent_skills_owner ON agent_skills(owner_id, status);
CREATE TABLE IF NOT EXISTS self_improvements (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, topic TEXT NOT NULL, skill TEXT NOT NULL, category TEXT NOT NULL DEFAULT 'technical', scope TEXT NOT NULL DEFAULT 'user', summary TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_self_improvements_user_created ON self_improvements(user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_facts_user ON facts(user_id);
CREATE INDEX IF NOT EXISTS idx_events_user ON events(user_id);
CREATE INDEX IF NOT EXISTS idx_chat_history_user ON chat_history(user_id);
CREATE INDEX IF NOT EXISTS idx_structured_memory_user ON structured_memory(user_id, category, status);
CREATE INDEX IF NOT EXISTS idx_relationship_events_user ON relationship_events(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_comic_bookmarks_user ON comic_bookmarks(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_user_notes ON user_notes(user_id, category);
CREATE INDEX IF NOT EXISTS idx_user_artifacts ON user_artifacts(user_id, updated_at);
";

pub const MAX_FACTS: i64 = 60;
pub const MAX_EVENTS: i64 = 30;
pub const RECALL_TOPK: usize = 5;

pub struct Db {
    pub(crate) conn: Mutex<Connection>,
    pepper: String,
}

pub fn uid(user_id: &str) -> String {
    user_id.chars().take(64).collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path, pepper: String) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("membuat folder {}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("membuka database {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        // Kolom tambahan pada DB lama yang dibuat Node sebelum ALTER-nya ada.
        for alter in [
            "ALTER TABLE users ADD COLUMN access_code_hash TEXT",
            "ALTER TABLE users ADD COLUMN session_version INTEGER DEFAULT 1",
            "ALTER TABLE users ADD COLUMN session_jti TEXT",
        ] {
            let _ = conn.execute_batch(alter); // gagal bila kolom sudah ada: abaikan, seperti Node
        }
        Ok(Self {
            conn: Mutex::new(conn),
            pepper,
        })
    }

    pub fn open_memory(pepper: String) -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            pepper,
        })
    }

    fn hash_access_code(&self, code: &str) -> String {
        let clean = code.trim().to_uppercase();
        let mut mac = HmacSha256::new_from_slice(self.pepper.as_bytes())
            .expect("HMAC menerima kunci berapa pun");
        mac.update(clean.as_bytes());
        hex(&mac.finalize().into_bytes())
    }

    fn legacy_hash_access_code(&self, code: &str) -> String {
        let clean = code.trim().to_uppercase();
        hex(&Sha256::digest(clean.as_bytes()))
    }

    pub fn user_exists(&self, user_id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT 1 FROM users WHERE user_id = ?",
            params![uid(user_id)],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn get_username(&self, user_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT username FROM users WHERE user_id = ?",
            params![uid(user_id)],
            |r| r.get(0),
        )
        .ok()
    }

    // === Sesi ===
    pub fn get_session_state(&self, user_id: &str) -> Option<(i64, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT session_version, session_jti FROM users WHERE user_id = ?",
            params![uid(user_id)],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(1),
                    r.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .ok()
    }

    pub fn set_session_jti(&self, user_id: &str, jti: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE users SET session_jti = ? WHERE user_id = ?",
            params![jti, uid(user_id)],
        );
    }

    pub fn bump_session_version(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE users SET session_version = COALESCE(session_version, 1) + 1, session_jti = NULL WHERE user_id = ?",
            params![uid(user_id)],
        );
    }

    // === User & kode akses ===
    fn generate_access_code(&self, conn: &Connection) -> String {
        const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let gen = |len: usize| -> String {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            (0..len)
                .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
                .collect()
        };
        for _ in 0..100 {
            let code = format!("YUKI-{}-{}", gen(4), gen(4));
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM users WHERE access_code = ? OR access_code_hash = ?",
                    params![code, self.hash_access_code(&code)],
                    |_| Ok(()),
                )
                .is_ok();
            if !exists {
                return code;
            }
        }
        format!("YUKI-{:X}", now_ms())
    }

    /// registerUser di Node. Bedanya: fakta benih disimpan tanpa vektor
    /// embedding (embedder masuk Fase 2).
    pub fn register_user(&self, username: &str) -> anyhow::Result<(String, String)> {
        let conn = self.conn.lock().unwrap();
        let access_code = self.generate_access_code(&conn);
        let user_id = {
            use rand::Rng;
            let rand_part: String = rand::thread_rng()
                .sample_iter(&rand::distributions::Alphanumeric)
                .take(6)
                .map(|b| (b as char).to_ascii_lowercase())
                .collect();
            format!("u_{}{}", base36(now_ms() as u64), rand_part)
        };
        conn.execute(
            "INSERT INTO users (user_id, username, access_code, access_code_hash) VALUES (?, ?, NULL, ?)",
            params![user_id, username, self.hash_access_code(&access_code)],
        )?;
        // Fakta benih "Nama user adalah ..." disimpan route lewat
        // memory::add_facts agar ikut ter-embedding, seperti addFacts di Node.
        Ok((user_id, access_code))
    }

    pub fn username(&self, user_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT username FROM users WHERE user_id = ?",
            params![uid(user_id)],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    pub fn username_taken_recently(&self, username: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE username = ? AND datetime(created_at) > datetime('now', '-1 day')",
                params![username],
                |r| r.get(0),
            )
            .unwrap_or(0);
        count > 0
    }

    pub fn get_user_by_access_code(&self, code: &str) -> Option<(String, Option<String>)> {
        if code.trim().is_empty() {
            return None;
        }
        let clean = code.trim().to_uppercase();
        let conn = self.conn.lock().unwrap();
        let find = |hash: &str| -> Option<(String, Option<String>)> {
            conn.query_row(
                "SELECT user_id, username FROM users WHERE access_code_hash = ?",
                params![hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        };
        if let Some(row) = find(&self.hash_access_code(&clean)) {
            return Some(row);
        }
        if let Some(row) = find(&self.legacy_hash_access_code(&clean)) {
            let _ = conn.execute(
                "UPDATE users SET access_code_hash = ? WHERE user_id = ?",
                params![self.hash_access_code(&clean), row.0],
            );
            return Some(row);
        }
        if let Ok(row) = conn.query_row(
            "SELECT user_id, username FROM users WHERE access_code = ?",
            params![clean],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        ) {
            let _ = conn.execute(
                "UPDATE users SET access_code_hash = ?, access_code = NULL WHERE user_id = ?",
                params![self.hash_access_code(&clean), row.0],
            );
            return Some(row);
        }
        None
    }

    // === Riwayat chat ===
    pub fn save_chat_message(&self, user_id: &str, role: &str, content: &str) -> i64 {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO chat_history (user_id, role, content) VALUES (?, ?, ?)",
            params![uid(user_id), role, content],
        );
        conn.last_insert_rowid()
    }

    pub fn count_chat_messages(&self, user_id: &str) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM chat_history WHERE user_id = ?",
            params![uid(user_id)],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn get_chat_history(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, role, content FROM chat_history WHERE user_id = ? ORDER BY id DESC LIMIT 40")
            .unwrap();
        let rows: Vec<serde_json::Value> = stmt
            .query_map(params![uid(user_id)], |r| {
                Ok(json!({
                    "messageId": r.get::<_, i64>(0)?,
                    "role": r.get::<_, String>(1)?,
                    "content": r.get::<_, String>(2)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        rows.into_iter().rev().collect()
    }

    pub fn get_chat_history_page(
        &self,
        user_id: &str,
        before_id: i64,
        limit: i64,
    ) -> serde_json::Value {
        let size = limit.clamp(1, 200);
        let total = self.count_chat_messages(user_id);
        let conn = self.conn.lock().unwrap();
        let query = if before_id > 0 {
            "SELECT id, role, content, created_at FROM chat_history WHERE user_id = ? AND id < ? ORDER BY id DESC LIMIT ?"
        } else {
            "SELECT id, role, content, created_at FROM chat_history WHERE user_id = ? ORDER BY id DESC LIMIT ?"
        };
        let mut stmt = conn.prepare(query).unwrap();
        let map_row = |r: &rusqlite::Row| {
            Ok(json!({
                "messageId": r.get::<_, i64>(0)?,
                "role": r.get::<_, String>(1)?,
                "content": r.get::<_, String>(2)?,
                "createdAt": r.get::<_, Option<String>>(3)?,
            }))
        };
        let mut messages: Vec<serde_json::Value> = if before_id > 0 {
            stmt.query_map(params![uid(user_id), before_id, size], map_row)
        } else {
            stmt.query_map(params![uid(user_id), size], map_row)
        }
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
        messages.reverse();
        let oldest = messages
            .first()
            .and_then(|m| m["messageId"].as_i64())
            .unwrap_or(0);
        let has_more = oldest > 0
            && conn
                .query_row(
                    "SELECT 1 FROM chat_history WHERE user_id = ? AND id < ? LIMIT 1",
                    params![uid(user_id), oldest],
                    |_| Ok(()),
                )
                .is_ok();
        json!({ "messages": messages, "total": total, "hasMore": has_more })
    }

    pub fn search_chat_history(
        &self,
        user_id: &str,
        query: &str,
        limit: i64,
    ) -> serde_json::Value {
        let q = query.trim();
        if q.is_empty() {
            return json!({ "messages": [], "total": 0 });
        }
        let size = limit.clamp(1, 200);
        let pattern = format!("%{}%", q.replace('%', "\\%").replace('_', "\\_"));
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM chat_history WHERE user_id = ? AND content LIKE ? ESCAPE '\\'",
                params![uid(user_id), pattern],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mut stmt = conn
            .prepare("SELECT id, role, content, created_at FROM chat_history WHERE user_id = ? AND content LIKE ? ESCAPE '\\' ORDER BY id DESC LIMIT ?")
            .unwrap();
        let mut messages: Vec<serde_json::Value> = stmt
            .query_map(params![uid(user_id), pattern, size], |r| {
                Ok(json!({
                    "messageId": r.get::<_, i64>(0)?,
                    "role": r.get::<_, String>(1)?,
                    "content": r.get::<_, String>(2)?,
                    "createdAt": r.get::<_, Option<String>>(3)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        messages.reverse();
        json!({ "messages": messages, "total": total })
    }

    // === Rate limit (consumeRateLimit di Node) ===
    pub fn consume_rate_limit(&self, bucket_key: &str, window_ms: i64, maximum: i64) -> (bool, i64, i64) {
        let key: String = bucket_key.chars().take(240).collect();
        let now = now_ms();
        let conn = self.conn.lock().unwrap();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT hits, reset_at FROM rate_limits WHERE bucket_key = ?",
                params![key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match row {
            None => {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO rate_limits (bucket_key, hits, reset_at) VALUES (?, 1, ?)",
                    params![key, now + window_ms],
                );
                (true, maximum - 1, now + window_ms)
            }
            Some((_, reset_at)) if now >= reset_at => {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO rate_limits (bucket_key, hits, reset_at) VALUES (?, 1, ?)",
                    params![key, now + window_ms],
                );
                (true, maximum - 1, now + window_ms)
            }
            Some((hits, reset_at)) => {
                let hits = hits + 1;
                let _ = conn.execute(
                    "UPDATE rate_limits SET hits = ? WHERE bucket_key = ?",
                    params![hits, key],
                );
                (hits <= maximum, (maximum - hits).max(0), reset_at)
            }
        }
    }

    /// State emosi tersimpan (JSON serialize() dari lib/emotion.js), bila ada.
    pub fn load_emotion_state(&self, user_id: &str) -> Option<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM emotion WHERE user_id = ?",
                params![uid(user_id)],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        state.and_then(|s| serde_json::from_str(&s).ok())
    }

    /// Simpan state emosi (Fase 5: serialize() Emotion ditulis balik,
    /// sama seperti saveEmotion di memory.js).
    pub fn save_emotion(&self, user_id: &str, state: &serde_json::Value) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO emotion (user_id, state, updated_at) VALUES (?, ?, datetime('now')) \
             ON CONFLICT(user_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            params![uid(user_id), state.to_string()],
        )
        .ok();
    }

    // === Bond & milestone ===
    /// Nilai bond dari emotion.state JSON (kolom `bond`).
    pub fn load_bond(&self, user_id: &str) -> f64 {
        let conn = self.conn.lock().unwrap();
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM emotion WHERE user_id = ?",
                params![uid(user_id)],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        state
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["bond"].as_f64())
            .unwrap_or(0.0)
    }

    pub fn record_relationship_event(
        &self,
        user_id: &str,
        kind: &str,
        title: &str,
        detail: &str,
        bond_value: f64,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO relationship_events (user_id, kind, title, detail, bond_value) VALUES (?, ?, ?, ?, ?)",
            params![uid(user_id), kind, title, detail, bond_value],
        );
    }

    pub fn get_relationship_events(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT kind, title, detail, bond_value, created_at FROM relationship_events WHERE user_id = ? ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok(json!({
                "kind": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "detail": r.get::<_, Option<String>>(2)?,
                "bondValue": r.get::<_, f64>(3)?,
                "createdAt": r.get::<_, Option<String>>(4)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    // === Fakta & event (Fase 2) ===
    pub fn insert_fact(&self, user_id: &str, text: &str, vec_json: Option<&str>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO facts (user_id, text, vec) VALUES (?, ?, ?)",
            params![uid(user_id), text, vec_json],
        );
    }

    pub fn prune_facts(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM facts WHERE user_id = ? AND rowid NOT IN (SELECT rowid FROM facts WHERE user_id = ? ORDER BY created_at DESC LIMIT ?)",
            params![uid(user_id), uid(user_id), MAX_FACTS],
        );
    }

    pub fn facts_with_vec(&self, user_id: &str) -> Vec<(String, Option<String>, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT text, vec, created_at FROM facts WHERE user_id = ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn facts_for_list(&self, user_id: &str) -> Vec<(i64, String, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT rowid, text, created_at FROM facts WHERE user_id = ? ORDER BY created_at DESC") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn delete_fact(&self, user_id: &str, fact_id: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM facts WHERE user_id = ? AND rowid = ?",
            params![uid(user_id), fact_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn insert_event(&self, user_id: &str, summary: &str, mood: Option<&str>, vec_json: Option<&str>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO events (user_id, summary, mood, vec) VALUES (?, ?, ?, ?)",
            params![uid(user_id), summary, mood, vec_json],
        );
        let _ = conn.execute(
            "DELETE FROM events WHERE user_id = ? AND id NOT IN (SELECT id FROM events WHERE user_id = ? ORDER BY ts DESC LIMIT ?)",
            params![uid(user_id), uid(user_id), MAX_EVENTS],
        );
    }

    pub fn events_with_vec(&self, user_id: &str) -> Vec<(String, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT summary, vec FROM events WHERE user_id = ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn recent_events(&self, user_id: &str, limit: i64) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT summary, mood FROM events WHERE user_id = ? ORDER BY ts DESC LIMIT ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id), limit], |r| {
            Ok(json!({ "summary": r.get::<_, String>(0)?, "mood": r.get::<_, Option<String>>(1)? }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn get_profile(&self, user_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT profile_summary FROM user_profile WHERE user_id = ?",
            params![uid(user_id)],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    pub fn set_profile(&self, user_id: &str, summary: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO user_profile (user_id, profile_summary, updated_at) VALUES (?, ?, datetime('now'))",
            params![uid(user_id), summary],
        );
    }

    pub fn get_chat_summary(&self, user_id: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT summary FROM chat_summary WHERE user_id = ?",
            params![uid(user_id)],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    pub fn store_chat_summary(&self, user_id: &str, summary: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO chat_summary (user_id, summary, updated_at) VALUES (?, ?, datetime('now'))",
            params![uid(user_id), summary],
        );
    }

    pub fn oldest_chat_messages(&self, user_id: &str, limit: i64) -> Vec<(i64, String, String)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT id, role, content FROM chat_history WHERE user_id = ? ORDER BY created_at ASC LIMIT ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id), limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn delete_chat_messages(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let conn = self.conn.lock().unwrap();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM chat_history WHERE id IN ({placeholders})");
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        let _ = conn.execute(&sql, params_vec.as_slice());
    }

    // === Structured memory ===
    pub fn upsert_structured(&self, user_id: &str, category: &str, content: &str, importance: i64) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO structured_memory (user_id, category, content, importance) VALUES (?, ?, ?, ?) ON CONFLICT(user_id, category, content) DO UPDATE SET importance = MAX(importance, excluded.importance), updated_at = datetime('now')",
            params![uid(user_id), category, content, importance],
        );
    }

    pub fn resolve_recent_open_loops(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE structured_memory SET status = 'resolved', updated_at = datetime('now') WHERE user_id = ? AND category = 'open_loop' AND status = 'active' AND id IN (SELECT id FROM structured_memory WHERE user_id = ? AND category = 'open_loop' AND status = 'active' ORDER BY updated_at DESC LIMIT 2)",
            params![uid(user_id), uid(user_id)],
        );
    }

    pub fn prune_structured(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM structured_memory WHERE user_id = ? AND id NOT IN (SELECT id FROM structured_memory WHERE user_id = ? ORDER BY importance DESC, updated_at DESC LIMIT 40)",
            params![uid(user_id), uid(user_id)],
        );
    }

    pub fn structured_for_list(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT id, category, content, status, importance, updated_at FROM structured_memory WHERE user_id = ? ORDER BY status = 'active' DESC, importance DESC, updated_at DESC") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "category": r.get::<_, String>(1)?,
                "content": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
                "importance": r.get::<_, i64>(4)?, "createdAt": r.get::<_, Option<String>>(5)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn structured_for_recall(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT 'user' AS scopeType, '' AS scopeId, category, content, status, 0.7 AS confidence, updated_at AS at FROM structured_memory WHERE user_id = ? ORDER BY CASE status WHEN 'active' THEN 0 ELSE 1 END, importance DESC, updated_at DESC LIMIT 10") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok(json!({
                "scopeType": r.get::<_, String>(0)?, "scopeId": r.get::<_, String>(1)?,
                "category": r.get::<_, String>(2)?, "content": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?, "confidence": r.get::<_, f64>(5)?,
                "at": r.get::<_, Option<String>>(6)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn delete_structured(&self, user_id: &str, memory_id: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        let content: Option<String> = conn
            .query_row(
                "SELECT content FROM structured_memory WHERE user_id = ? AND id = ?",
                params![uid(user_id), memory_id],
                |r| r.get(0),
            )
            .ok();
        let deleted = conn
            .execute(
                "DELETE FROM structured_memory WHERE user_id = ? AND id = ?",
                params![uid(user_id), memory_id],
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if deleted {
            if let Some(content) = content {
                let _ = conn.execute(
                    "DELETE FROM structured_memory WHERE user_id = ? AND LOWER(TRIM(content)) = LOWER(TRIM(?))",
                    params![uid(user_id), content],
                );
                let _ = conn.execute(
                    "DELETE FROM agent_memories WHERE user_id = ? AND LOWER(TRIM(content)) = LOWER(TRIM(?))",
                    params![uid(user_id), content],
                );
            }
        }
        deleted
    }

    // === Agent memories + settings (memory-store.js) ===
    pub fn get_memory_settings(&self, user_id: &str) -> (bool, Option<String>) {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT auto_capture, updated_at FROM memory_settings WHERE user_id = ?",
            params![crate::memory::store_user(user_id)],
            |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap_or((true, None))
    }

    pub fn set_memory_settings(&self, user_id: &str, auto_capture: bool) -> (bool, Option<String>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO memory_settings (user_id, auto_capture, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT(user_id) DO UPDATE SET auto_capture = excluded.auto_capture, updated_at = datetime('now')",
            params![crate::memory::store_user(user_id), auto_capture as i64],
        );
        drop(conn);
        self.get_memory_settings(user_id)
    }

    pub fn agent_memories_raw(
        &self,
        user_id: &str,
        scope_type: &str,
        scope_id: &str,
        status: &str,
    ) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut clauses = vec!["user_id = ?".to_string()];
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(crate::memory::store_user(user_id))];
        const SCOPES: [&str; 4] = ["user", "project", "task", "lesson"];
        const STATUSES: [&str; 3] = ["active", "resolved", "archived"];
        if !scope_type.is_empty() && SCOPES.contains(&scope_type) {
            clauses.push("scope_type = ?".to_string());
            args.push(Box::new(scope_type.to_string()));
        }
        if !scope_id.is_empty() {
            clauses.push("scope_id = ?".to_string());
            args.push(Box::new(scope_id.chars().take(120).collect::<String>()));
        }
        if !status.is_empty() && STATUSES.contains(&status) {
            clauses.push("status = ?".to_string());
            args.push(Box::new(status.to_string()));
        }
        let sql = format!(
            "SELECT id, scope_type, scope_id, category, content, importance, confidence, source_message_id, evidence_json, status, expires_at, created_at, updated_at FROM agent_memories WHERE {} ORDER BY status = 'active' DESC, importance DESC, updated_at DESC LIMIT 200",
            clauses.join(" AND ")
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let params_ref: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
        stmt.query_map(params_ref.as_slice(), |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "scopeType": r.get::<_, String>(1)?,
                "scopeId": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                "content": r.get::<_, String>(4)?, "importance": r.get::<_, i64>(5)?,
                "confidence": r.get::<_, f64>(6)?, "sourceMessageId": r.get::<_, Option<i64>>(7)?,
                "evidence_json": r.get::<_, String>(8)?, "status": r.get::<_, String>(9)?,
                "expiresAt": r.get::<_, Option<String>>(10)?, "createdAt": r.get::<_, Option<String>>(11)?,
                "updatedAt": r.get::<_, Option<String>>(12)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn agent_contents(&self, user_id: &str) -> Vec<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT content FROM agent_memories WHERE user_id = ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![crate::memory::store_user(user_id)], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_agent_memory(
        &self,
        user_id: &str,
        scope_type: &str,
        scope_id: &str,
        category: &str,
        content: &str,
        importance: i64,
        confidence: f64,
        source_message_id: Option<i64>,
        evidence_json: &str,
        expires_at: Option<&str>,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO agent_memories (user_id, scope_type, scope_id, category, content, importance, confidence, source_message_id, evidence_json, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(user_id, scope_type, scope_id, category, content) DO UPDATE SET importance = MAX(importance, excluded.importance), confidence = MAX(confidence, excluded.confidence), evidence_json = excluded.evidence_json, updated_at = datetime('now')",
            params![
                crate::memory::store_user(user_id),
                scope_type,
                scope_id.chars().take(120).collect::<String>(),
                category,
                content,
                importance.clamp(1, 5),
                confidence.clamp(0.0, 1.0),
                source_message_id,
                evidence_json,
                expires_at,
            ],
        );
    }

    pub fn get_agent_memory(&self, user_id: &str, memory_id: i64) -> Option<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, content, status, importance FROM agent_memories WHERE id = ? AND user_id = ?",
            params![memory_id, crate::memory::store_user(user_id)],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "content": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?, "importance": r.get::<_, i64>(3)?,
                }))
            },
        )
        .ok()
    }

    pub fn update_agent_memory_row(
        &self,
        user_id: &str,
        memory_id: i64,
        content: &str,
        status: &str,
        importance: i64,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE agent_memories SET content = ?, status = ?, importance = ?, updated_at = datetime('now') WHERE id = ? AND user_id = ?",
            params![content, status, importance, memory_id, crate::memory::store_user(user_id)],
        );
    }

    pub fn delete_agent_memory(&self, user_id: &str, memory_id: i64) -> bool {
        let conn = self.conn.lock().unwrap();
        let u = crate::memory::store_user(user_id);
        let content: Option<String> = conn
            .query_row(
                "SELECT content FROM agent_memories WHERE id = ? AND user_id = ?",
                params![memory_id, u],
                |r| r.get(0),
            )
            .ok();
        let deleted = conn
            .execute(
                "DELETE FROM agent_memories WHERE id = ? AND user_id = ?",
                params![memory_id, u],
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if deleted {
            if let Some(content) = content {
                let _ = conn.execute(
                    "DELETE FROM structured_memory WHERE user_id = ? AND LOWER(TRIM(content)) = LOWER(TRIM(?))",
                    params![crate::memory::uid(user_id), content],
                );
                let _ = conn.execute(
                    "DELETE FROM agent_memories WHERE user_id = ? AND LOWER(TRIM(content)) = LOWER(TRIM(?))",
                    params![u, content],
                );
            }
        }
        deleted
    }

    pub fn scoped_memories_for_recall(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT scope_type, scope_id, category, content, status, confidence, updated_at FROM agent_memories WHERE user_id = ? AND status = 'active' AND (expires_at IS NULL OR expires_at > datetime('now')) ORDER BY importance DESC, confidence DESC, updated_at DESC LIMIT 12") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![crate::memory::store_user(user_id)], |r| {
            Ok(json!({
                "scopeType": r.get::<_, String>(0)?, "scopeId": r.get::<_, String>(1)?,
                "category": r.get::<_, String>(2)?, "content": r.get::<_, String>(3)?,
                "status": r.get::<_, String>(4)?, "confidence": r.get::<_, f64>(5)?,
                "at": r.get::<_, Option<String>>(6)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn feedback_ratings(&self, user_id: &str) -> Vec<i64> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT rating FROM response_feedback WHERE user_id = ? ORDER BY created_at DESC, id DESC LIMIT 12") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| r.get::<_, i64>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    pub fn save_response_feedback(&self, user_id: &str, message_id: Option<i64>, rating: i64, reason: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO response_feedback (user_id, message_id, rating, reason) VALUES (?, ?, ?, ?)",
            params![uid(user_id), message_id, if rating > 0 { 1 } else { -1 }, reason.chars().take(300).collect::<String>()],
        );
    }

    // === Bookmark komik (memory.js, Fase 5) ===
    pub fn get_comic_bookmarks(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT title, url, format, genre, chapter, score, image, created_at FROM comic_bookmarks WHERE user_id = ? ORDER BY created_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id)], |r| {
            Ok(json!({
                "title": r.get::<_, String>(0)?,
                "url": r.get::<_, String>(1)?,
                "format": r.get::<_, Option<String>>(2)?,
                "type": r.get::<_, Option<String>>(3)?,
                "chapter": r.get::<_, Option<String>>(4)?,
                "score": r.get::<_, Option<String>>(5)?,
                "image": r.get::<_, Option<String>>(6)?,
                "createdAt": r.get::<_, Option<String>>(7)?,
            }))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn save_comic_bookmark(
        &self,
        user_id: &str,
        comic: &serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let text = |v: &serde_json::Value| -> String {
            match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            }
        };
        let title = comic["title"].as_str().unwrap_or("").trim().to_string();
        let url = comic["url"].as_str().unwrap_or("").trim().to_string();
        let format = {
            let f = text(&comic["format"]);
            if f.is_empty() { "KOMIK".to_string() } else { f }
        };
        let genre = {
            let t = text(&comic["type"]);
            if !t.is_empty() { t } else { text(&comic["genre"]) }
        };
        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO comic_bookmarks (user_id, title, url, format, genre, chapter, score, image, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, datetime('now')) \
                 ON CONFLICT(user_id, url) DO UPDATE SET title = excluded.title, format = excluded.format, \
                 genre = excluded.genre, chapter = excluded.chapter, score = excluded.score, \
                 image = excluded.image, created_at = datetime('now')",
                params![
                    uid(user_id),
                    title,
                    url,
                    format,
                    genre,
                    text(&comic["chapter"]),
                    text(&comic["score"]),
                    text(&comic["image"]),
                ],
            )
            .ok();
            // Node juga mencatat preferensi terstruktur untuk bookmark.
            let format_label = {
                let f = text(&comic["format"]);
                if f.is_empty() { "Komik".to_string() } else { f }
            };
            conn.execute(
                "INSERT INTO structured_memory (user_id, category, content, importance) \
                 VALUES (?, 'preference', ?, 3) \
                 ON CONFLICT(user_id, category, content) DO UPDATE SET importance = 3, updated_at = datetime('now')",
                params![
                    uid(user_id),
                    format!("User menyimpan komik \"{title}\" ({format_label}) ke daftar bacaan"),
                ],
            )
            .ok();
        }
        self.get_comic_bookmarks(user_id)
    }

    pub fn delete_comic_bookmark(
        &self,
        user_id: &str,
        comic_url: &str,
    ) -> Vec<serde_json::Value> {
        {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "DELETE FROM comic_bookmarks WHERE user_id = ? AND url = ?",
                params![uid(user_id), comic_url.trim()],
            )
            .ok();
        }
        self.get_comic_bookmarks(user_id)
    }

    // === IP block (lib/ip-block.js, Fase 5) ===
    pub fn ip_block_until(&self, ip: &str) -> Option<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT blocked_until FROM ip_blocks WHERE ip = ?",
            params![ip],
            |r| r.get::<_, i64>(0),
        )
        .ok()
    }

    pub fn ip_block_delete(&self, ip: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM ip_blocks WHERE ip = ?", params![ip])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn ip_block_upsert(&self, ip: &str, blocked_until: i64, reason: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO ip_blocks (ip, blocked_until, reason) VALUES (?, ?, ?)",
            params![ip, blocked_until, reason],
        )
        .ok();
    }

    pub fn ip_blocks_list(&self) -> Vec<serde_json::Value> {
        let now = crate::emotion::now_ms();
        {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM ip_blocks WHERE blocked_until <= ?", params![now])
                .ok();
        }
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT ip, blocked_until, reason, created_at FROM ip_blocks ORDER BY blocked_until DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| {
            Ok(json!({
                "ip": r.get::<_, String>(0)?,
                "blockedUntil": r.get::<_, i64>(1)?,
                "reason": r.get::<_, Option<String>>(2)?,
                "createdAt": r.get::<_, Option<String>>(3)?,
            }))
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    /// regenerateAccessCode (memory.js): kunci baru disimpan sebagai
    /// hash + plaintext (agar bisa ditemukan lagi via lookup-key admin).
    pub fn regenerate_access_code(&self, user_id: &str) -> Option<serde_json::Value> {
        let id = uid(user_id);
        let conn = self.conn.lock().unwrap();
        let username: Option<String> = conn
            .query_row("SELECT username FROM users WHERE user_id = ?", params![id], |r| {
                r.get(0)
            })
            .ok()
            .flatten();
        let username = username?;
        let access_code = self.generate_access_code(&conn);
        let hash = self.hash_access_code(&access_code);
        conn.execute(
            "UPDATE users SET access_code = ?, access_code_hash = ? WHERE user_id = ?",
            params![access_code, hash, id],
        )
        .ok();
        Some(json!({ "userId": id, "username": username, "accessCode": access_code }))
    }

    /// deleteUserData: dinamis seperti Node — semua tabel berkolom user_id.
    pub fn delete_user_data(&self, user_id: &str) {
        let u = uid(user_id);
        let conn = self.conn.lock().unwrap();
        let tables: Vec<String> = {
            let mut stmt = match conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'") {
                Ok(s) => s,
                Err(_) => return,
            };
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        let mut targets: Vec<String> = Vec::new();
        for name in tables {
            if name == "users" || name.starts_with("sqlite_") {
                continue;
            }
            let has_user_id = {
                let mut stmt = match conn.prepare(&format!("PRAGMA table_info({name})")) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let cols: Vec<String> = stmt
                    .query_map([], |r| r.get::<_, String>(1))
                    .unwrap()
                    .filter_map(|r| r.ok())
                    .collect();
                cols.iter().any(|c| c == "user_id")
            };
            if has_user_id {
                targets.push(name);
            }
        }
        let _ = conn.execute_batch("BEGIN");
        let mut ok = true;
        for table in &targets {
            if conn
                .execute(&format!("DELETE FROM {table} WHERE user_id = ?"), params![u])
                .is_err()
            {
                ok = false;
                break;
            }
        }
        if ok {
            let _ = conn.execute("DELETE FROM users WHERE user_id = ?", params![u]);
            let _ = conn.execute_batch("COMMIT");
        } else {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }

    // === Scheduler (Fase 3) ===
    fn task_saved_row(
        &self,
        conn: &Connection,
        task_id: i64,
        user_id: &str,
    ) -> Option<serde_json::Value> {
        conn.query_row(
            "SELECT id, title, human_schedule, COALESCE(next_run_at_utc, next_run), timezone, created_at FROM scheduled_tasks WHERE id = ? AND user_id = ?",
            params![task_id, user_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                    "humanSchedule": r.get::<_, String>(2)?,
                    "nextRunAtUtc": r.get::<_, Option<String>>(3)?,
                    "timezone": r.get::<_, Option<String>>(4)?,
                    "createdAt": r.get::<_, Option<String>>(5)?,
                }))
            },
        )
        .ok()
    }

    pub fn find_task_by_idempotency(&self, user_id: &str, key: &str) -> Option<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM scheduled_tasks WHERE user_id = ? AND idempotency_key = ? AND status = 'active'",
                params![user_id, key],
                |r| r.get(0),
            )
            .ok();
        id.and_then(|task_id| self.task_saved_row(&conn, task_id, user_id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_task(
        &self,
        user_id: &str,
        title: &str,
        description: &str,
        cron_expr: &str,
        human_schedule: &str,
        run_once: bool,
        run_at: Option<&str>,
        next_run: Option<&str>,
        timezone: &str,
        idempotency_key: &str,
    ) -> Option<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO scheduled_tasks (user_id, title, description, cron_expr, human_schedule, run_once, run_at, next_run, next_run_at_utc, timezone, idempotency_key, status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active')",
            params![user_id, title, description, cron_expr, human_schedule, run_once as i64, run_at, next_run, next_run, timezone, idempotency_key],
        )
        .ok()?;
        let task_id = conn.last_insert_rowid();
        self.task_saved_row(&conn, task_id, user_id)
    }

    pub fn list_scheduled_tasks(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT id, title, description, human_schedule, cron_expr, run_once, COALESCE(next_run_at_utc, next_run) AS next_run_at_utc, last_run, status, created_at, timezone FROM scheduled_tasks WHERE user_id = ? AND status != 'cancelled' ORDER BY CASE status WHEN 'active' THEN 0 WHEN 'paused' THEN 1 WHEN 'failed' THEN 2 ELSE 3 END, COALESCE(next_run_at_utc, next_run, created_at) ASC") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![user_id], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                "description": r.get::<_, String>(2)?, "human_schedule": r.get::<_, String>(3)?,
                "cron_expr": r.get::<_, String>(4)?, "run_once": r.get::<_, i64>(5)?,
                "next_run_at_utc": r.get::<_, Option<String>>(6)?,
                "last_run": r.get::<_, Option<String>>(7)?,
                "status": r.get::<_, String>(8)?,
                "created_at": r.get::<_, Option<String>>(9)?,
                "timezone": r.get::<_, Option<String>>(10)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn agenda_deliveries(&self, user_id: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT o.id, o.task_id, o.scheduled_for, o.status, o.attempt_count, o.last_error, o.delivered_at, t.title FROM scheduled_occurrences o JOIN scheduled_tasks t ON t.id = o.task_id WHERE o.user_id = ? ORDER BY o.created_at DESC LIMIT 50") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![user_id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "task_id": r.get::<_, i64>(1)?,
                "scheduled_for": r.get::<_, String>(2)?, "status": r.get::<_, String>(3)?,
                "attempt_count": r.get::<_, i64>(4)?,
                "last_error": r.get::<_, Option<String>>(5)?,
                "delivered_at": r.get::<_, Option<String>>(6)?,
                "title": r.get::<_, String>(7)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn get_task_full(&self, task_id: i64, user_id: &str) -> Option<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, user_id, title, description, cron_expr, human_schedule, run_once, run_at, last_run, next_run, next_run_at_utc, timezone, idempotency_key, status, created_at FROM scheduled_tasks WHERE id = ? AND user_id = ?",
            params![task_id, user_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "user_id": r.get::<_, String>(1)?,
                    "title": r.get::<_, String>(2)?, "description": r.get::<_, String>(3)?,
                    "cron_expr": r.get::<_, String>(4)?, "human_schedule": r.get::<_, String>(5)?,
                    "run_once": r.get::<_, i64>(6)?, "run_at": r.get::<_, Option<String>>(7)?,
                    "last_run": r.get::<_, Option<String>>(8)?, "next_run": r.get::<_, Option<String>>(9)?,
                    "next_run_at_utc": r.get::<_, Option<String>>(10)?,
                    "timezone": r.get::<_, Option<String>>(11)?,
                    "idempotency_key": r.get::<_, Option<String>>(12)?,
                    "status": r.get::<_, String>(13)?,
                    "created_at": r.get::<_, Option<String>>(14)?,
                }))
            },
        )
        .ok()
    }

    pub fn get_task_full_by_id(&self, task_id: i64) -> Option<serde_json::Value> {
        let user: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT user_id FROM scheduled_tasks WHERE id = ?",
                params![task_id],
                |r| r.get(0),
            )
            .ok()
        };
        user.and_then(|u| self.get_task_full(task_id, &u))
    }

    pub fn update_task_status(&self, task_id: i64, user_id: &str, status: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET status = ? WHERE id = ? AND user_id = ?",
            params![status, task_id, user_id],
        );
    }

    pub fn set_task_status_by_id(&self, task_id: i64, status: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET status = ? WHERE id = ?",
            params![status, task_id],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn snooze_task(
        &self,
        task_id: i64,
        user_id: &str,
        when_iso: &str,
        cron_expr: &str,
        human_schedule: &str,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET status = 'active', run_once = 1, run_at = ?, next_run = ?, next_run_at_utc = ?, cron_expr = ?, human_schedule = ? WHERE id = ? AND user_id = ?",
            params![when_iso, when_iso, when_iso, cron_expr, human_schedule, task_id, user_id],
        );
    }

    pub fn update_task_title_desc(&self, task_id: i64, user_id: &str, title: &str, description: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET title = ?, description = ? WHERE id = ? AND user_id = ?",
            params![title, description, task_id, user_id],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reschedule_task(
        &self,
        task_id: i64,
        user_id: &str,
        cron_expr: &str,
        human_schedule: &str,
        run_once: bool,
        run_at: Option<&str>,
        next_run: Option<&str>,
        timezone: &str,
    ) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET cron_expr = ?, human_schedule = ?, run_once = ?, run_at = ?, next_run = ?, next_run_at_utc = ?, timezone = ?, status = 'active' WHERE id = ? AND user_id = ?",
            params![cron_expr, human_schedule, run_once as i64, run_at, next_run, next_run, timezone, task_id, user_id],
        );
    }

    pub fn latest_active_task(&self, user_id: &str) -> Option<serde_json::Value> {
        let id: Option<i64> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT id FROM scheduled_tasks WHERE user_id = ? AND status = 'active' ORDER BY created_at DESC, id DESC LIMIT 1",
                params![user_id],
                |r| r.get(0),
            )
            .ok()
        };
        id.and_then(|task_id| self.get_task_full(task_id, user_id))
    }

    pub fn active_tasks(&self) -> Vec<serde_json::Value> {
        let ids: Vec<i64> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = match conn.prepare("SELECT id FROM scheduled_tasks WHERE status = 'active'") {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            stmt.query_map([], |r| r.get::<_, i64>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        ids.iter().filter_map(|id| self.get_task_full_by_id(*id)).collect()
    }

    pub fn due_run_once_tasks(&self, now_iso: &str) -> Vec<serde_json::Value> {
        let ids: Vec<i64> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = match conn.prepare("SELECT id FROM scheduled_tasks WHERE status = 'active' AND run_once = 1 AND COALESCE(next_run_at_utc, run_at, next_run) <= ?") {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            };
            stmt.query_map(params![now_iso], |r| r.get::<_, i64>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        ids.iter().filter_map(|id| self.get_task_full_by_id(*id)).collect()
    }

    pub fn enqueue_occurrence(&self, occurrence_id: &str, task_id: i64, user_id: &str, scheduled_for: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO scheduled_occurrences (id, task_id, user_id, scheduled_for, status, next_attempt_at) VALUES (?, ?, ?, ?, 'pending', datetime('now'))",
            params![occurrence_id, task_id, user_id, scheduled_for],
        );
    }

    pub fn due_occurrences(&self, now_iso: &str) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT o.id, o.task_id, o.user_id, o.scheduled_for, o.attempt_count, t.title, t.description, t.run_once, t.cron_expr, t.timezone FROM scheduled_occurrences o JOIN scheduled_tasks t ON t.id = o.task_id WHERE o.status IN ('pending','retrying') AND COALESCE(o.next_attempt_at, o.scheduled_for) <= ? ORDER BY o.scheduled_for ASC LIMIT 20") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![now_iso], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "task_id": r.get::<_, i64>(1)?,
                "user_id": r.get::<_, String>(2)?, "scheduled_for": r.get::<_, String>(3)?,
                "attempt_count": r.get::<_, i64>(4)?, "title": r.get::<_, String>(5)?,
                "description": r.get::<_, String>(6)?, "run_once": r.get::<_, i64>(7)?,
                "cron_expr": r.get::<_, String>(8)?,
                "timezone": r.get::<_, Option<String>>(9)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn claim_occurrence(&self, occurrence_id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE scheduled_occurrences SET status = 'delivering', claimed_at = datetime('now'), attempt_count = attempt_count + 1 WHERE id = ? AND status IN ('pending','retrying')",
            params![occurrence_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn mark_occurrence_delivered(&self, occurrence_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_occurrences SET status = 'delivered', delivered_at = datetime('now'), last_error = NULL WHERE id = ?",
            params![occurrence_id],
        );
    }

    pub fn mark_occurrence_retry(&self, occurrence_id: &str, status: &str, next_attempt: &str, error: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_occurrences SET status = ?, next_attempt_at = ?, last_error = ? WHERE id = ?",
            params![status, next_attempt, error.chars().take(300).collect::<String>(), occurrence_id],
        );
    }

    pub fn set_task_last_run(&self, task_id: i64) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET last_run = datetime('now') WHERE id = ?",
            params![task_id],
        );
    }

    pub fn update_task_next_run(&self, task_id: i64, next_run: Option<&str>) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE scheduled_tasks SET next_run = ?, next_run_at_utc = ? WHERE id = ?",
            params![next_run, next_run, task_id],
        );
    }

    pub fn count_active_tasks(&self) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM scheduled_tasks WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn delete_user_tasks(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM scheduled_tasks WHERE user_id = ?", params![user_id]);
        let _ = conn.execute("DELETE FROM scheduled_occurrences WHERE user_id = ?", params![user_id]);
    }

    pub fn get_scheduled_reminder_messages(&self, user_id: &str, after_id: i64) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT id, role, content, created_at FROM chat_history WHERE user_id = ? AND id > ? AND role = 'assistant' AND (content LIKE '*[Pengingat Terjadwal:%' OR content LIKE '[YUKI_REMINDER]%') ORDER BY id ASC LIMIT 40") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![uid(user_id), after_id], |r| {
            let content = r.get::<_, String>(2)?;
            let stripped = content
                .strip_prefix("[YUKI_REMINDER]")
                .map(|s| s.trim_start().to_string())
                .unwrap_or(content);
            Ok(json!({
                "messageId": r.get::<_, i64>(0)?, "role": r.get::<_, String>(1)?,
                "content": stripped, "createdAt": r.get::<_, Option<String>>(3)?,
            }))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    // === Push subscriptions (Fase 3) ===
    pub fn subscription_owner(&self, endpoint: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT user_id FROM push_subscriptions WHERE endpoint = ?",
            params![endpoint],
            |r| r.get(0),
        )
        .ok()
    }

    pub fn save_subscription(&self, user_id: &str, endpoint: &str, subscription_json: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO push_subscriptions (endpoint, user_id, subscription_json, failure_count) VALUES (?, ?, ?, 0) ON CONFLICT(endpoint) DO UPDATE SET subscription_json = excluded.subscription_json, failure_count = 0",
            params![endpoint, user_id, subscription_json],
        );
    }

    pub fn remove_subscription(&self, user_id: &str, endpoint: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM push_subscriptions WHERE user_id = ? AND endpoint = ?",
            params![user_id, endpoint],
        );
    }

    pub fn list_subscriptions(&self, user_id: &str) -> Vec<(String, String, i64)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT endpoint, subscription_json, failure_count FROM push_subscriptions WHERE user_id = ?") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![user_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
    }

    pub fn subscription_mark_success(&self, endpoint: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE push_subscriptions SET last_success_at = datetime('now'), failure_count = 0 WHERE endpoint = ?",
            params![endpoint],
        );
    }

    pub fn subscription_mark_failure(&self, endpoint: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE push_subscriptions SET failure_count = failure_count + 1 WHERE endpoint = ?",
            params![endpoint],
        );
    }

    pub fn delete_subscription_by_endpoint(&self, endpoint: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM push_subscriptions WHERE endpoint = ?",
            params![endpoint],
        );
    }

    pub fn delete_user_subscriptions(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM push_subscriptions WHERE user_id = ?",
            params![user_id],
        );
    }

    pub fn sync_bond_milestones(&self, user_id: &str, bond_value: f64) -> Vec<serde_json::Value> {
        const MILESTONES: [(f64, &str, &str); 5] = [
            (0.0, "first_meeting", "Pertama bertemu"),
            (8.0, "familiar", "Mulai terbiasa"),
            (24.0, "caring", "Diam-diam peduli"),
            (50.0, "close", "Mulai terbuka"),
            (78.0, "partner", "Menjadi pasangan"),
        ];
        for (minimum, kind, title) in MILESTONES {
            if bond_value >= minimum {
                self.record_relationship_event(user_id, kind, title, "", bond_value);
            }
        }
        self.get_relationship_events(user_id)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut out = String::new();
    while n > 0 {
        let digit = (n % 36) as u32;
        out.push(char::from_digit(digit, 36).unwrap());
        n /= 36;
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Db {
        Db::open_memory("pepper-test".to_string()).unwrap()
    }

    #[test]
    fn register_and_login_code_roundtrip() {
        let db = test_db();
        let (user_id, code) = db.register_user("Bagus").unwrap();
        assert!(db.user_exists(&user_id));
        let (found, name) = db.get_user_by_access_code(&code).unwrap();
        assert_eq!(found, user_id);
        assert_eq!(name.as_deref(), Some("Bagus"));
        assert!(db.get_user_by_access_code("YUKI-SALA-HCOD").is_none());
    }

    #[test]
    fn session_state_and_history() {
        let db = test_db();
        let (user_id, _) = db.register_user("A").unwrap();
        assert_eq!(db.get_session_state(&user_id), Some((1, None)));
        db.set_session_jti(&user_id, "jti-1");
        assert_eq!(
            db.get_session_state(&user_id),
            Some((1, Some("jti-1".to_string())))
        );
        db.bump_session_version(&user_id);
        assert_eq!(db.get_session_state(&user_id), Some((2, None)));
        db.save_chat_message(&user_id, "user", "halo");
        db.save_chat_message(&user_id, "assistant", "hai");
        let history = db.get_chat_history(&user_id);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["content"], "halo");
        assert_eq!(db.count_chat_messages(&user_id), 2);
    }

    #[test]
    fn rate_limit_blocks_after_max() {
        let db = test_db();
        for _ in 0..3 {
            assert!(db.consume_rate_limit("u:/api/chat", 60_000, 3).0);
        }
        assert!(!db.consume_rate_limit("u:/api/chat", 60_000, 3).0);
    }
}
