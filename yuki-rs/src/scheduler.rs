//! Port `lib/scheduler.js`: scheduled tasks per user.
//!
//! Perbedaan mesin (ruling Fase 3): Node memakai node-cron per task; Rust
//! memakai satu ticker 15 detik yang (a) memulihkan occurrence run_once
//! yang terlewat, (b) meng-enqueue occurrence untuk task yang next_run-nya
//! jatuh tempo, (c) menjalankan processDueOccurrences — pipeline occurrence
//! di DB (claim, delivered, retry backoff maks 5x) identik dengan Node.

use crate::db::Db;
use crate::push;
use crate::state::AppState;
use crate::timeutil::{
    normalize_timezone, parse_iso_loose, to_iso_z, zoned_datetime_to_utc, zoned_parts,
};
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ===== Parser bahasa natural -> cron (scheduler.js) =====
fn normalize_clock_words(text: &str) -> String {
    let to_hour = |h: i64, mi: &str, mode: &str| -> String {
        let hour = match mode {
            "pagi" => {
                if h == 12 {
                    0
                } else {
                    h
                }
            }
            "malam" => {
                if h == 12 {
                    0
                } else if h < 12 {
                    h + 12
                } else {
                    h
                }
            }
            _ => {
                if h == 12 {
                    12
                } else if h < 12 {
                    h + 12
                } else {
                    h
                }
            }
        };
        if mi.is_empty() {
            format!("jam {hour}")
        } else {
            format!("jam {hour}:{mi}")
        }
    };
    let re1 = Regex::new(r"(?i)jam\s+(\d{1,2})(?::(\d{2}))?\s*(pagi|siang|sore|malam)").unwrap();
    let step = re1.replace_all(text, |caps: &regex::Captures| {
        to_hour(
            caps[1].parse().unwrap_or(0),
            caps.get(2).map(|m| m.as_str()).unwrap_or(""),
            &caps[3].to_lowercase(),
        )
    });
    let re2 = Regex::new(r"(?i)\b(\d{1,2})\s*(pagi|siang|sore|malam)\b").unwrap();
    re2.replace_all(&step, |caps: &regex::Captures| {
        to_hour(caps[1].parse().unwrap_or(0), "00", &caps[2].to_lowercase())
    })
    .to_string()
}

fn normalize_dot_clock(text: &str) -> String {
    let re = Regex::new(r"\b(\d{1,2})\.(\d{2})\b").unwrap();
    re.replace_all(text, |caps: &regex::Captures| {
        let h: i64 = caps[1].parse().unwrap_or(99);
        let m: i64 = caps[2].parse().unwrap_or(99);
        if h <= 23 && m <= 59 {
            format!("{}:{}", &caps[1], &caps[2])
        } else {
            caps[0].to_string()
        }
    })
    .to_string()
}

pub struct ParsedSchedule {
    pub cron: String,
    pub human: String,
    pub run_once: bool,
    pub run_at: Option<String>,
    pub timezone: Option<String>,
}

pub fn parse_to_cron_expr(natural: &str, timezone: &str, now: DateTime<Utc>) -> Option<ParsedSchedule> {
    let t = normalize_clock_words(&normalize_dot_clock(&natural.to_lowercase()))
        .trim()
        .to_string();
    let zone = normalize_timezone(timezone);

    if let Some(caps) = Regex::new(r"setiap\s+(\d+)\s+menit").unwrap().captures(&t) {
        return Some(ParsedSchedule {
            cron: format!("*/{} * * * *", &caps[1]),
            human: format!("Setiap {} menit", &caps[1]),
            run_once: false,
            run_at: None,
            timezone: None,
        });
    }
    if let Some(caps) = Regex::new(r"setiap\s+(\d+)\s+jam").unwrap().captures(&t) {
        return Some(ParsedSchedule {
            cron: format!("0 */{} * * *", &caps[1]),
            human: format!("Setiap {} jam", &caps[1]),
            run_once: false,
            run_at: None,
            timezone: None,
        });
    }
    if let Some(caps) = Regex::new(r"setiap\s+hari\s+(?:jam\s+)?(\d{1,2})(?::(\d{2}))?")
        .unwrap()
        .captures(&t)
    {
        let h = &caps[1];
        let m = caps.get(2).map(|x| x.as_str()).unwrap_or("0");
        return Some(ParsedSchedule {
            // Node memakai string tangkapan mentah untuk cron + jam human.
            cron: format!("{m} {h} * * *"),
            human: format!("Setiap hari jam {}:{:02}", h, m.parse::<i64>().unwrap_or(0)),
            run_once: false,
            run_at: None,
            timezone: None,
        });
    }
    let days: [(&str, i64); 7] = [
        ("senin", 1),
        ("selasa", 2),
        ("rabu", 3),
        ("kamis", 4),
        ("jumat", 5),
        ("sabtu", 6),
        ("minggu", 0),
    ];
    for (name, num) in days {
        let pattern = Regex::new(&format!(
            r"setiap\s+{name}(?:\s+jam\s+(\d{{1,2}})(?::(\d{{2}}))?)?"
        ))
        .unwrap();
        if let Some(caps) = pattern.captures(&t) {
            let h = caps.get(1).map(|x| x.as_str()).unwrap_or("8");
            let min = caps.get(2).map(|x| x.as_str()).unwrap_or("0");
            let label = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
            return Some(ParsedSchedule {
                cron: format!("{min} {h} * * {num}"),
                human: format!(
                    "Setiap {} jam {}:{:02}",
                    label,
                    h,
                    min.parse::<i64>().unwrap_or(0)
                ),
                run_once: false,
                run_at: None,
                timezone: None,
            });
        }
    }
    if let Some(caps) = Regex::new(r"(?:besok|sekali)\s+jam\s+(\d{1,2})(?::(\d{2}))?")
        .unwrap()
        .captures(&t)
    {
        let h_raw = &caps[1];
        let min_raw = caps.get(2).map(|x| x.as_str()).unwrap_or("0");
        let h: u32 = h_raw.parse().unwrap_or(0);
        let min: u32 = min_raw.parse().unwrap_or(0);
        let parts = zoned_parts(now, &zone);
        // Besok di zona lokal: ambil tanggal lokal + 1 hari via UTC noon.
        let noon_utc = zoned_datetime_to_utc(parts.year, parts.month, parts.day, 12, 0, 0, "UTC")
            + Duration::days(1);
        let target = zoned_parts(noon_utc, "UTC");
        let run_at = zoned_datetime_to_utc(target.year, target.month, target.day, h, min, 0, &zone);
        return Some(ParsedSchedule {
            // Node: cron + jam human memakai string tangkapan mentah
            // ("07" tetap "07"), menit human padStart(2) — setara {:02}.
            cron: format!("{min_raw} {h_raw} {} {} *", target.day, target.month),
            human: format!("Sekali besok jam {}:{:02}", h_raw, min),
            run_once: true,
            run_at: Some(to_iso_z(&run_at)),
            timezone: Some(zone),
        });
    }
    if let Some(caps) = Regex::new(r"(\d+)\s+menit\s+(?:lagi|dari sekarang)")
        .unwrap()
        .captures(&t)
    {
        let n: i64 = caps[1].parse().unwrap_or(0);
        let when = now + Duration::minutes(n);
        let local = zoned_parts(when, &zone);
        return Some(ParsedSchedule {
            cron: format!("{} {} {} {} *", local.minute, local.hour, local.day, local.month),
            human: format!("{} menit lagi", &caps[1]),
            run_once: true,
            run_at: Some(to_iso_z(&when)),
            timezone: None,
        });
    }
    if let Some(caps) = Regex::new(r"(\d+)\s+jam\s+(?:lagi|dari sekarang)")
        .unwrap()
        .captures(&t)
    {
        let n: i64 = caps[1].parse().unwrap_or(0);
        let when = now + Duration::hours(n);
        let local = zoned_parts(when, &zone);
        return Some(ParsedSchedule {
            cron: format!("{} {} {} {} *", local.minute, local.hour, local.day, local.month),
            human: format!("{} jam lagi", &caps[1]),
            run_once: true,
            run_at: Some(to_iso_z(&when)),
            timezone: None,
        });
    }
    if let Some(caps) = Regex::new(r"(?:hari\s+ini\s+)?jam\s+(\d{1,2})(?::(\d{2}))?(?:\s*(?:nanti))?$")
        .unwrap()
        .captures(&t)
    {
        let hour: u32 = caps[1].parse().unwrap_or(0);
        let minute: u32 = caps.get(2).and_then(|x| x.as_str().parse().ok()).unwrap_or(0);
        if hour > 23 || minute > 59 {
            return None;
        }
        let parts = zoned_parts(now, &zone);
        let mut run_at = zoned_datetime_to_utc(parts.year, parts.month, parts.day, hour, minute, 0, &zone);
        if run_at <= now {
            if t.contains("hari ini") {
                return None;
            }
            let noon_utc = zoned_datetime_to_utc(parts.year, parts.month, parts.day, 12, 0, 0, "UTC")
                + Duration::days(1);
            let tomorrow = zoned_parts(noon_utc, "UTC");
            run_at = zoned_datetime_to_utc(tomorrow.year, tomorrow.month, tomorrow.day, hour, minute, 0, &zone);
        }
        let local = zoned_parts(run_at, &zone);
        return Some(ParsedSchedule {
            cron: format!("{} {} {} {} *", local.minute, local.hour, local.day, local.month),
            human: format!(
                "Sekali {}-{:02}-{:02} jam {:02}:{:02}",
                local.year, local.month, local.day, hour, minute
            ),
            run_once: true,
            run_at: Some(to_iso_z(&run_at)),
            timezone: Some(zone),
        });
    }
    None
}

fn field_matches(field: &str, value: u32) -> bool {
    if field == "*" {
        return true;
    }
    if let Some(step) = field.strip_prefix("*/") {
        let n: u32 = step.parse().unwrap_or(0);
        return n != 0 && value % n == 0;
    }
    field
        .split(',')
        .any(|item| item.parse::<u32>().map(|v| v == value).unwrap_or(false))
}

pub fn cron_valid(expr: &str) -> bool {
    expr.split_whitespace().count() == 5
}

pub fn estimate_next_run(cron_expr: &str, timezone: &str, now: DateTime<Utc>) -> Option<String> {
    let fields: Vec<&str> = cron_expr.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let zone = normalize_timezone(timezone);
    for offset in 1..=(8 * 24 * 60) {
        let candidate = now + Duration::minutes(offset);
        let p = zoned_parts(candidate, &zone);
        if field_matches(fields[0], p.minute)
            && field_matches(fields[1], p.hour)
            && field_matches(fields[2], p.day)
            && field_matches(fields[3], p.month)
            && field_matches(fields[4], p.weekday)
        {
            return Some(to_iso_z(&candidate));
        }
    }
    None
}

// ===== CRUD =====
pub struct CreateTask {
    pub user_id: String,
    pub title: String,
    pub description: String,
    pub cron_expr: String,
    pub human_schedule: String,
    pub run_once: bool,
    pub run_at: Option<String>,
    pub timezone: String,
    pub idempotency_key: String,
}

pub fn create_scheduled_task(db: &Db, input: CreateTask) -> Option<Value> {
    let zone = normalize_timezone(&input.timezone);
    let key = if input.idempotency_key.is_empty() {
        sha256_hex(&format!(
            "{}:{}:{}:{}:{}",
            input.user_id, input.title, input.description, input.cron_expr, zone
        ))
        .chars()
        .take(120)
        .collect::<String>()
    } else {
        input.idempotency_key.chars().take(120).collect()
    };
    if let Some(existing) = db.find_task_by_idempotency(&input.user_id, &key) {
        let mut existing = existing;
        existing["idempotent"] = json!(true);
        existing["verified"] = json!(true);
        return Some(existing);
    }
    let next_run = input
        .run_at
        .clone()
        .or_else(|| estimate_next_run(&input.cron_expr, &zone, Utc::now()));
    let mut saved = db.insert_task(
        &input.user_id,
        &input.title,
        &input.description,
        &input.cron_expr,
        &input.human_schedule,
        input.run_once,
        input.run_at.as_deref(),
        next_run.as_deref(),
        &zone,
        &key,
    )?;
    saved["verified"] = json!(saved.is_object());
    Some(saved)
}

pub fn get_agenda(db: &Db, user_id: &str) -> Value {
    json!({
        "timezone": "Asia/Jakarta",
        "tasks": db.list_scheduled_tasks(user_id),
        "deliveries": db.agenda_deliveries(user_id),
    })
}

pub fn set_task_status(db: &Db, task_id: i64, user_id: &str, status: &str) -> Value {
    if !["active", "paused", "cancelled"].contains(&status) {
        return json!({ "success": false, "error": "Status jadwal tidak valid." });
    }
    let Some(task) = db.get_task_full(task_id, user_id) else {
        return json!({ "success": false, "error": "Pengingat tidak ditemukan atau sudah selesai." });
    };
    let current = task["status"].as_str().unwrap_or("");
    if current == "cancelled" || current == "done" {
        return json!({ "success": false, "error": "Pengingat tidak ditemukan atau sudah selesai." });
    }
    db.update_task_status(task_id, user_id, status);
    json!({ "success": true, "verified": true, "id": task_id, "status": status })
}

pub fn snooze_task(db: &Db, task_id: i64, user_id: &str, minutes: i64) -> Value {
    let safe = minutes.clamp(1, 1440);
    let Some(task) = db.get_task_full(task_id, user_id) else {
        return json!({ "success": false, "error": "Pengingat tidak ditemukan." });
    };
    let when = Utc::now() + Duration::minutes(safe);
    let zone = task["timezone"].as_str().unwrap_or("Asia/Jakarta");
    let local = zoned_parts(when, zone);
    let cron = format!("{} {} {} {} *", local.minute, local.hour, local.day, local.month);
    let when_iso = to_iso_z(&when);
    db.snooze_task(task_id, user_id, &when_iso, &cron, &format!("Ditunda {safe} menit"));
    json!({ "success": true, "verified": true, "id": task_id, "status": "active", "nextRunAtUtc": when_iso })
}

pub fn update_task(
    db: &Db,
    task_id: i64,
    user_id: &str,
    title: Option<&str>,
    description: Option<&str>,
    schedule: Option<&str>,
    timezone: Option<&str>,
) -> Value {
    let Some(mut current) = db.get_task_full(task_id, user_id) else {
        return json!({ "success": false, "error": "Pengingat tidak ditemukan." });
    };
    if let Some(schedule) = schedule {
        let moved = reschedule_task(
            db,
            Some(task_id),
            user_id,
            schedule,
            timezone.unwrap_or(current["timezone"].as_str().unwrap_or("Asia/Jakarta")),
        );
        if moved["success"].as_bool() != Some(true) {
            return moved;
        }
        current = db.get_task_full(task_id, user_id).unwrap_or(current);
    }
    let new_title: String = title
        .unwrap_or(current["title"].as_str().unwrap_or(""))
        .trim()
        .chars()
        .take(120)
        .collect();
    let new_desc: String = description
        .unwrap_or(current["description"].as_str().unwrap_or(""))
        .trim()
        .chars()
        .take(500)
        .collect();
    db.update_task_title_desc(task_id, user_id, &new_title, &new_desc);
    let task = db
        .list_scheduled_tasks(user_id)
        .into_iter()
        .find(|item| item["id"] == json!(task_id));
    json!({ "success": true, "verified": true, "task": task })
}

pub fn cancel_task(db: &Db, task_id: i64, user_id: &str) -> Value {
    let Some(task) = db.get_task_full(task_id, user_id) else {
        return json!({ "success": false, "message": "Task tidak ditemukan atau bukan milikmu." });
    };
    db.set_task_status_by_id(task_id, "cancelled");
    json!({
        "success": true, "verified": true, "id": task_id,
        "title": task["title"], "status": "cancelled",
        "message": format!("Task \"{}\" berhasil dibatalkan.", task["title"].as_str().unwrap_or("")),
    })
}

pub fn reschedule_task(
    db: &Db,
    task_id: Option<i64>,
    user_id: &str,
    schedule: &str,
    timezone: &str,
) -> Value {
    let task = match task_id {
        Some(id) => db
            .get_task_full(id, user_id)
            .filter(|t| t["status"].as_str() == Some("active")),
        None => db.latest_active_task(user_id),
    };
    let Some(task) = task else {
        return json!({ "success": false, "error": "Pengingat aktif tidak ditemukan. Sebutkan ID jadwal jika ingin mengubah jadwal tertentu." });
    };
    let Some(parsed) = parse_to_cron_expr(schedule, timezone, Utc::now()) else {
        return json!({ "success": false, "error": format!("Format jadwal tidak dikenali atau waktunya sudah lewat: \"{schedule}\".") });
    };
    let zone = parsed
        .timezone
        .clone()
        .unwrap_or_else(|| normalize_timezone(timezone));
    let next_run = parsed
        .run_at
        .clone()
        .or_else(|| estimate_next_run(&parsed.cron, &zone, Utc::now()));
    let id = task["id"].as_i64().unwrap_or(0);
    db.reschedule_task(
        id,
        user_id,
        &parsed.cron,
        &parsed.human,
        parsed.run_once,
        parsed.run_at.as_deref(),
        next_run.as_deref(),
        &zone,
    );
    let saved = db.get_task_full(id, user_id).map(|t| {
        json!({
            "id": t["id"], "title": t["title"], "humanSchedule": t["human_schedule"],
            "nextRunAtUtc": if t["next_run_at_utc"].is_null() { t["next_run"].clone() } else { t["next_run_at_utc"].clone() },
            "timezone": t["timezone"],
        })
    });
    json!({ "success": true, "verified": saved.is_some(), "task": saved })
}

// ===== Mesin occurrence =====
fn occurrence_id(task_id: i64, normalized: &str) -> String {
    sha256_hex(&format!("{task_id}:{normalized}"))
        .chars()
        .take(32)
        .collect()
}

fn normalize_minute(iso: &str) -> Option<String> {
    let dt = parse_iso_loose(iso)?;
    Some(format!("{}:00.000Z", dt.format("%Y-%m-%dT%H:%M")))
}

pub fn enqueue_occurrence(db: &Db, task_id: i64, scheduled_for_iso: &str) {
    let Some(task) = db.get_task_full_by_id(task_id) else {
        return;
    };
    if task["status"].as_str() != Some("active") {
        return;
    }
    let Some(normalized) = normalize_minute(scheduled_for_iso) else {
        return;
    };
    let id = occurrence_id(task_id, &normalized);
    db.enqueue_occurrence(
        &id,
        task_id,
        task["user_id"].as_str().unwrap_or(""),
        &normalized,
    );
}

/// Callback pengiriman server.js: simpan pesan pengingat + kirim push.
pub async fn deliver_reminder(state: &AppState, user_id: &str, task: &Value) {
    let title = task["title"].as_str().unwrap_or("");
    let description = task["description"].as_str().unwrap_or("");
    let generic = description.is_empty()
        || Regex::new(r"(?i)^Pengingat:\s*Pengingat$")
            .unwrap()
            .is_match(description.trim());
    let detail = if generic {
        "Waktunya melakukan hal yang tadi kamu minta Yuki ingatkan."
    } else {
        description.trim()
    };
    let message = format!("[YUKI_REMINDER]\n*mengetuk layar dua kali agar kamu memperhatikan*\n\n### ⏰ {title}\n\n{detail}\n\nHmph, Yuki sudah menepati janji mengingatkanmu. Sekarang jangan malah diabaikan, ya.\n\n[emosi: kesal]");
    state.db.save_chat_message(user_id, "assistant", &message);
    let occurrence_id = task["occurrenceId"].as_str().unwrap_or("");
    let payload = json!({
        "title": format!("⏰ {title}"),
        "body": format!("{detail} — Hmph, jangan diabaikan ya."),
        "tag": format!("yuki-reminder-{}", if occurrence_id.is_empty() { task["id"].to_string() } else { occurrence_id.to_string() }),
        "url": "/",
        "occurrenceId": task["occurrenceId"],
    });
    let result = push::send_push_to_user(state, user_id, &payload).await;
    if result.failed > 0 {
        eprintln!("[push] {} subscription gagal untuk user {}", result.failed, user_id);
    }
    println!("[scheduler] Notifikasi pengingat disimpan untuk user {user_id}: \"{title}\"");
}

pub async fn process_due_occurrences(state: &AppState, now: DateTime<Utc>) -> usize {
    let due = state.db.due_occurrences(&to_iso_z(&now));
    let count = due.len();
    for occurrence in &due {
        let occ_id = occurrence["id"].as_str().unwrap_or("").to_string();
        if !state.db.claim_occurrence(&occ_id) {
            continue;
        }
        let user_id = occurrence["user_id"].as_str().unwrap_or("").to_string();
        let task_arg = json!({
            "id": occurrence["task_id"], "occurrenceId": occ_id,
            "title": occurrence["title"], "description": occurrence["description"],
        });
        // Callback selalu tersedia di server Rust (deliver_reminder).
        deliver_reminder(state, &user_id, &task_arg).await;
        state.db.mark_occurrence_delivered(&occ_id);
        let task_id = occurrence["task_id"].as_i64().unwrap_or(0);
        state.db.set_task_last_run(task_id);
        if occurrence["run_once"].as_i64() == Some(1) {
            state.db.set_task_status_by_id(task_id, "done");
        } else {
            let next = estimate_next_run(
                occurrence["cron_expr"].as_str().unwrap_or(""),
                occurrence["timezone"].as_str().unwrap_or("Asia/Jakarta"),
                now + Duration::seconds(1),
            );
            state.db.update_task_next_run(task_id, next.as_deref());
        }
    }
    count
}

/// Satu siklus ticker: pulihkan yang terlewat, enqueue yang jatuh tempo,
/// lalu proses occurrence yang due.
pub async fn tick(state: &AppState) {
    let now = Utc::now();
    let now_iso = to_iso_z(&now);
    // recoverMissedOccurrences: run_once aktif yang next_run-nya sudah lewat.
    for task in state.db.due_run_once_tasks(&now_iso) {
        let scheduled = task["next_run_at_utc"]
            .as_str()
            .or_else(|| task["run_at"].as_str())
            .or_else(|| task["next_run"].as_str())
            .unwrap_or("")
            .to_string();
        if !scheduled.is_empty() {
            enqueue_occurrence(&state.db, task["id"].as_i64().unwrap_or(0), &scheduled);
        }
    }
    // Task berulang yang next_run-nya jatuh tempo -> enqueue occurrence.
    for task in state.db.active_tasks() {
        if task["run_once"].as_i64() == Some(1) {
            continue;
        }
        let Some(next_run) = task["next_run_at_utc"]
            .as_str()
            .or_else(|| task["next_run"].as_str())
        else {
            continue;
        };
        if next_run <= now_iso.as_str() {
            enqueue_occurrence(&state.db, task["id"].as_i64().unwrap_or(0), next_run);
        }
    }
    let _ = process_due_occurrences(state, now).await;
}

pub fn spawn_ticker(state: AppState) {
    tokio::spawn(async move {
        // restoreScheduledJobs: proses sekali di awal, lalu tiap 15 detik.
        tick(&state).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            interval.tick().await;
            tick(&state).await;
        }
    });
}

pub fn scheduler_stats(db: &Db) -> Value {
    json!({ "activeJobs": db.count_active_tasks(), "dbActive": db.count_active_tasks() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parse_variants() {
        let p = parse_to_cron_expr("setiap hari jam 7:30", "Asia/Jakarta", now()).unwrap();
        assert_eq!(p.cron, "30 7 * * *");
        assert!(!p.run_once);
        let p = parse_to_cron_expr("setiap senin jam 8", "Asia/Jakarta", now()).unwrap();
        assert_eq!(p.cron, "0 8 * * 1");
        let p = parse_to_cron_expr("30 menit lagi", "Asia/Jakarta", now()).unwrap();
        assert!(p.run_once);
        assert_eq!(p.run_at.as_deref(), Some("2026-11-09T02:30:00.000Z"));
        let p = parse_to_cron_expr("ingatkan jam 19.15 nanti", "Asia/Jakarta", now()).unwrap();
        assert!(p.run_once);
        // 19:15 WIB = 12:15 UTC
        assert_eq!(p.run_at.as_deref(), Some("2026-11-09T12:15:00.000Z"));
        assert!(parse_to_cron_expr("kapan-kapan aja", "Asia/Jakarta", now()).is_none());
    }

    #[test]
    fn estimate_daily() {
        let next = estimate_next_run("30 7 * * *", "Asia/Jakarta", now()).unwrap();
        // now = 09:00 WIB; berikutnya besok 07:30 WIB = 00:30 UTC
        assert_eq!(next, "2026-11-10T00:30:00.000Z");
    }
}
