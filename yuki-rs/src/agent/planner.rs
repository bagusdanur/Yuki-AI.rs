//! Port lib/agent/planner.js.

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::agent::stores::stable_stringify;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

pub fn is_continuation(text: &str) -> bool {
    re(r"(?i)^(?:tolong\s+)?(?:lanjut(?:kan)?|teruskan|coba lagi|ulangi|retry|yang tadi|masih sama|belum berubah|tetap bug|masih bug|gak berubah|nggak berubah|belum keperbaiki)[.!?\s]*$")
        .is_match(text)
}

pub fn classify_intent(text: &str) -> &'static str {
    let value = text.to_lowercase();
    if re(r"\b(analisis|review|jelaskan|cek saja|jangan ubah|tanpa mengubah)\b").is_match(&value) {
        return "analyze";
    }
    if re(r"\b(perbaiki\w*|benerin\w*|benahi\w*|betulin\w*|fix|bug|error|rusak|gagal|masih sama|belum berubah)\b")
        .is_match(&value)
    {
        return "fix";
    }
    if re(r"\b(refactor|rapikan|optimasi|optimize)\b").is_match(&value) {
        return "refactor";
    }
    if re(r"\b(tambah|tambahkan|fitur|implementasikan)\b").is_match(&value) {
        return "extend";
    }
    if re(r"(?s)\b(buat(?:kan)?|create|generate)\b.{0,60}\b(file|proyek|project|aplikasi|app|script|halaman|component|komponen)\b")
        .is_match(&value)
    {
        return "create";
    }
    "general"
}

fn minimize_tool_allowlist(names: Vec<String>, text: &str, intent: &str) -> Vec<String> {
    let mut selected: Vec<String> = names;
    let value = text.to_lowercase();
    if intent == "analyze" {
        let prefix = re(r"^(?:create_|replace_|patch_|update_|delete_|rollback_|schedule_|reschedule_|cancel_)");
        selected.retain(|n| !prefix.is_match(n));
    }
    if intent == "fix"
        && !re(r"(?s)\b(buat(?:kan)?|create|tambahkan)\b.{0,60}\b(file|component|komponen)\b").is_match(&value)
    {
        selected.retain(|n| n != "create_workspace_file");
    }
    if re(r"\b(jadwal\w*|schedule\w*|reminder\w*|pengingat\w*|ingatkan\w*)\b|\b\d+\s+(?:menit|jam)\s+lagi\b")
        .is_match(&value)
    {
        let desired = if re(r"\b(ubah|ganti|maju|mundur|reschedule)\b").is_match(&value) {
            "reschedule_task"
        } else if re(r"\b(batal|batalkan|cancel|hapus)\b").is_match(&value) {
            "cancel_scheduled_task"
        } else if re(r"\b(list|daftar|tampil|lihat|semua)\b").is_match(&value) {
            "list_scheduled_tasks"
        } else {
            "schedule_task"
        };
        for name in ["schedule_task", "reschedule_task", "list_scheduled_tasks", "cancel_scheduled_task"] {
            if name != desired {
                selected.retain(|n| n != name);
            }
        }
        for name in [
            "add_user_note",
            "list_user_notes",
            "delete_user_note",
            "calculate_expression",
            "calculate_date_difference",
            "get_current_time",
        ] {
            selected.retain(|n| n != name);
        }
    }
    if re(r"\b(catatan|note|todo)\b").is_match(&value) {
        let desired = if re(r"\b(hapus|delete)\b").is_match(&value) {
            "delete_user_note"
        } else if re(r"\b(list|daftar|tampil|lihat|semua)\b").is_match(&value) {
            "list_user_notes"
        } else {
            "add_user_note"
        };
        for name in ["add_user_note", "list_user_notes", "delete_user_note"] {
            if name != desired {
                selected.retain(|n| n != name);
            }
        }
    }
    selected
}

pub fn strategy_hash(tool_name: &str, args: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}:{}", tool_name, stable_stringify(args)));
    let digest = format!("{:x}", hasher.finalize());
    digest[..24].to_string()
}

pub struct RetryGuard {
    attempts: HashMap<String, u32>,
    failed_strategies: HashSet<String>,
    failures: u32,
    max_failures: u32,
    max_attempts_per_tool: u32,
}

impl RetryGuard {
    pub fn new(max_failures: u32, max_attempts_per_tool: u32) -> Self {
        Self {
            attempts: HashMap::new(),
            failed_strategies: HashSet::new(),
            failures: 0,
            max_failures,
            max_attempts_per_tool,
        }
    }

    /// (allowed, hash, reason)
    pub fn inspect(&self, tool_name: &str, args: &Value) -> (bool, String, Option<String>) {
        let hash = strategy_hash(tool_name, args);
        let tool_attempts = *self.attempts.get(tool_name).unwrap_or(&0);
        if self.failed_strategies.contains(&hash) {
            return (false, hash, Some("IDENTICAL_FAILED_STRATEGY".into()));
        }
        if self.failures >= self.max_failures || tool_attempts >= self.max_attempts_per_tool {
            return (false, hash, Some("RETRY_BUDGET_EXHAUSTED".into()));
        }
        (true, hash, None)
    }

    pub fn record(&mut self, tool_name: &str, args: &Value, failed: bool) {
        if failed {
            *self.attempts.entry(tool_name.to_string()).or_insert(0) += 1;
            self.failures += 1;
            self.failed_strategies.insert(strategy_hash(tool_name, args));
        }
    }
}

/// messages: daftar {role, content} (hanya user yang dipakai).
pub fn build_execution_plan(
    messages: &[Value],
    select_tools: &[String],
    has_active_artifact: bool,
    max_steps: i64,
) -> Value {
    let users: Vec<String> = messages
        .iter()
        .filter(|m| m["role"] == json!("user"))
        .filter_map(|m| m["content"].as_str().map(|s| s.to_string()))
        .collect();
    let current = users.last().cloned().unwrap_or_default().trim().to_string();
    let previous = if users.len() >= 2 {
        users[users.len().saturating_sub(3)..users.len() - 1].join("\n")
    } else {
        String::new()
    };
    let continuation = is_continuation(&current);
    let routing_text = if continuation {
        format!("{previous}\n{current}").trim().to_string()
    } else {
        current.clone()
    };
    let mut intent = classify_intent(&current);
    if intent == "general" && continuation {
        intent = classify_intent(&previous);
    }
    if has_active_artifact
        && continuation
        && re(r"(?i)\b(bug|error|rusak|perbaiki|belum berubah|masih sama)\b")
            .is_match(&format!("{previous}{current}"))
    {
        intent = "fix";
    }
    let tool_allowlist = minimize_tool_allowlist(select_tools.to_vec(), &routing_text, intent);
    let mutating = tool_allowlist.iter().any(|name| {
        re(r"^(?:create_|replace_|patch_|update_|delete_|schedule_|reschedule_|cancel_|rollback_)")
            .is_match(name)
    });
    let verification_tools: Vec<String> = tool_allowlist
        .iter()
        .filter(|name| re(r"(?:validate|diff|test)").is_match(name))
        .cloned()
        .collect();
    let mut steps = vec![json!({ "id": "understand", "title": "Memahami tujuan terbaru", "kind": "planning" })];
    if !tool_allowlist.is_empty() {
        steps.push(json!({ "id": "execute", "title": "Menjalankan tool yang relevan", "kind": "execution" }));
    }
    if mutating {
        steps.push(json!({ "id": "verify", "title": "Memverifikasi perubahan dengan bukti", "kind": "verification" }));
    }
    steps.push(json!({ "id": "report", "title": "Menyusun laporan berdasarkan hasil aktual", "kind": "reporting" }));
    let max_steps = max_steps;
    let clamped = if max_steps == 0 { 10 } else { max_steps }.clamp(2, 20);
    json!({
        "version": 1,
        "goal": current.chars().take(2000).collect::<String>(),
        "intent": intent,
        "continuation": continuation,
        "contextSource": if continuation { "explicit_continuation" } else { "latest_request" },
        "toolAllowlist": tool_allowlist,
        "verificationTools": verification_tools,
        "requiresEvidence": mutating,
        "maxSteps": clamped,
        "steps": steps,
    })
}

pub fn validate_execution_plan(plan: &Value) -> Option<String> {
    if plan["version"] != json!(1)
        || plan["goal"].as_str().unwrap_or("").is_empty()
        || !plan["toolAllowlist"].is_array()
        || !plan["steps"].is_array()
    {
        return Some("INVALID_PLAN_SCHEMA".into());
    }
    let name_re = re(r"^[a-z][a-z0-9_]{2,60}$");
    if plan["toolAllowlist"]
        .as_array()
        .map(|a| a.iter().any(|n| n.as_str().map(|s| !name_re.is_match(s)).unwrap_or(true)))
        .unwrap_or(false)
    {
        return Some("INVALID_TOOL_ALLOWLIST".into());
    }
    if plan["steps"].as_array().map(|s| s.len() as i64).unwrap_or(0)
        > plan["maxSteps"].as_i64().unwrap_or(0)
    {
        return Some("PLAN_STEP_BUDGET_EXCEEDED".into());
    }
    None
}
