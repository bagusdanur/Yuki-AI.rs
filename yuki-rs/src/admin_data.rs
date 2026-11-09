//! Port `lib/admin-stats.js` + `lib/admin-browser.js` + ringkasan admin
//! dari `lib/memory.js` (Fase 5): statistik, penjelajah tabel DB,
//! penjelajah per-user, pencari kunci ingatan, dan browser workspace.
//! Semua query ter-parameter; nama tabel/kolom hanya dari allowlist
//! skema nyata (PRAGMA), sama seperti Node.

use crate::db::Db;
use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const APP_DATA_LABELS: &[(&str, &str)] = &[
    ("chat_history", "Riwayat chat"),
    ("users", "Pengguna"),
    ("facts", "Fakta ingatan"),
    ("structured_memory", "Memori terstruktur"),
    ("agent_memories", "Memori agent"),
    ("events", "Peristiwa"),
    ("relationship_events", "Peristiwa hubungan"),
    ("emotion", "Emosi / bond"),
    ("agent_runs", "Agent runs"),
    ("agent_run_steps", "Langkah agent"),
    ("scheduled_tasks", "Jadwal tersimpan"),
    ("scheduled_occurrences", "Riwayat jadwal"),
    ("comic_bookmarks", "Bookmark komik"),
    ("user_artifacts", "Artifact user"),
    ("response_feedback", "Feedback respons"),
    ("self_improvements", "Self-improvement"),
    ("chat_summary", "Ringkasan chat"),
    ("user_profile", "Profil user"),
    ("user_notes", "Catatan user"),
    ("push_subscriptions", "Langganan push"),
];

pub fn label_of(name: &str) -> String {
    APP_DATA_LABELS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| name.to_string())
}

fn sql_to_json(v: SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        SqlValue::Text(s) => json!(s),
        SqlValue::Blob(b) => json!(String::from_utf8_lossy(&b).to_string()),
    }
}

fn scalar(db: &Db, sql: &str) -> i64 {
    let conn = db.conn.lock().unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0)
}

fn all_bonds(db: &Db) -> Vec<f64> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare("SELECT state FROM emotion") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    rows.iter()
        .map(|s| {
            serde_json::from_str::<Value>(s)
                .ok()
                .and_then(|v| v["bond"].as_f64())
                .unwrap_or(0.0)
        })
        .collect()
}

// === memory.js: getAdminStats + getAppDataSummary ===

pub fn app_data_summary(db: &Db, file: &str) -> Value {
    let conn = db.conn.lock().unwrap();
    let names: Vec<String> = {
        let mut stmt = match conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name") {
            Ok(s) => s,
            Err(_) => return json!({ "file": file, "sizeBytes": 0, "tables": [] }),
        };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    };
    let mut tables: Vec<Value> = Vec::new();
    for name in names {
        if name.starts_with("sqlite_") || name.contains("_fts") || name.contains("_vec") {
            continue;
        }
        let count: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| r.get(0))
            .unwrap_or(0);
        tables.push(json!({ "name": name, "label": label_of(&name), "count": count }));
    }
    drop(conn);
    tables.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    let size = std::fs::metadata(file).map(|m| m.len() as i64).unwrap_or(0);
    json!({ "file": file, "sizeBytes": size, "tables": tables })
}

pub fn get_admin_stats(db: &Db, file: &str) -> Value {
    let users = scalar(db, "SELECT COUNT(*) FROM users");
    let messages = scalar(db, "SELECT COUNT(*) FROM chat_history");
    let feedback_positive = scalar(db, "SELECT COUNT(*) FROM response_feedback WHERE rating = 1");
    let feedback_negative = scalar(db, "SELECT COUNT(*) FROM response_feedback WHERE rating = -1");
    let active_today = scalar(
        db,
        "SELECT COUNT(DISTINCT user_id) FROM chat_history WHERE created_at >= datetime('now', '-1 day')",
    );
    let recent: Vec<Value> = {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT u.username, COUNT(h.id), MAX(h.created_at) FROM users u \
                 LEFT JOIN chat_history h ON h.user_id = u.user_id \
                 GROUP BY u.user_id ORDER BY MAX(h.created_at) DESC LIMIT 20",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok(json!({
                "username": r.get::<_, Option<String>>(0)?,
                "messages": r.get::<_, i64>(1)?,
                "lastActive": r.get::<_, Option<String>>(2)?,
            }))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    };
    let bonds = all_bonds(db);
    let average_bond = if bonds.is_empty() {
        0.0
    } else {
        bonds.iter().sum::<f64>() / bonds.len() as f64
    };
    json!({
        "users": users,
        "messages": messages,
        "activeToday": active_today,
        "feedbackPositive": feedback_positive,
        "feedbackNegative": feedback_negative,
        "averageBond": average_bond,
        "recent": recent,
        "appData": app_data_summary(db, file),
    })
}

// === admin-stats.js ===

pub fn get_database_stats(db: &Db) -> Value {
    let c = |sql: &str| scalar(db, sql);
    let users = c("SELECT COUNT(*) FROM users");
    let messages = c("SELECT COUNT(*) FROM chat_history");
    let active_today = c("SELECT COUNT(DISTINCT user_id) FROM chat_history WHERE created_at >= datetime('now', '-1 day')");
    let active_week = c("SELECT COUNT(DISTINCT user_id) FROM chat_history WHERE created_at >= datetime('now', '-7 day')");
    let user_messages = c("SELECT COUNT(*) FROM chat_history WHERE role = 'user'");
    let assistant_messages = c("SELECT COUNT(*) FROM chat_history WHERE role = 'assistant'");
    let agent_messages = c("SELECT COUNT(*) FROM chat_history WHERE mode = 'agent'");
    let facts = c("SELECT COUNT(*) FROM facts");
    let structured = c("SELECT COUNT(*) FROM structured_memory");
    let agent_memories = c("SELECT COUNT(*) FROM agent_memories");
    let events = c("SELECT COUNT(*) FROM events");
    let relationship_events = c("SELECT COUNT(*) FROM relationship_events");
    let feedback_positive = c("SELECT COUNT(*) FROM response_feedback WHERE rating = 1");
    let feedback_negative = c("SELECT COUNT(*) FROM response_feedback WHERE rating = -1");
    let scheduled_tasks = c("SELECT COUNT(*) FROM scheduled_tasks");
    let scheduled_done = c("SELECT COUNT(*) FROM scheduled_occurrences WHERE status = 'done'");
    let bookmarks = c("SELECT COUNT(*) FROM comic_bookmarks");
    let artifacts = c("SELECT COUNT(*) FROM user_artifacts");
    let agent_runs = c("SELECT COUNT(*) FROM agent_runs");
    let agent_steps = c("SELECT COUNT(*) FROM agent_run_steps");
    let self_improvements = c("SELECT COUNT(*) FROM self_improvements");
    let summaries = c("SELECT COUNT(*) FROM chat_summary");
    let new_today = c("SELECT COUNT(*) FROM users WHERE created_at >= datetime('now', '-1 day')");
    let new_week = c("SELECT COUNT(*) FROM users WHERE created_at >= datetime('now', '-7 day')");
    let bonds = all_bonds(db);
    let average_bond = if bonds.is_empty() { 0.0 } else { bonds.iter().sum::<f64>() / bonds.len() as f64 };
    json!({
        "users": users, "messages": messages, "activeToday": active_today, "activeWeek": active_week,
        "userMessages": user_messages, "assistantMessages": assistant_messages, "agentMessages": agent_messages,
        "facts": facts, "structuredMemory": structured, "agentMemories": agent_memories, "events": events,
        "relationshipEvents": relationship_events,
        "feedbackPositive": feedback_positive, "feedbackNegative": feedback_negative,
        "feedbackTotal": feedback_positive + feedback_negative,
        "scheduledTasks": scheduled_tasks, "scheduledDone": scheduled_done, "bookmarks": bookmarks,
        "artifacts": artifacts, "agentRuns": agent_runs, "agentSteps": agent_steps,
        "selfImprovements": self_improvements, "summaries": summaries,
        "newUsersToday": new_today, "newUsersWeek": new_week,
        "averageBond": average_bond,
        "memoryTotal": facts + structured + agent_memories,
    })
}

pub fn get_chat_trend(db: &Db, days: i64) -> Vec<Value> {
    let conn = db.conn.lock().unwrap();
    let mut map: std::collections::HashMap<String, (i64, i64)> = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT date(created_at), COUNT(*), COUNT(DISTINCT user_id) FROM chat_history \
         WHERE created_at >= datetime('now', ?1) GROUP BY date(created_at) ORDER BY 1 ASC",
    ) {
        if let Ok(rows) = stmt.query_map(params![format!("-{days} day")], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) {
            for (d, m, u) in rows.filter_map(|r| r.ok()) {
                map.insert(d, (m, u));
            }
        }
    }
    drop(conn);
    let mut out = Vec::new();
    let today = chrono::Utc::now().date_naive();
    for i in (0..days).rev() {
        let d = today - chrono::Duration::days(i);
        let key = d.format("%Y-%m-%d").to_string();
        let (m, u) = map.get(&key).cloned().unwrap_or((0, 0));
        out.push(json!({ "date": key, "messages": m, "users": u }));
    }
    out
}

pub fn get_hourly_activity(db: &Db) -> Vec<Value> {
    let conn = db.conn.lock().unwrap();
    let mut map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT strftime('%H', created_at), COUNT(*) FROM chat_history \
         WHERE created_at >= datetime('now', '-1 day') GROUP BY 1 ORDER BY 1 ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        }) {
            for (h, m) in rows.filter_map(|r| r.ok()) {
                map.insert(h, m);
            }
        }
    }
    drop(conn);
    (0..24)
        .map(|h| {
            let key = format!("{h:02}");
            json!({ "hour": format!("{key}:00"), "messages": map.get(&key).cloned().unwrap_or(0) })
        })
        .collect()
}

pub fn get_top_users(db: &Db, limit: i64) -> Vec<Value> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(
        "SELECT u.username, u.user_id, COUNT(h.id), MAX(h.created_at) FROM users u \
         LEFT JOIN chat_history h ON h.user_id = u.user_id \
         GROUP BY u.user_id ORDER BY 3 DESC, 4 DESC LIMIT ?",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![limit], |r| {
        Ok(json!({
            "username": r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "tanpa nama".to_string()),
            "userId": r.get::<_, String>(1)?,
            "messages": r.get::<_, i64>(2)?,
            "lastActive": r.get::<_, Option<String>>(3)?,
        }))
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn get_recent_users(db: &Db, limit: i64) -> Vec<Value> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(
        "SELECT username, user_id, created_at FROM users ORDER BY created_at DESC LIMIT ?",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![limit], |r| {
        Ok(json!({
            "username": r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "tanpa nama".to_string()),
            "userId": r.get::<_, String>(1)?,
            "createdAt": r.get::<_, Option<String>>(2)?,
        }))
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn get_emotion_distribution(db: &Db) -> Vec<Value> {
    let bonds = all_bonds(db);
    let mut buckets = [0i64; 5];
    for bond in bonds {
        let idx = if bond < 5.0 {
            0
        } else if bond < 15.0 {
            1
        } else if bond < 30.0 {
            2
        } else if bond < 60.0 {
            3
        } else {
            4
        };
        buckets[idx] += 1;
    }
    ["asing", "kenalan", "dekat", "sahabat", "istimewa"]
        .iter()
        .enumerate()
        .map(|(i, label)| json!({ "label": label, "value": buckets[i] }))
        .collect()
}

pub fn get_storage_info(db: &Db) -> Value {
    let conn = db.conn.lock().unwrap();
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap_or(0);
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap_or(0);
    drop(conn);
    let tables: Vec<Value> = ["chat_history", "facts", "structured_memory", "agent_memories", "events"]
        .iter()
        .map(|name| {
            json!({ "name": name, "rows": scalar(db, &format!("SELECT COUNT(*) FROM {name}")) })
        })
        .collect();
    json!({ "bytes": page_count * page_size, "tables": tables })
}

// === admin-browser.js: penjelajah tabel ===

const REDACT_COLUMNS: [&str; 10] = [
    "access_code", "accesscode", "session_token", "sessiontoken", "api_key", "apikey", "password",
    "token", "secret", "hash",
];

fn is_redacted(col: &str) -> bool {
    REDACT_COLUMNS.contains(&col.to_lowercase().as_str())
}

fn is_hidden_table(name: &str) -> bool {
    name.starts_with("sqlite_") || name.ends_with("_fts") || name.ends_with("_vec")
}

fn table_names(db: &Db) -> Vec<String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn column_names(db: &Db, table: &str) -> Vec<String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(&format!("PRAGMA table_info(\"{table}\")")) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| r.get::<_, String>(1))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

fn primary_key(db: &Db, table: &str) -> Option<String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")).ok()?;
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    rows.into_iter().find(|(_, pk)| *pk > 0).map(|(name, _)| name)
}

struct TableMeta {
    name: String,
    count: i64,
    columns: Vec<String>,
    primary_key: Option<String>,
}

fn list_table_metas(db: &Db) -> Vec<TableMeta> {
    let mut out = Vec::new();
    for name in table_names(db) {
        if is_hidden_table(&name) {
            continue;
        }
        let columns = column_names(db, &name);
        let count = scalar(db, &format!("SELECT COUNT(*) FROM \"{name}\""));
        out.push(TableMeta {
            primary_key: primary_key(db, &name),
            name,
            count,
            columns,
        });
    }
    out.sort_by(|a, b| b.count.cmp(&a.count));
    out
}

pub fn list_tables(db: &Db) -> Vec<Value> {
    list_table_metas(db)
        .iter()
        .map(|m| {
            json!({
                "name": m.name,
                "label": label_of(&m.name),
                "count": m.count,
                "columns": m.columns.len(),
                "primaryKey": m.primary_key,
                "searchable": m.columns.iter().filter(|c| !is_redacted(c)).take(8).cloned().collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn safe_rows(
    db: &Db,
    sql: &str,
    params: Vec<SqlValue>,
    columns: &[String],
) -> Vec<Value> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        let mut obj = Map::new();
        for (i, col) in columns.iter().enumerate() {
            let value: SqlValue = row.get(i)?;
            if is_redacted(col) {
                // Node: nilai falsy (null/0/'') tampil apa adanya.
                let truthy = match &value {
                    SqlValue::Null => false,
                    SqlValue::Integer(n) => *n != 0,
                    SqlValue::Real(f) => *f != 0.0,
                    SqlValue::Text(s) => !s.is_empty(),
                    SqlValue::Blob(b) => !b.is_empty(),
                };
                obj.insert(
                    col.clone(),
                    if truthy { json!("••••••") } else { sql_to_json(value) },
                );
                continue;
            }
            let mut v = sql_to_json(value);
            if let Value::String(s) = &v {
                if s.chars().count() > 500 {
                    v = json!(format!("{}…", s.chars().take(500).collect::<String>()));
                }
            }
            obj.insert(col.clone(), v);
        }
        Ok(Value::Object(obj))
    });
    match rows {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

pub fn read_table(
    db: &Db,
    table: &str,
    page: i64,
    page_size: i64,
    search: &str,
    order_by: Option<&str>,
    order: &str,
) -> Result<Value, String> {
    let metas = list_table_metas(db);
    let meta = metas
        .iter()
        .find(|m| m.name == table)
        .ok_or_else(|| "Tabel tidak dikenal.".to_string())?;
    let size = page_size.clamp(1, 200);
    let current = page.max(1);
    let offset = (current - 1) * size;
    let columns = column_names(db, table);
    let term: String = search.trim().chars().take(120).collect();
    let mut where_parts: Vec<String> = Vec::new();
    let mut params: Vec<SqlValue> = Vec::new();
    if !term.is_empty() {
        for col in columns.iter().filter(|c| !is_redacted(c)) {
            where_parts.push(format!("CAST(\"{col}\" AS TEXT) LIKE ?"));
            params.push(SqlValue::Text(format!("%{term}%")));
        }
    }
    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!("WHERE ({})", where_parts.join(" OR "))
    };
    let sort_column = order_by
        .filter(|c| columns.iter().any(|x| x == c))
        .map(|s| s.to_string())
        .or_else(|| meta.primary_key.clone())
        .or_else(|| columns.first().cloned())
        .unwrap_or_default();
    let sort_dir = if order.to_lowercase() == "asc" { "ASC" } else { "DESC" };
    let total = {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM \"{table}\" {where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    let mut row_params = params.clone();
    row_params.push(SqlValue::Integer(size));
    row_params.push(SqlValue::Integer(offset));
    let rows = safe_rows(
        db,
        &format!(
            "SELECT * FROM \"{table}\" {where_sql} ORDER BY \"{sort_column}\" {sort_dir} LIMIT ? OFFSET ?"
        ),
        row_params,
        &columns,
    );
    let page_count = ((total as f64) / (size as f64)).ceil().max(1.0) as i64;
    Ok(json!({
        "table": table,
        "label": label_of(table),
        "page": current.min(page_count),
        "pageSize": size,
        "pageCount": page_count,
        "total": total,
        "orderBy": sort_column,
        "order": sort_dir.to_lowercase(),
        "columns": columns.iter().filter(|c| !is_redacted(c)).cloned().collect::<Vec<_>>(),
        "redactedColumns": columns.iter().filter(|c| is_redacted(c)).cloned().collect::<Vec<_>>(),
        "rows": rows,
    }))
}

// === admin-browser.js: per-user ===

const USER_FILTERED_TABLES: [&str; 21] = [
    "facts", "events", "emotion", "user_profile", "users", "chat_history", "chat_summary",
    "structured_memory", "relationship_events", "response_feedback", "comic_bookmarks",
    "user_notes", "scheduled_tasks", "user_artifacts", "self_improvements", "agent_workflows",
    "agent_runs", "scheduled_occurrences", "push_subscriptions", "agent_memories",
    "memory_settings",
];

fn directory_size(dir: &Path) -> (i64, i64) {
    let mut files = 0i64;
    let mut bytes = 0i64;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if ["node_modules", ".git", "__pycache__", ".venv", "venv"].contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                let (f, b) = directory_size(&path);
                files += f;
                bytes += b;
            } else if let Ok(meta) = entry.metadata() {
                files += 1;
                bytes += meta.len() as i64;
            }
        }
    }
    (files, bytes)
}

pub fn workspace_root() -> PathBuf {
    let raw = std::env::var("AGENT_WORKSPACE_DIR").unwrap_or_else(|_| "./agent-workspace".to_string());
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

pub fn list_users(db: &Db, search: &str, page: i64, page_size: i64) -> Value {
    let size = if page_size == 0 { 24 } else { page_size }.clamp(1, 200);
    let current = page.max(1);
    let offset = (current - 1) * size;
    let term: String = search.trim().chars().take(80).collect();
    let (where_sql, like_params): (String, Vec<SqlValue>) = if term.is_empty() {
        (String::new(), Vec::new())
    } else {
        (
            "WHERE u.username LIKE ? OR u.user_id LIKE ?".to_string(),
            vec![
                SqlValue::Text(format!("%{term}%")),
                SqlValue::Text(format!("%{term}%")),
            ],
        )
    };
    let total = {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM users u {where_sql}"),
            rusqlite::params_from_iter(like_params.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    let rows: Vec<Value> = {
        let conn = db.conn.lock().unwrap();
        let mut stmt = match conn.prepare(&format!(
            "SELECT u.user_id, u.username, u.created_at, \
             (SELECT COUNT(*) FROM chat_history c WHERE c.user_id = u.user_id), \
             (SELECT MAX(c.created_at) FROM chat_history c WHERE c.user_id = u.user_id), \
             (SELECT COUNT(*) FROM facts f WHERE f.user_id = u.user_id), \
             (SELECT COUNT(*) FROM structured_memory m WHERE m.user_id = u.user_id), \
             (SELECT COUNT(*) FROM scheduled_tasks t WHERE t.user_id = u.user_id), \
             (SELECT COUNT(*) FROM comic_bookmarks b WHERE b.user_id = u.user_id), \
             (SELECT COUNT(*) FROM user_artifacts a WHERE a.user_id = u.user_id) \
             FROM users u {where_sql} ORDER BY 4 DESC, u.username COLLATE NOCASE ASC LIMIT ? OFFSET ?"
        )) {
            Ok(s) => s,
            Err(_) => return json!({ "users": [], "page": current, "pageSize": size, "pageCount": 1, "total": 0 }),
        };
        let mut p = like_params.clone();
        p.push(SqlValue::Integer(size));
        p.push(SqlValue::Integer(offset));
        stmt.query_map(rusqlite::params_from_iter(p.iter()), |r| {
            Ok(json!({
                "userId": r.get::<_, String>(0)?,
                "username": r.get::<_, Option<String>>(1)?,
                "createdAt": r.get::<_, Option<String>>(2)?,
                "messages": r.get::<_, i64>(3)?,
                "lastActive": r.get::<_, Option<String>>(4)?,
                "facts": r.get::<_, i64>(5)?,
                "memories": r.get::<_, i64>(6)?,
                "schedules": r.get::<_, i64>(7)?,
                "bookmarks": r.get::<_, i64>(8)?,
                "artifacts": r.get::<_, i64>(9)?,
            }))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    };
    let ws_base = workspace_root();
    let users: Vec<Value> = rows
        .into_iter()
        .map(|mut u| {
            let dir = ws_base.join(u["userId"].as_str().unwrap_or(""));
            let (ws_files, ws_bytes) = if dir.exists() { directory_size(&dir) } else { (0, 0) };
            let total_user =
                u["messages"].as_i64().unwrap_or(0) + u["facts"].as_i64().unwrap_or(0) + u["memories"].as_i64().unwrap_or(0);
            u["wsFiles"] = json!(ws_files);
            u["wsBytes"] = json!(ws_bytes);
            u["total"] = json!(total_user);
            u
        })
        .collect();
    let page_count = ((total as f64) / (size as f64)).ceil().max(1.0) as i64;
    json!({
        "users": users,
        "page": current,
        "pageSize": size,
        "pageCount": page_count,
        "total": total,
    })
}

pub fn find_memory_keys(db: &Db, search: &str, limit: i64) -> Value {
    let term: String = search.trim().chars().take(80).collect();
    if term.is_empty() {
        return json!({ "users": [], "total": 0, "query": "" });
    }
    let size = if limit == 0 { 25 } else { limit }.clamp(1, 50);
    let escaped: String = term
        .chars()
        .flat_map(|c| if matches!(c, '\\' | '%' | '_') { vec!['\\', c] } else { vec![c] })
        .collect();
    let like = format!("%{escaped}%");
    let where_sql = "WHERE u.username LIKE ? ESCAPE '\\' OR u.user_id LIKE ? ESCAPE '\\' OR u.access_code LIKE ? ESCAPE '\\'";
    let total = {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM users u {where_sql}"),
            params![like, like, like],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    let conn = db.conn.lock().unwrap();
    let mut stmt = match conn.prepare(&format!(
        "SELECT u.user_id, u.username, u.access_code, u.created_at, e.state, \
         (SELECT COUNT(*) FROM chat_history c WHERE c.user_id = u.user_id), \
         (SELECT MAX(c.created_at) FROM chat_history c WHERE c.user_id = u.user_id) \
         FROM users u LEFT JOIN emotion e ON e.user_id = u.user_id {where_sql} \
         ORDER BY 6 DESC, u.username COLLATE NOCASE ASC LIMIT ?"
    )) {
        Ok(s) => s,
        Err(_) => return json!({ "users": [], "total": 0, "query": term }),
    };
    let rows: Vec<Value> = stmt
        .query_map(params![like, like, like, size], |r| {
            let state_s: Option<String> = r.get(4)?;
            let (bond, mood_energy) = match state_s
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            {
                Some(state) => (
                    state["bond"].as_f64().unwrap_or(0.0),
                    state.pointer("/mood/energy").cloned().unwrap_or(Value::Null),
                ),
                None => (0.0, Value::Null),
            };
            let access_code: Option<String> = r.get(2)?;
            Ok(json!({
                "userId": r.get::<_, String>(0)?,
                "username": r.get::<_, Option<String>>(1)?,
                "accessCode": access_code.clone().unwrap_or_default(),
                "keyRecoverable": access_code.map(|c| !c.is_empty()).unwrap_or(false),
                "createdAt": r.get::<_, Option<String>>(3)?,
                "messages": r.get::<_, i64>(5)?,
                "lastActive": r.get::<_, Option<String>>(6)?,
                "bond": (bond * 10.0).round() / 10.0,
                "bondName": crate::llm::bond_level_name(bond),
                "mood": mood_energy,
            }))
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    json!({ "users": rows, "total": total, "query": term })
}

pub fn list_user_tables(db: &Db, user_id: &str) -> Value {
    let mut tables: Vec<Value> = Vec::new();
    for name in table_names(db) {
        if is_hidden_table(&name) || !USER_FILTERED_TABLES.contains(&name.as_str()) {
            continue;
        }
        let columns = column_names(db, &name);
        if !columns.iter().any(|c| c == "user_id") {
            continue;
        }
        let count = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                &format!("SELECT COUNT(*) FROM \"{name}\" WHERE user_id = ?"),
                params![user_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
        };
        tables.push(json!({ "name": name, "label": label_of(&name), "count": count }));
    }
    tables.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    json!({ "userId": user_id, "tables": tables })
}

pub fn read_user_table(
    db: &Db,
    user_id: &str,
    table: &str,
    page: i64,
    page_size: i64,
    search: &str,
) -> Result<Value, String> {
    if !USER_FILTERED_TABLES.contains(&table) {
        return Err("Tabel ini tidak dikelompokkan per user.".to_string());
    }
    let available = list_user_tables(db, user_id);
    let found = available["tables"]
        .as_array()
        .map(|arr| arr.iter().any(|t| t["name"] == json!(table)))
        .unwrap_or(false);
    if !found {
        return Err("Tabel tidak dikenal.".to_string());
    }
    let size = if page_size == 0 { 25 } else { page_size }.clamp(1, 200);
    let current = page.max(1);
    let offset = (current - 1) * size;
    let columns = column_names(db, table);
    let term: String = search.trim().chars().take(120).collect();
    let mut where_parts = vec!["user_id = ?".to_string()];
    let mut params: Vec<SqlValue> = vec![SqlValue::Text(user_id.to_string())];
    if !term.is_empty() {
        let clauses: Vec<String> = columns
            .iter()
            .filter(|c| *c != "user_id" && !is_redacted(c))
            .map(|col| {
                params.push(SqlValue::Text(format!("%{term}%")));
                format!("CAST(\"{col}\" AS TEXT) LIKE ?")
            })
            .collect();
        if !clauses.is_empty() {
            where_parts.push(format!("({})", clauses.join(" OR ")));
        }
    }
    let where_sql = format!("WHERE {}", where_parts.join(" AND "));
    let total = {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM \"{table}\" {where_sql}"),
            rusqlite::params_from_iter(params.iter()),
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    let order_col = if columns.iter().any(|c| c == "created_at") {
        "created_at".to_string()
    } else {
        primary_key(db, table).or_else(|| columns.first().cloned()).unwrap_or_default()
    };
    let mut row_params = params.clone();
    row_params.push(SqlValue::Integer(size));
    row_params.push(SqlValue::Integer(offset));
    let rows = safe_rows(
        db,
        &format!("SELECT * FROM \"{table}\" {where_sql} ORDER BY \"{order_col}\" DESC LIMIT ? OFFSET ?"),
        row_params,
        &columns,
    );
    let page_count = ((total as f64) / (size as f64)).ceil().max(1.0) as i64;
    Ok(json!({
        "userId": user_id,
        "table": table,
        "label": label_of(table),
        "page": current.min(page_count),
        "pageSize": size,
        "pageCount": page_count,
        "total": total,
        "columns": columns.iter().filter(|c| !is_redacted(c)).cloned().collect::<Vec<_>>(),
        "redactedColumns": columns.iter().filter(|c| is_redacted(c)).cloned().collect::<Vec<_>>(),
        "rows": rows,
    }))
}

// === admin-browser.js: browser workspace (read-only, sandboxed) ===

const SKIP_DIRS: [&str; 5] = ["node_modules", ".git", "__pycache__", ".venv", "venv"];

pub fn resolve_safe_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let base = root.to_path_buf();
    if Path::new(relative).is_absolute() {
        return Err("Path absolut tidak diizinkan.".to_string());
    }
    let lower = relative.to_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return Err("Path ter-encode tidak diizinkan.".to_string());
    }
    let clean = relative.replace('\\', "/").trim_start_matches('/').to_string();
    let target = base.join(&clean);
    // Normalisasi manual: tolak segmen .. yang keluar dari root.
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for comp in target.components() {
        match comp {
            std::path::Component::ParentDir => {
                if parts.len() <= base.components().count() {
                    return Err("Path di luar workspace tidak diizinkan.".to_string());
                }
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    let normalized: PathBuf = parts.iter().collect();
    if normalized != base && !normalized.starts_with(&base) {
        return Err("Path di luar workspace tidak diizinkan.".to_string());
    }
    Ok(normalized)
}

fn iso_mtime(meta: &std::fs::Metadata) -> Option<String> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| {
            chrono::DateTime::from_timestamp_millis(d.as_millis() as i64)
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                .unwrap_or_default()
        })
}

pub fn workspace_summary(root: &Path) -> Value {
    if !root.exists() {
        return json!({ "root": root.to_string_lossy(), "exists": false, "users": [] });
    }
    let mut users: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !entry.path().is_dir() || name.starts_with('.') {
                continue;
            }
            let (files, bytes) = directory_size(&entry.path());
            let mut mtime: Option<String> = None;
            // mtime terbaru di pohon (perkiraan dari root folder user).
            if let Ok(meta) = entry.metadata() {
                mtime = iso_mtime(&meta);
            }
            users.push(json!({ "name": name, "files": files, "bytes": bytes, "mtime": mtime }));
        }
    }
    users.sort_by(|a, b| b["bytes"].as_i64().cmp(&a["bytes"].as_i64()));
    json!({ "root": root.to_string_lossy(), "exists": true, "users": users })
}

pub fn list_directory(root: &Path, relative: &str) -> Result<Value, String> {
    if !root.exists() {
        return Ok(json!({ "path": "", "absolute": root.to_string_lossy(), "exists": false, "entries": [] }));
    }
    let target = resolve_safe_path(root, relative)?;
    if !target.exists() {
        return Err("Folder tidak ditemukan.".to_string());
    }
    if !target.is_dir() {
        return Err("Bukan sebuah folder.".to_string());
    }
    let mut entries: Vec<Value> = Vec::new();
    let read = std::fs::read_dir(&target).map_err(|e| e.to_string())?;
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let is_dir = entry.path().is_dir();
        let (size, mtime) = match entry.metadata() {
            Ok(meta) => (meta.len() as i64, iso_mtime(&meta)),
            Err(_) => (0, None),
        };
        entries.push(json!({
            "name": name,
            "type": if is_dir { "dir" } else { "file" },
            "size": size,
            "mtime": mtime,
        }));
    }
    entries.sort_by(|a, b| {
        let (ta, tb) = (a["type"].as_str().unwrap_or(""), b["type"].as_str().unwrap_or(""));
        if ta != tb {
            if ta == "dir" { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
        } else {
            a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
        }
    });
    Ok(json!({
        "path": relative,
        "absolute": target.to_string_lossy(),
        "exists": true,
        "entries": entries,
    }))
}

pub fn read_text_file(root: &Path, relative: &str) -> Result<Value, String> {
    let target = resolve_safe_path(root, relative)?;
    let meta = std::fs::metadata(&target).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err("Ini sebuah folder.".to_string());
    }
    let bytes = std::fs::read(&target).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let max = 64 * 1024usize;
    if (meta.len() as usize) > max {
        let content: String = text.chars().take(max).collect();
        return Ok(json!({ "truncated": true, "size": meta.len(), "content": content }));
    }
    Ok(json!({ "truncated": false, "size": meta.len(), "content": text }))
}
