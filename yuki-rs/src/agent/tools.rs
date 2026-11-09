//! Port skills-engine executeTool + handler non-workspace/artifact/web:
//! kalkulator, datetime, catatan, scheduler, self-improvement,
//! code-analyzer, scratchpad, skill-health, delegasi.

use std::sync::Arc;
use std::time::Instant;

use regex::Regex;
use serde_json::{json, Value};

use crate::agent::sandbox::{embedded_check_js_syntax, embedded_check_ts_syntax, run_embedded_snippet, SandboxLimits};
use crate::agent::skills;
use crate::agent::toolresult::{build_evidence, create_tool_result, verify_tool_result};
use crate::db::Db;
use crate::llm::Llm;
use crate::provider::router::RunBudget;

pub type CancelFn = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone)]
pub struct ToolContext {
    pub db: Arc<Db>,
    pub http: reqwest::Client,
    pub llm: Arc<Llm>,
    pub user_id: String,
    pub username: String,
    pub active_artifact_id: Option<String>,
    pub budget: Arc<tokio::sync::Mutex<RunBudget>>,
    pub should_cancel: CancelFn,
    pub tool_call_id: Option<String>,
}

impl ToolContext {
    pub fn new(db: Arc<Db>, http: reqwest::Client, llm: Arc<Llm>, user_id: &str) -> Self {
        Self {
            db,
            http,
            llm,
            user_id: user_id.to_string(),
            username: String::new(),
            active_artifact_id: None,
            budget: Arc::new(tokio::sync::Mutex::new(RunBudget::default())),
            should_cancel: Arc::new(|| false),
            tool_call_id: None,
        }
    }
}

/// Hasil mentah handler: objek data (bisa memuat error/success:false).
pub type HandlerResult = Value;

fn err(message: &str) -> Value {
    json!({ "error": message })
}

pub async fn execute_tool(tool_name: &str, args: &Value, ctx: &ToolContext) -> Value {
    let Some((skill_name, _)) = skills::tool_skill(tool_name) else {
        return json!({ "error": format!("Tool \"{tool_name}\" tidak ditemukan atau belum terdaftar.") });
    };
    // Judul skill dari katalog runtime (sama seperti Node).
    let skill_title = skills::skills_catalog()
        .iter()
        .find(|s| s.name == skill_name)
        .map(|s| s.title.clone())
        .unwrap_or_else(|| skill_name.to_string());
    let started = Instant::now();
    let started_at = crate::timeutil::to_iso_z(&chrono::Utc::now());
    let result = dispatch(tool_name, args, ctx).await;
    let duration = started.elapsed().as_millis() as i64;
    let is_failure = result.get("success") == Some(&json!(false)) || result.get("error").is_some();
    if is_failure {
        let message = result["error"].as_str().unwrap_or("Tool gagal tanpa detail.").to_string();
        let contract = create_tool_result(
            ctx.tool_call_id.as_deref(),
            skill_name,
            tool_name,
            "failed",
            &result,
            Some(json!({
                "code": result["code"].as_str().unwrap_or("TOOL_FAILED"),
                "message": message,
                "retryable": result["retryable"].as_bool().unwrap_or(false),
            })),
            vec![],
            &started_at,
            duration,
        );
        return json!({
            "tool": tool_name,
            "skillName": skill_name,
            "skillTitle": skill_title,
            "durationMs": duration,
            "error": message,
            "data": result,
            "contract": contract,
            "evidence": [],
        });
    }
    let evidence = build_evidence(tool_name, &result);
    let mut contract = create_tool_result(
        ctx.tool_call_id.as_deref(),
        skill_name,
        tool_name,
        "succeeded",
        &result,
        None,
        evidence.clone(),
        &started_at,
        duration,
    );
    let (verified, reason) = verify_tool_result(&contract);
    if !verified {
        contract["status"] = json!("failed");
        contract["error"] = json!({ "code": "EVIDENCE_MISSING", "message": reason, "retryable": false });
        return json!({
            "tool": tool_name,
            "skillName": skill_name,
            "skillTitle": skill_title,
            "durationMs": duration,
            "error": reason,
            "data": result,
            "contract": contract,
            "evidence": evidence,
        });
    }
    json!({
        "tool": tool_name,
        "skillName": skill_name,
        "skillTitle": skill_title,
        "durationMs": duration,
        "data": result,
        "contract": contract,
        "evidence": evidence,
    })
}

async fn dispatch(tool_name: &str, args: &Value, ctx: &ToolContext) -> Value {
    match tool_name {
        // workspace & artifacts & web di modul lain
        "list_workspace_files" | "read_workspace_file" | "search_workspace_code"
        | "create_workspace_file" | "replace_workspace_text" | "get_workspace_diff"
        | "rollback_workspace_file" | "patch_workspace_files" | "validate_workspace_project"
        | "run_workspace_tests" => crate::agent::tools_workspace::dispatch_workspace(tool_name, args, ctx).await,
        "validate_interactive_artifact" | "build_interactive_artifact" | "get_active_artifact"
        | "read_artifact_file" | "update_interactive_artifact" | "patch_interactive_artifact" => {
            crate::agent::tools_artifacts::dispatch_artifacts(tool_name, args, ctx).await
        }
        "web_search" | "read_url" | "http_api_request" | "search_ryukomik" | "get_latest_comics"
        | "browse_page" | "screenshot_url" | "browser_extract" => {
            crate::agent::tools_web::dispatch_web(tool_name, args, ctx).await
        }
        "calculate_expression" => calculate_expression(args),
        "get_current_time" => get_current_time(args),
        "calculate_date_difference" => calculate_date_difference(args),
        "add_user_note" => add_user_note(args, ctx),
        "list_user_notes" => list_user_notes(args, ctx),
        "delete_user_note" => delete_user_note(args, ctx),
        "schedule_task" => schedule_task(args, ctx),
        "reschedule_task" => reschedule_task(args, ctx),
        "list_scheduled_tasks" => list_scheduled_tasks(ctx),
        "cancel_scheduled_task" => cancel_scheduled_task(args, ctx),
        "record_self_improvement" => record_self_improvement(args, ctx),
        "list_self_improvements" => list_self_improvements(args, ctx),
        "analyze_code_syntax" => analyze_code_syntax(args).await,
        "run_javascript_code" => run_javascript_code(args).await,
        "run_skill_health_check" => run_skill_health_check(ctx).await,
        "delegate_tasks" => delegate_tasks_tool(args, ctx).await,
        _ => err(&format!("Tool \"{tool_name}\" tidak ditemukan atau belum terdaftar.")),
    }
}

// ===== safe-calculator =====

pub fn calculate_expression(args: &Value) -> Value {
    let expression = args["expression"].as_str().unwrap_or("");
    if expression.is_empty() {
        return err("Ekspresi matematika tidak boleh kosong.");
    }
    let mut clean = expression.replace('×', "*").replace('÷', "/");
    let pct_of = Regex::new(r"(?i)(\d+)\s*%\s*(?:dari|of|\*)\s*(\d+(?:\.\d+)?)").unwrap();
    clean = pct_of
        .replace_all(&clean, |c: &regex::Captures| format!("({}/100 * {})", &c[1], &c[2]))
        .to_string();
    let pct = Regex::new(r"(\d+)%").unwrap();
    clean = pct
        .replace_all(&clean, |c: &regex::Captures| format!("({}/100)", &c[1]))
        .to_string();
    let clean = clean.trim().to_string();
    let allowed = Regex::new(r"(?i)^[0-9+\-*/().,\s%^eMathsqrtcospowabsroundfloorceillogPIE]+$").unwrap();
    if !allowed.is_match(&clean) {
        return err(&format!(
            "Ekspresi mengandung karakter atau fungsi yang tidak diizinkan: \"{expression}\""
        ));
    }
    match eval_expression(&clean) {
        Ok(result) => {
            if result.is_nan() || result.is_infinite() {
                let bad = if result.is_nan() { "NaN" } else { "Infinity" };
                let _ = bad;
                return err("Hasil bukan angka yang valid.");
            }
            let rounded = if result.fract() == 0.0 {
                result
            } else {
                (result * 1e6).round() / 1e6
            };
            json!({
                "expression": expression,
                "evaluatedExpression": clean,
                "result": serde_json::Number::from_f64(rounded).unwrap_or(serde_json::Number::from(0)),
                "formatted": format_number_id(result),
            })
        }
        Err(message) => err(&format!("Gagal menghitung ekspresi: {message}")),
    }
}

/// Format Intl.NumberFormat('id-ID'): ribuan '.', desimal ','.
pub fn format_number_id(value: f64) -> String {
    let negative = value < 0.0;
    let abs = value.abs();
    let text = if abs.fract() == 0.0 {
        format!("{}", abs as i64)
    } else {
        let s = format!("{abs:.10}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    let (int_part, frac_part) = match text.split_once('.') {
        Some((a, b)) => (a.to_string(), Some(b.to_string())),
        None => (text, None),
    };
    let mut grouped = String::new();
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    let mut out = if negative { "-".to_string() } else { String::new() };
    out.push_str(&grouped);
    if let Some(frac) = frac_part {
        out.push(',');
        out.push_str(&frac);
    }
    out
}

struct ExprParser<'a> {
    chars: Vec<char>,
    pos: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while self.peek().map(|c| c.is_whitespace()).unwrap_or(false) {
            self.pos += 1;
        }
    }
    fn parse_expr(&mut self) -> Result<f64, String> {
        let mut value = self.parse_term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.next();
                    value += self.parse_term()?;
                }
                Some('-') => {
                    self.next();
                    value -= self.parse_term()?;
                }
                _ => break,
            }
        }
        Ok(value)
    }
    fn parse_term(&mut self) -> Result<f64, String> {
        let mut value = self.parse_factor()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.next();
                    value *= self.parse_factor()?;
                }
                Some('/') => {
                    self.next();
                    value /= self.parse_factor()?;
                }
                Some('%') => {
                    self.next();
                    value %= self.parse_factor()?;
                }
                _ => break,
            }
        }
        Ok(value)
    }
    fn parse_factor(&mut self) -> Result<f64, String> {
        let base = self.parse_unary()?;
        self.skip_ws();
        if self.peek() == Some('^') {
            self.next();
            let exponent = self.parse_factor()?;
            // Semantik JS: ^ adalah XOR bitwise (ToInt32).
            let a = base as i64 as i32;
            let b = exponent as i64 as i32;
            return Ok((a ^ b) as f64);
        }
        Ok(base)
    }
    fn parse_unary(&mut self) -> Result<f64, String> {
        self.skip_ws();
        if self.peek() == Some('-') {
            self.next();
            return Ok(-self.parse_unary()?);
        }
        if self.peek() == Some('+') {
            self.next();
            return self.parse_unary();
        }
        self.parse_primary()
    }
    fn parse_primary(&mut self) -> Result<f64, String> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.next();
                let value = self.parse_expr()?;
                self.skip_ws();
                if self.next() != Some(')') {
                    return Err("Unexpected token ')'".into());
                }
                Ok(value)
            }
            Some(c) if c.is_ascii_digit() || c == '.' => {
                let start = self.pos;
                while self
                    .peek()
                    .map(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
                    .unwrap_or(false)
                {
                    // Hentikan exponent bila tanda bukan tepat setelah e/E.
                    // pos bisa 0 bila ekspresi diawali angka — jaga underflow.
                    let prev = if self.pos > 0 {
                        self.chars[self.pos - 1]
                    } else {
                        '\0'
                    };
                    let cur = self.peek().unwrap();
                    if (cur == '+' || cur == '-') && !matches!(prev, 'e' | 'E') {
                        break;
                    }
                    self.pos += 1;
                }
                let text: String = self.chars[start..self.pos].iter().collect();
                text.parse::<f64>()
                    .map_err(|_| format!("Unexpected token '{text}'"))
            }
            Some(c) if c.is_ascii_alphabetic() => {
                // Math.<fn>(args) / Math.PI / Math.E
                let rest: String = self.chars[self.pos..].iter().collect();
                if !rest.starts_with("Math") {
                    return Err(format!("{c} is not defined"));
                }
                self.pos += 4;
                if self.next() != Some('.') {
                    return Err("Unexpected token".into());
                }
                let name_start = self.pos;
                while self.peek().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                    self.pos += 1;
                }
                let name: String = self.chars[name_start..self.pos].iter().collect();
                match name.as_str() {
                    "PI" => Ok(std::f64::consts::PI),
                    "E" => Ok(std::f64::consts::E),
                    "sqrt" | "cos" | "sin" | "tan" | "abs" | "round" | "floor" | "ceil" | "log"
                    | "pow" => {
                        self.skip_ws();
                        if self.next() != Some('(') {
                            return Err("Unexpected token".into());
                        }
                        let mut args = vec![self.parse_expr()?];
                        self.skip_ws();
                        if self.peek() == Some(',') {
                            self.next();
                            args.push(self.parse_expr()?);
                        }
                        self.skip_ws();
                        if self.next() != Some(')') {
                            return Err("Unexpected token ')'".into());
                        }
                        Ok(match name.as_str() {
                            "sqrt" => args[0].sqrt(),
                            "cos" => args[0].cos(),
                            "sin" => args[0].sin(),
                            "tan" => args[0].tan(),
                            "abs" => args[0].abs(),
                            "round" => args[0].round(),
                            "floor" => args[0].floor(),
                            "ceil" => args[0].ceil(),
                            "log" => args[0].ln(),
                            "pow" => args[0].powf(args.get(1).copied().unwrap_or(0.0)),
                            _ => f64::NAN,
                        })
                    }
                    _ => Err(format!("Math.{name} is not a function")),
                }
            }
            _ => Err("Unexpected end of input".into()),
        }
    }
}

fn eval_expression(clean: &str) -> Result<f64, String> {
    let mut parser = ExprParser {
        chars: clean.chars().collect(),
        pos: 0,
        _marker: std::marker::PhantomData,
    };
    let value = parser.parse_expr()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err("Unexpected token".into());
    }
    Ok(value)
}

// ===== datetime-helper =====

const WEEKDAYS_ID: [&str; 7] = ["Minggu", "Senin", "Selasa", "Rabu", "Kamis", "Jumat", "Sabtu"];
const MONTHS_ID: [&str; 12] = [
    "Januari", "Februari", "Maret", "April", "Mei", "Juni", "Juli", "Agustus", "September",
    "Oktober", "November", "Desember",
];

pub fn get_current_time(args: &Value) -> Value {
    let timezone_in = args["timezone"].as_str().unwrap_or("WIB");
    let selected = match timezone_in.to_lowercase().as_str() {
        "wita" => "Asia/Makassar",
        "wit" => "Asia/Jayapura",
        "utc" | "gmt" => "UTC",
        "tokyo" | "jst" => "Asia/Tokyo",
        _ => "Asia/Jakarta",
    };
    let now = chrono::Utc::now();
    let parts = crate::timeutil::zoned_parts(now, selected);
    let datetime = format!(
        "{}, {} {} {} pukul {:02}.{:02}.{:02}",
        WEEKDAYS_ID[(parts.weekday % 7) as usize],
        parts.day,
        MONTHS_ID[(parts.month.max(1) - 1) as usize],
        parts.year,
        parts.hour,
        parts.minute,
        parts.second
    );
    json!({
        "timezone": timezone_in.to_uppercase(),
        "ianaTimezone": selected,
        "datetime": datetime,
        "iso": crate::timeutil::to_iso_z(&now),
    })
}

pub fn calculate_date_difference(args: &Value) -> Value {
    let target_date = args["target_date"].as_str().unwrap_or("");
    if target_date.is_empty() {
        return err("target_date harus diisi (format YYYY-MM-DD).");
    }
    let parsed = chrono::NaiveDate::parse_from_str(target_date.trim(), "%Y-%m-%d")
        .ok()
        .or_else(|| {
            crate::timeutil::parse_iso_loose(target_date).map(|dt| dt.date_naive())
        });
    let Some(target) = parsed else {
        return err(&format!(
            "Format tanggal \"{target_date}\" tidak valid. Gunakan format YYYY-MM-DD."
        ));
    };
    let today = chrono::Utc::now().date_naive();
    let diff = (target - today).num_days();
    let status = if diff == 0 {
        "hari ini".to_string()
    } else if diff > 0 {
        format!("{diff} hari lagi")
    } else {
        format!("{} hari yang lalu", diff.abs())
    };
    json!({
        "today": today.format("%Y-%m-%d").to_string(),
        "targetDate": target_date,
        "differenceDays": diff,
        "status": status,
    })
}

// ===== task-planner (notes) =====

fn add_user_note(args: &Value, ctx: &ToolContext) -> Value {
    let title = args["title"].as_str().unwrap_or("");
    let content = args["content"].as_str().unwrap_or("");
    let category = args["category"].as_str().unwrap_or("general");
    let idempotency_key = args["idempotency_key"].as_str().unwrap_or("");
    let timezone = args["timezone"].as_str().unwrap_or("Asia/Jakarta");
    match ctx.db.save_user_note(&ctx.user_id, title, content, category, idempotency_key) {
        Ok(saved) => {
            let Some(verified) = ctx.db.get_user_note_by_id(&ctx.user_id, saved["id"].as_i64().unwrap_or(0)) else {
                return err("Gagal menyimpan catatan: Read-after-write catatan gagal.");
            };
            let created = crate::timeutil::format_in_timezone(verified["createdAt"].as_str().unwrap_or(""), timezone);
            let mut note = verified.clone();
            note["createdAtUtc"] = created.as_ref().map(|c| c["isoUtc"].clone()).unwrap_or(Value::Null);
            note["createdAtLocal"] = created.as_ref().map(|c| c["label"].clone()).unwrap_or(Value::Null);
            note["timezone"] = created.as_ref().map(|c| c["timezone"].clone()).unwrap_or(Value::Null);
            json!({
                "success": true,
                "message": format!("Catatan \"{}\" berhasil disimpan ke kategori [{}].", saved["title"].as_str().unwrap_or(""), saved["category"].as_str().unwrap_or("")),
                "note": note,
                "idempotent": saved["idempotent"].as_bool().unwrap_or(false),
                "verified": true,
            })
        }
        Err(e) => err(&format!("Gagal menyimpan catatan: {e}")),
    }
}

fn list_user_notes(args: &Value, ctx: &ToolContext) -> Value {
    let category = args["category"].as_str();
    let notes = ctx.db.get_user_notes(&ctx.user_id, category);
    let mapped: Vec<Value> = notes
        .iter()
        .map(|n| {
            json!({
                "id": n["id"], "title": n["title"], "content": n["content"],
                "category": n["category"], "updatedAt": n["updatedAt"],
                "updatedAtUtc": crate::timeutil::format_in_timezone(n["updatedAt"].as_str().unwrap_or(""), "UTC").map(|c| c["isoUtc"].clone()).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "total": mapped.len(),
        "category": category.unwrap_or("semua"),
        "notes": mapped,
    })
}

fn delete_user_note(args: &Value, ctx: &ToolContext) -> Value {
    let note_id = args["note_id"].as_i64().unwrap_or(0);
    ctx.db.delete_user_note(&ctx.user_id, note_id);
    json!({
        "success": true,
        "message": format!("Catatan dengan ID {note_id} berhasil dihapus."),
    })
}

// ===== task-scheduler =====

fn schedule_task(args: &Value, ctx: &ToolContext) -> Value {
    if ctx.user_id.is_empty() {
        return err("User ID tidak tersedia.");
    }
    let schedule = args["schedule"].as_str().unwrap_or("");
    if schedule.is_empty() {
        return err("schedule wajib diisi.");
    }
    let title_raw = args["title"].as_str().unwrap_or("Pengingat");
    let safe_title: String = title_raw.trim().chars().take(120).collect();
    let safe_title = if safe_title.is_empty() { "Pengingat".to_string() } else { safe_title };
    let description = args["description"].as_str().unwrap_or("");
    let safe_description: String = if description.is_empty() {
        format!("Pengingat: {safe_title}")
    } else {
        description.trim().to_string()
    }
    .chars()
    .take(1000)
    .collect();
    let timezone = args["timezone"].as_str().unwrap_or("Asia/Jakarta");
    let Some(parsed) = crate::scheduler::parse_to_cron_expr(schedule, timezone, chrono::Utc::now()) else {
        return err(&format!(
            "Format jadwal tidak dikenali: \"{schedule}\". Coba: \"setiap Senin jam 8\", \"setiap hari jam 20:00\", \"30 menit lagi\", \"besok jam 9\"."
        ));
    };
    let minute_field = parsed.cron.split_whitespace().next().unwrap_or("").to_string();
    if minute_field == "*" || minute_field.starts_with("*/") {
        let step: i64 = if minute_field == "*" {
            1
        } else {
            minute_field[2..].parse().unwrap_or(0)
        };
        if step < 5 {
            return err(&format!(
                "Jadwal terlalu sering (setiap {step} menit). Interval minimum yang diizinkan adalah 5 menit."
            ));
        }
    }
    if !parsed.run_once && ctx.db.list_scheduled_tasks(&ctx.user_id).len() >= 20 {
        return err("Kamu sudah punya 20 jadwal aktif. Batalkan salah satunya dulu sebelum menambah baru.");
    }
    let task = crate::scheduler::create_scheduled_task(
        &ctx.db,
        crate::scheduler::CreateTask {
            user_id: ctx.user_id.clone(),
            title: safe_title.clone(),
            description: safe_description,
            cron_expr: parsed.cron.clone(),
            human_schedule: parsed.human.clone(),
            run_once: parsed.run_once,
            run_at: parsed.run_at.clone(),
            timezone: parsed.timezone.clone().unwrap_or_else(|| timezone.to_string()),
            idempotency_key: args["idempotency_key"].as_str().unwrap_or("").to_string(),
        },
    );
    match task {
        Some(task) => json!({
            "success": true,
            "message": format!("Pengingat \"{safe_title}\" berhasil dijadwalkan: {}.", parsed.human),
            "task": task,
            "verified": task["verified"].as_bool().unwrap_or(false),
        }),
        None => err("Gagal membuat jadwal: gagal menyimpan task."),
    }
}

fn reschedule_task(args: &Value, ctx: &ToolContext) -> Value {
    if ctx.user_id.is_empty() {
        return err("User ID tidak tersedia.");
    }
    let schedule = args["schedule"].as_str().unwrap_or("");
    if schedule.is_empty() {
        return err("schedule wajib diisi.");
    }
    let timezone = args["timezone"].as_str().unwrap_or("Asia/Jakarta");
    let result = crate::scheduler::reschedule_task(
        &ctx.db,
        args["task_id"].as_i64(),
        &ctx.user_id,
        schedule,
        timezone,
    );
    if result["success"] != json!(true) {
        return err(result["error"].as_str().unwrap_or("Gagal mengubah jadwal."));
    }
    json!({
        "success": true,
        "message": format!("Pengingat \"{}\" berhasil diubah: {}.", result.pointer("/task/title").and_then(|v| v.as_str()).unwrap_or(""), result.pointer("/task/humanSchedule").and_then(|v| v.as_str()).unwrap_or("")),
        "task": result["task"],
        "verified": result["verified"],
    })
}

fn list_scheduled_tasks(ctx: &ToolContext) -> Value {
    if ctx.user_id.is_empty() {
        return err("User ID tidak tersedia.");
    }
    let tasks = ctx.db.list_scheduled_tasks(&ctx.user_id);
    if tasks.is_empty() {
        return json!({ "tasks": [], "message": "Tidak ada jadwal aktif saat ini." });
    }
    let mapped: Vec<Value> = tasks
        .iter()
        .map(|t| {
            json!({
                "id": t["id"],
                "title": t["title"],
                "description": t["description"],
                "schedule": t["human_schedule"],
                "nextRunAtUtc": t["next_run_at_utc"],
                "lastRun": if t["last_run"].is_null() { json!("belum pernah") } else { t["last_run"].clone() },
                "timezone": if t["timezone"].is_null() { json!("Asia/Jakarta") } else { t["timezone"].clone() },
            })
        })
        .collect();
    json!({ "tasks": mapped, "total": mapped.len() })
}

fn cancel_scheduled_task(args: &Value, ctx: &ToolContext) -> Value {
    if ctx.user_id.is_empty() {
        return err("User ID tidak tersedia.");
    }
    let task_id = args["task_id"].as_i64().unwrap_or(0);
    if task_id == 0 {
        return err("task_id wajib diisi.");
    }
    crate::scheduler::cancel_task(&ctx.db, task_id, &ctx.user_id)
}

// ===== self-improvement =====

fn record_self_improvement(args: &Value, ctx: &ToolContext) -> Value {
    let topic = args["topic"].as_str().unwrap_or("General Improvement");
    let summary = args["learning_summary"]
        .as_str()
        .or_else(|| args["summary"].as_str())
        .or_else(|| args["patch_note"].as_str())
        .unwrap_or("");
    let skill = args["skill_affected"].as_str().unwrap_or("core");
    let category = args["category"].as_str().unwrap_or("technical");
    ctx.db.record_self_improvement(&ctx.user_id, topic, summary, skill, category)
}

fn list_self_improvements(args: &Value, ctx: &ToolContext) -> Value {
    let limit = args["limit"].as_i64().unwrap_or(15);
    let lessons = ctx.db.list_self_improvements(&ctx.user_id, limit);
    let mut out = json!({
        "success": true,
        "total": lessons.len(),
        "lessons": lessons,
    });
    if out["total"] == json!(0) {
        out["message"] = json!("Belum ada catatan self-improvement yang tersimpan.");
    }
    out
}

// ===== code-analyzer =====

async fn analyze_code_syntax(args: &Value) -> Value {
    let code = args["code"].as_str().unwrap_or("");
    if code.is_empty() {
        return err("Kode yang dianalisis tidak boleh kosong.");
    }
    let lang = args["language"].as_str().unwrap_or("javascript").to_lowercase();
    let lines: Vec<&str> = code.split('\n').collect();
    let line_count = code.split('\n').count();
    let mut issues: Vec<Value> = Vec::new();
    let mut is_valid = true;
    let mut summary = String::new();
    if lang == "json" {
        match serde_json::from_str::<Value>(code) {
            Ok(_) => summary = "Format JSON valid dan terstruktur dengan benar.".into(),
            Err(e) => {
                is_valid = false;
                let message = format!("{e}");
                issues.push(json!({
                    "type": "SyntaxError",
                    "message": message,
                    "suggestion": "Pastikan tanda petik ganda, koma penutup, dan tanda kurung kurawal sudah seimbang.",
                }));
                summary = format!("JSON tidak valid: {message}");
            }
        }
    } else if lang == "typescript" || lang == "ts" {
        let (ok, message, _) = embedded_check_ts_syntax(code).await;
        if ok {
            summary = "Sintaks dasar TypeScript valid.".into();
        } else {
            is_valid = false;
            issues.push(json!({ "type": "TypeScriptSyntaxError", "message": message }));
            summary = format!("TypeScript tidak valid: {message}");
        }
    } else if lang == "javascript" || lang == "js" {
        let (ok, message, stack) = embedded_check_js_syntax(code).await;
        if ok {
            summary = "Sintaks JavaScript valid tanpa error kompilasi dasar.".into();
        } else {
            is_valid = false;
            let line_num = Regex::new(r"evalmachine\.<anonymous>:(\d+)")
                .unwrap()
                .captures(&stack)
                .and_then(|c| c[1].parse::<usize>().ok());
            let mut issue = json!({
                "type": "SyntaxError",
                "message": message,
                "suggestion": "Periksa tanda kurung, titik koma, kurung kurawal yang belum tertutup, atau penamaan variabel.",
            });
            if let Some(line) = line_num {
                issue["line"] = json!(line);
                if let Some(snippet) = lines.get(line - 1) {
                    issue["snippet"] = json!(snippet.trim());
                }
            }
            issues.push(issue);
            summary = format!(
                "Terdeteksi kesalahan sintaks pada baris {}: {message}",
                line_num.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
            );
        }
    } else {
        let count = |c: char| code.matches(c).count();
        if count('{') != count('}') {
            issues.push(json!({ "type": "BracketMismatch", "message": format!("Jumlah tanda kurung kurawal '{{' ({}) tidak seimbang dengan '}}' ({}).", count('{'), count('}')) }));
        }
        if count('(') != count(')') {
            issues.push(json!({ "type": "ParenMismatch", "message": format!("Jumlah tanda kurung '(' ({}) tidak seimbang dengan ')' ({}).", count('('), count(')')) }));
        }
        if count('[') != count(']') {
            issues.push(json!({ "type": "BracketMismatch", "message": format!("Jumlah tanda kurung siku '[' ({}) tidak seimbang dengan ']' ({}).", count('['), count(']')) }));
        }
        is_valid = issues.is_empty();
        summary = if is_valid {
            format!("Kode {} memiliki keseimbangan struktur yang baik.", lang.to_uppercase())
        } else {
            format!("Ditemukan {} potensi masalah struktur pada kode {}.", issues.len(), lang.to_uppercase())
        };
    }
    if code.contains("var ") && (lang.contains("js") || lang.contains("javascript") || lang.contains("ts")) {
        issues.push(json!({ "type": "BestPracticeWarning", "message": "Ditemukan penggunaan \"var\". Disarankan memakai \"const\" atau \"let\" untuk scope yang lebih aman." }));
    }
    if code.contains("== ") && !code.contains("=== ") {
        issues.push(json!({ "type": "StyleWarning", "message": "Ditemukan operator perbandingan longgar \"==\". Disarankan menggunakan strict equality \"===\". " }));
    }
    let mut out = json!({
        "language": lang,
        "valid": is_valid,
        "lineCount": line_count,
        "charCount": code.chars().count(),
        "summary": summary,
    });
    if !issues.is_empty() {
        out["issues"] = json!(issues);
    }
    out
}

// ===== code-scratchpad =====

async fn run_javascript_code(args: &Value) -> Value {
    let code = args["code"].as_str().unwrap_or("");
    if code.is_empty() {
        return err("Kode JavaScript tidak boleh kosong.");
    }
    if code.chars().count() > 8000 {
        return json!({ "success": false, "error": "[Keamanan Sandbox]: kode terlalu panjang (maks 8000 karakter)." });
    }
    let (safe, reason) = crate::agent::security::validate_safe_code_execution(code);
    if !safe {
        return json!({ "success": false, "error": format!("[Keamanan Sandbox]: {reason}") });
    }
    let run = match run_embedded_snippet(code, &SandboxLimits::default()).await {
        Ok(r) => r,
        Err(e) => {
            return json!({ "success": false, "error": format!("Error saat eksekusi kode: {e}") });
        }
    };
    let marker = "\n__YUKI_RESULT__";
    let mut stdout = run.stdout.clone();
    let mut result = Value::Null;
    if let Some(idx) = stdout.rfind(marker) {
        let payload = stdout[idx + marker.len()..].trim().to_string();
        stdout = stdout[..idx].to_string();
        result = serde_json::from_str(&payload).unwrap_or_else(|_| json!(payload));
        if payload.is_empty() {
            result = Value::Null;
        }
    }
    if run.timed_out {
        return json!({
            "success": false,
            "error": "Error saat eksekusi kode: eksekusi melebihi batas 5 detik.",
            "logs": if stdout.is_empty() { Value::Null } else { json!(stdout) },
            "stdout": stdout, "stderr": run.stderr, "exitCode": 1, "timedOut": true,
            "sandbox": run.sandbox.clone(), "timeoutMs": 5000,
        });
    }
    if run.code != Some(0) {
        let err_text = if run.stderr.trim().is_empty() {
            format!("proses keluar dengan kode {}", run.code.unwrap_or(1))
        } else {
            run.stderr.trim().to_string()
        };
        return json!({
            "success": false,
            "error": format!("Error saat eksekusi kode: {}", err_text.chars().take(500).collect::<String>()),
            "logs": if stdout.is_empty() { Value::Null } else { json!(stdout) },
            "stdout": stdout, "stderr": run.stderr, "exitCode": run.code.unwrap_or(1), "timedOut": false,
            "sandbox": run.sandbox.clone(), "timeoutMs": 5000,
        });
    }
    json!({
        "success": true,
        "result": result,
        "logs": if stdout.is_empty() { Value::Null } else { json!(stdout) },
        "stdout": stdout,
        "stderr": "",
        "exitCode": 0,
        "timedOut": false,
        "sandbox": run.sandbox.clone(),
        "timeoutMs": 5000,
    })
}

// ===== skill-health =====

async fn run_skill_health_check(ctx: &ToolContext) -> Value {
    let registry = skills::skills_health();
    let mut checks: Vec<Value> = Vec::new();
    let calc = calculate_expression(&json!({ "expression": "25% * 200" }));
    checks.push(json!({ "name": "calculator", "status": if calc["result"] == json!(50) { "healthy" } else { "unhealthy" }, "durationMs": 0 }));
    let scratch = run_javascript_code(&json!({ "code": "console.log(\"health\")\nreturn 2 + 2" })).await;
    checks.push(json!({ "name": "code-scratchpad", "status": if scratch["exitCode"] == json!(0) { "healthy" } else { "unhealthy" }, "durationMs": 0 }));
    let analyzer = analyze_code_syntax(&json!({ "code": "const ok = true", "language": "javascript" })).await;
    checks.push(json!({ "name": "code-analyzer", "status": if analyzer["valid"] == json!(true) { "healthy" } else { "unhealthy" }, "durationMs": 0 }));
    let time = get_current_time(&json!({ "timezone": "WIB" }));
    checks.push(json!({ "name": "datetime", "status": if time["iso"].is_string() { "healthy" } else { "unhealthy" }, "durationMs": 0 }));
    let scratch_sel = crate::agent::runner::select_agent_tool_names("Pakai Code Scratchpad untuk jalankan JavaScript");
    let learning_sel = crate::agent::runner::select_agent_tool_names("Pakai Self-Improvement untuk simpan evaluasi");
    let routing_ok = scratch_sel.len() == 1
        && scratch_sel.contains("run_javascript_code")
        && learning_sel.contains("record_self_improvement")
        && !learning_sel.contains("run_skill_health_check");
    checks.push(json!({ "name": "explicit-routing", "status": if routing_ok { "healthy" } else { "unhealthy" }, "durationMs": 0 }));
    let _ = ctx;
    let functional_healthy = checks.iter().all(|c| c["status"] == json!("healthy"));
    let healthy = registry["healthy"].as_bool().unwrap_or(false) && functional_healthy;
    json!({
        "success": healthy,
        "healthy": healthy,
        "skills_count": registry["skills_count"],
        "tools_count": registry["tools_count"],
        "missing_schemas": registry["missing_schemas"],
        "status": if healthy { "healthy" } else { "degraded" },
        "registry_health": {
            "status": if registry["healthy"].as_bool().unwrap_or(false) { "healthy" } else { "unhealthy" },
            "skills_count": registry["skills_count"],
            "tools_count": registry["tools_count"],
            "missing_schemas": registry["missing_schemas"],
        },
        "functional_health": { "status": if functional_healthy { "healthy" } else { "degraded" }, "checks": checks },
        "untested_live_capabilities": ["web-search", "url-reader", "browser-agent", "api-tester", "ryukomik-expert", "scheduler"],
        "note": "Status ini membedakan kesehatan registrasi dari synthetic check. Capability jaringan yang belum dites dilaporkan terpisah, bukan dianggap sehat.",
    })
}

// ===== delegate-task =====

async fn delegate_tasks_tool(args: &Value, ctx: &ToolContext) -> Value {
    if ctx.user_id.is_empty() {
        return err("User ID tidak tersedia.");
    }
    let tasks = args["tasks"].as_array().cloned().unwrap_or_default();
    if tasks.len() < 2 {
        return err("Minimal 2 subtask diperlukan untuk delegation.");
    }
    if tasks.len() > 4 {
        return err("Maksimal 4 subtask untuk delegation.");
    }
    crate::agent::subagent::delegate_tasks(
        &tasks,
        &ctx.user_id,
        args["shared_context"].as_str().unwrap_or(""),
        ctx,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::calculate_expression;
    use serde_json::json;

    fn calc(expr: &str) -> f64 {
        let out = calculate_expression(&json!({ "expression": expr }));
        out["result"]
            .as_f64()
            .unwrap_or_else(|| panic!("ekspresi {expr:?} gagal: {out}"))
    }

    #[test]
    fn kalkulator_angka_di_posisi_nol() {
        // Regresi: panic underflow chars[pos-1] saat ekspresi diawali
        // angka (bug report VPS 2026-11-09, "17*23" -> koneksi gugur).
        assert_eq!(calc("17*23"), 391.0);
        assert_eq!(calc("17 * 23"), 391.0);
        assert_eq!(calc("17×23"), 391.0);
        assert_eq!(calc("2+3*4"), 14.0);
        assert_eq!(calc("(2+3)*4"), 20.0);
        assert_eq!(calc("10/4"), 2.5);
        assert_eq!(calc("-5+3"), -2.0);
    }

    #[test]
    fn kalkulator_eksponen_di_posisi_nol() {
        assert_eq!(calc("1e3+1"), 1001.0);
        assert_eq!(calc("1.5e-2"), 0.015);
    }
}
