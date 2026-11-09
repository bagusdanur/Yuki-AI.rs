//! Port lib/agent/tool-result.js + observability.js.

use serde_json::{json, Value};

fn safe_value(value: &Value) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    if let Some(s) = value.as_str() {
        let sanitized = crate::llm::sanitize_output_secrets(s);
        return json!(sanitized.chars().take(16_000).collect::<String>());
    }
    let text = crate::llm::sanitize_output_secrets(&serde_json::to_string(value).unwrap_or_default());
    let clipped: String = text.chars().take(16_000).collect();
    serde_json::from_str(&clipped).unwrap_or_else(|_| json!("[evidence redacted]"))
}

fn push_evidence(list: &mut Vec<Value>, kind: &str, value: Value) {
    if !value.is_null() {
        list.push(json!({ "kind": kind, "value": safe_value(&value) }));
    }
}

/// Port buildEvidence.
pub fn build_evidence(tool_name: &str, data: &Value) -> Vec<Value> {
    let mut evidence = Vec::new();
    if !data.is_object() {
        return evidence;
    }
    let get = |k: &str| data.get(k).cloned().unwrap_or(Value::Null);
    let has = |k: &str| !get(k).is_null();
    if has("path") && (has("hash") || has("after_hash") || has("created") || has("replacements")) {
        push_evidence(
            &mut evidence,
            "file",
            json!({
                "path": get("path"),
                "hash": if has("hash") { get("hash") } else { get("after_hash") },
                "created": get("created").as_bool().unwrap_or(false),
                "snapshotId": get("snapshot_id"),
            }),
        );
    }
    if let Some(files) = data["files"].as_array() {
        if !files.is_empty() {
            let mapped: Vec<Value> = files
                .iter()
                .map(|item| json!({ "path": item["path"], "hash": item["hash"], "valid": item["valid"] }))
                .collect();
            push_evidence(&mut evidence, "file", json!(mapped));
        }
    }
    if has("stdout") || has("stderr") || has("code") || has("exitCode") {
        let exit = if !get("exitCode").is_null() {
            get("exitCode")
        } else if !get("code").is_null() {
            get("code")
        } else {
            json!(0)
        };
        push_evidence(
            &mut evidence,
            "stdout",
            json!({
                "stdout": if has("stdout") { get("stdout") } else { get("logs") },
                "stderr": get("stderr"),
                "exitCode": exit,
                "timedOut": get("timedOut").as_bool().unwrap_or(false),
            }),
        );
    }
    if has("id") || !get("note").is_null() || !get("task").is_null() || has("learning_entry_id") {
        let id = if has("learning_entry_id") {
            get("learning_entry_id")
        } else if !get("note").is_null() {
            data.pointer("/note/id").cloned().unwrap_or(Value::Null)
        } else if !get("task").is_null() {
            data.pointer("/task/id").cloned().unwrap_or(Value::Null)
        } else {
            get("id")
        };
        let timestamp = if has("timestamp") {
            get("timestamp")
        } else if !get("note").is_null() {
            data.pointer("/note/createdAt").cloned().unwrap_or(Value::Null)
        } else if !get("task").is_null() {
            data.pointer("/task/createdAt").cloned().unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        push_evidence(
            &mut evidence,
            "record",
            json!({ "id": id, "type": tool_name, "timestamp": timestamp }),
        );
    }
    if has("status") && has("url") {
        push_evidence(
            &mut evidence,
            "http",
            json!({
                "url": get("url"), "status": get("status"), "ok": get("ok"),
                "durationMs": get("durationMs"), "responseSize": get("responseSize"),
            }),
        );
    }
    if !get("artifact").is_null() {
        push_evidence(
            &mut evidence,
            "artifact",
            json!({
                "id": data.pointer("/artifact/id").cloned().unwrap_or(Value::Null),
                "title": data.pointer("/artifact/title").cloned().unwrap_or(Value::Null),
                "version": data.pointer("/artifact/version").cloned().unwrap_or(Value::Null),
                "validated": get("validated") != json!(false),
            }),
        );
    }
    if let Some(results) = data["results"].as_array() {
        let mapped: Vec<Value> = results
            .iter()
            .map(|item| {
                json!({
                    "title": item["title"], "url": item["url"],
                    "relevance": item["relevance"], "publishedAt": item["publishedAt"],
                })
            })
            .collect();
        push_evidence(&mut evidence, "source", json!(mapped));
    }
    if let Some(comics) = data["comics"].as_array() {
        let mapped: Vec<Value> = comics
            .iter()
            .map(|item| json!({ "title": item["title"], "url": item["url"], "format": item["format"] }))
            .collect();
        push_evidence(&mut evidence, "source", json!(mapped));
    }
    evidence
}

/// Port createToolResult.
pub fn create_tool_result(
    tool_call_id: Option<&str>,
    skill_id: &str,
    tool_name: &str,
    status: &str,
    data: &Value,
    error: Option<Value>,
    evidence: Vec<Value>,
    started_at: &str,
    duration_ms: i64,
) -> Value {
    json!({
        "toolCallId": tool_call_id.map(|s| s.to_string()).unwrap_or_else(|| format!("tool_{}", uuid::Uuid::new_v4())),
        "skillId": skill_id,
        "toolName": tool_name,
        "status": status,
        "data": data,
        "error": error.unwrap_or(Value::Null),
        "evidence": evidence,
        "startedAt": started_at,
        "finishedAt": crate::timeutil::to_iso_z(&chrono::Utc::now()),
        "durationMs": duration_ms,
    })
}

/// Port verifyToolResult -> (verified, reason).
pub fn verify_tool_result(result: &Value) -> (bool, String) {
    if result["status"] != json!("succeeded") {
        let message = result
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .unwrap_or("Tool belum berhasil.")
            .to_string();
        return (false, message);
    }
    let kinds: Vec<&str> = result["evidence"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| e["kind"].as_str()).collect())
        .unwrap_or_default();
    let required: Option<&str> = match result["toolName"].as_str().unwrap_or("") {
        "create_workspace_file" | "replace_workspace_text" | "patch_workspace_files" => Some("file"),
        "run_workspace_tests" | "run_javascript_code" => Some("stdout"),
        "add_user_note" | "schedule_task" | "reschedule_task" | "record_self_improvement" => {
            Some("record")
        }
        "http_api_request" => Some("http"),
        "build_interactive_artifact" | "update_interactive_artifact" | "patch_interactive_artifact" => {
            Some("artifact")
        }
        _ => None,
    };
    if let Some(required) = required {
        if !kinds.contains(&required) {
            return (
                false,
                format!(
                    "Bukti {required} tidak tersedia untuk {}.",
                    result["toolName"].as_str().unwrap_or("")
                ),
            );
        }
    }
    (true, String::new())
}

const ALLOWED_EVENTS: [&str; 13] = [
    "agent_run_started",
    "skill_selected",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_failed",
    "approval_requested",
    "approval_approved",
    "approval_rejected",
    "workflow_resumed",
    "verification_passed",
    "verification_failed",
    "agent_run_completed",
    "agent_run_cancel_requested",
];

/// Port traceEvent (observability.js).
pub fn trace_event(event: &str, meta: &Value) {
    if !ALLOWED_EVENTS.contains(&event) {
        return;
    }
    let mut out = json!({ "event": event, "at": crate::timeutil::to_iso_z(&chrono::Utc::now()) });
    if let Some(map) = meta.as_object() {
        for (key, value) in map {
            let lower = key.to_lowercase();
            if ["content", "prompt", "message", "token", "secret", "password", "authorization", "cookie", "args"]
                .iter()
                .any(|bad| lower.contains(bad))
            {
                continue;
            }
            match value {
                Value::String(s) => {
                    out[key] = json!(s.chars().take(180).collect::<String>());
                }
                Value::Number(_) | Value::Bool(_) => {
                    out[key] = value.clone();
                }
                _ => {}
            }
        }
    }
    eprintln!("{}", serde_json::to_string(&out).unwrap_or_default());
}
