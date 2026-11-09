//! Port run-store.js, workflow-store.js, dynamic-skills.js, dan fungsi
//! notes/artifacts/self-improvements dari memory.js + handler
//! self-improvement — sebagai metode Db tambahan (skema sama persis).

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::Db;
use crate::memory::{contains_injection_pattern, store_user};

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn now_iso() -> String {
    crate::timeutil::to_iso_z(&chrono::Utc::now())
}

fn parse_json(text: Option<String>) -> Value {
    text.and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// JSON.stringify dengan kunci objek terurut rekursif (stable di Node).
pub fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        stable_stringify(&map[*k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(stable_stringify).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

pub fn normalized_input_hash(tool_name: &str, args: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{tool_name}:{}", stable_stringify(args)));
    format!("{:x}", hasher.finalize())
}

const RUN_TERMINAL: [&str; 4] = ["succeeded", "partially_succeeded", "failed", "cancelled"];
const RUN_ACTIVE: [&str; 6] = [
    "queued",
    "planning",
    "running",
    "resuming",
    "verifying",
    "awaiting_approval",
];

fn clean_id(value: &str) -> String {
    value.chars().take(80).collect()
}

fn clean_user(value: &str) -> String {
    value.chars().take(64).collect()
}

impl Db {
    // ===== agent_runs (run-store.js) =====

    fn serialize_run(&self, conn: &rusqlite::Connection, request_id: &str) -> Option<Value> {
        let row = conn
            .query_row(
                "SELECT request_id, user_id, goal, state, result_json, error_code, cancel_requested_at, created_at, updated_at, completed_at FROM agent_runs WHERE request_id = ?",
                params![request_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()
            .ok()??;
        let mut stmt = conn
            .prepare("SELECT payload_json FROM agent_run_steps WHERE request_id = ? ORDER BY sequence, updated_at")
            .ok()?;
        let steps: Vec<Value> = stmt
            .query_map(params![request_id], |r| r.get::<_, String>(0))
            .ok()?
            .filter_map(|r| r.ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect();
        let mut out = json!({
            "requestId": row.0,
            "userId": row.1,
            "goal": row.2,
            "state": row.3,
            "done": RUN_TERMINAL.contains(&row.3.as_str()),
            "steps": steps,
            "result": parse_json(row.4),
            "cancelRequested": row.6.is_some(),
            "createdAt": row.7,
            "updatedAt": row.8,
        });
        if let Some(code) = row.5 {
            out["errorCode"] = json!(code);
        }
        if let Some(done_at) = row.9 {
            out["completedAt"] = json!(done_at);
        }
        Some(out)
    }

    pub fn create_or_get_agent_run(
        &self,
        request_id: &str,
        user_id: &str,
        goal: &str,
    ) -> Option<Value> {
        let request_id = clean_id(request_id);
        let user_id = clean_user(user_id);
        if request_id.is_empty() || user_id.is_empty() {
            return None;
        }
        let conn = self.conn.lock().unwrap();
        let existing_owner: Option<String> = conn
            .query_row(
                "SELECT user_id FROM agent_runs WHERE request_id = ?",
                params![request_id],
                |r| r.get(0),
            )
            .optional()
            .ok()?;
        if let Some(owner) = &existing_owner {
            if *owner != user_id {
                return None;
            }
        }
        let goal: String = goal.chars().take(2000).collect();
        conn.execute(
            "INSERT OR IGNORE INTO agent_runs (request_id, user_id, goal, state) VALUES (?, ?, ?, 'queued')",
            params![request_id, user_id, goal],
        )
        .ok()?;
        drop(conn);
        self.get_agent_run(&request_id, &user_id)
    }

    pub fn get_agent_run(&self, request_id: &str, user_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        let owner: Option<String> = conn
            .query_row(
                "SELECT user_id FROM agent_runs WHERE request_id = ?",
                params![clean_id(request_id)],
                |r| r.get(0),
            )
            .optional()
            .ok()?;
        if owner.as_deref() != Some(clean_user(user_id).as_str()) {
            return None;
        }
        self.serialize_run(&conn, &clean_id(request_id))
    }

    pub fn get_active_agent_run(&self, user_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        let request_id: Option<String> = conn
            .query_row(
                "SELECT request_id FROM agent_runs WHERE user_id = ? AND state IN ('queued','planning','running','resuming','verifying','awaiting_approval') ORDER BY updated_at DESC LIMIT 1",
                params![clean_user(user_id)],
                |r| r.get(0),
            )
            .optional()
            .ok()?;
        let request_id = request_id?;
        self.serialize_run(&conn, &request_id)
    }

    pub fn set_agent_run_state(
        &self,
        request_id: &str,
        user_id: &str,
        state: &str,
        result: Option<&Value>,
        error_code: Option<&str>,
    ) -> Option<Value> {
        let terminal = RUN_TERMINAL.contains(&state);
        let conn = self.conn.lock().unwrap();
        let result_json = result.map(|r| serde_json::to_string(r).unwrap_or_default());
        conn.execute(
            "UPDATE agent_runs SET state = ?, result_json = COALESCE(?, result_json), error_code = ?, updated_at = datetime('now'), completed_at = CASE WHEN ? THEN datetime('now') ELSE NULL END WHERE request_id = ? AND user_id = ?",
            params![state, result_json, error_code, terminal as i64, clean_id(request_id), clean_user(user_id)],
        )
        .ok()?;
        drop(conn);
        self.get_agent_run(request_id, user_id)
    }

    pub fn upsert_agent_run_step(&self, request_id: &str, user_id: &str, step: &Value) -> Option<Value> {
        let run = self.get_agent_run(request_id, user_id)?;
        let run_id = run["requestId"].as_str()?.to_string();
        let status_raw = step["status"].as_str().unwrap_or("");
        let status = if [
            "queued",
            "planning",
            "running",
            "awaiting_approval",
            "resuming",
            "verifying",
            "done",
            "error",
            "cancelled",
        ]
        .contains(&status_raw)
        {
            status_raw
        } else {
            "running"
        };
        let mut payload = json!({
            "id": step["id"].as_str().unwrap_or(""),
            "tool": step["tool"].as_str().unwrap_or("agent_core"),
            "title": step["title"].as_str().unwrap_or("Memproses tugas").chars().take(300).collect::<String>(),
            "status": status,
        });
        if let Some(d) = step["durationMs"].as_f64() {
            payload["durationMs"] = json!(d);
        }
        if let Some(s) = step["skillName"].as_str() {
            payload["skillName"] = json!(s.chars().take(100).collect::<String>());
        }
        if let Some(s) = step["skillTitle"].as_str() {
            payload["skillTitle"] = json!(s.chars().take(100).collect::<String>());
        }
        if let Some(approval_id) = step.pointer("/approval/id").and_then(|v| v.as_str()) {
            payload["approval"] = json!({
                "id": approval_id,
                "reason": step.pointer("/approval/reason").and_then(|v| v.as_str()).unwrap_or("").chars().take(300).collect::<String>(),
                "status": step.pointer("/approval/status").and_then(|v| v.as_str()).unwrap_or("pending"),
            });
        }
        if let Some(evidence) = step["evidence"].as_array() {
            payload["evidence"] = json!(evidence.iter().take(8).cloned().collect::<Vec<_>>());
        }
        let conn = self.conn.lock().unwrap();
        let old_seq: Option<i64> = conn
            .query_row(
                "SELECT sequence FROM agent_run_steps WHERE request_id = ? AND step_id = ?",
                params![run_id, payload["id"].as_str().unwrap_or("")],
                |r| r.get(0),
            )
            .optional()
            .ok()?;
        let sequence = match old_seq {
            Some(s) => s,
            None => conn
                .query_row(
                    "SELECT COALESCE(MAX(sequence), -1) + 1 FROM agent_run_steps WHERE request_id = ?",
                    params![run_id],
                    |r| r.get(0),
                )
                .unwrap_or(0),
        };
        conn.execute(
            "INSERT INTO agent_run_steps (request_id, step_id, sequence, payload_json) VALUES (?, ?, ?, ?) ON CONFLICT(request_id, step_id) DO UPDATE SET payload_json = excluded.payload_json, updated_at = datetime('now')",
            params![run_id, payload["id"].as_str().unwrap_or(""), sequence, serde_json::to_string(&payload).unwrap_or_default()],
        )
        .ok()?;
        drop(conn);
        if status == "awaiting_approval" {
            self.set_agent_run_state(&run_id, user_id, "awaiting_approval", None, None);
        } else if ["planning", "running", "resuming", "verifying"].contains(&status) {
            self.set_agent_run_state(&run_id, user_id, status, None, None);
        }
        Some(payload)
    }

    pub fn request_agent_run_cancellation(&self, request_id: &str, user_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        let changes = conn
            .execute(
                "UPDATE agent_runs SET cancel_requested_at = datetime('now'), state = CASE WHEN state IN ('succeeded','partially_succeeded','failed','cancelled') THEN state ELSE 'cancelled' END, completed_at = CASE WHEN state IN ('succeeded','partially_succeeded','failed') THEN completed_at ELSE datetime('now') END, updated_at = datetime('now') WHERE request_id = ? AND user_id = ?",
                params![clean_id(request_id), clean_user(user_id)],
            )
            .ok()?;
        drop(conn);
        if changes > 0 {
            self.get_agent_run(request_id, user_id)
        } else {
            None
        }
    }

    pub fn is_agent_run_cancellation_requested(&self, request_id: &str, user_id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT cancel_requested_at FROM agent_runs WHERE request_id = ? AND user_id = ?",
            params![clean_id(request_id), clean_user(user_id)],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
        .is_some()
    }

    pub fn claim_agent_run(&self, request_id: &str, user_id: &str, owner: &str, lease_ms: i64) -> bool {
        let now = now_ms();
        let owner: String = owner.chars().take(100).collect();
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agent_runs SET lease_owner = ?, lease_expires_at = ?, state = 'planning', updated_at = datetime('now') WHERE request_id = ? AND user_id = ? AND cancel_requested_at IS NULL AND state IN ('queued','planning','running','resuming','verifying') AND (lease_expires_at IS NULL OR lease_expires_at < ? OR lease_owner = ?)",
            params![owner, now + lease_ms, clean_id(request_id), clean_user(user_id), now, owner],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn recover_interrupted_agent_runs(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE agent_runs SET state = 'failed', error_code = 'PROCESS_RESTARTED', completed_at = datetime('now'), updated_at = datetime('now') WHERE state IN ('planning','running','resuming','verifying')",
            [],
        )
        .unwrap_or(0)
    }

    pub fn delete_user_agent_runs(&self, user_id: &str) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM agent_runs WHERE user_id = ?",
            params![clean_user(user_id)],
        )
        .unwrap_or(0)
    }

    /// Port deleteUserWorkflows (workflow-store.js) — hapus akun Fase 5.
    pub fn delete_user_workflows(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM agent_workflows WHERE user_id = ?",
            params![user_id],
        )
        .ok();
    }

    /// Port deleteDynamicSkillData (dynamic-skills.js) — kolom owner_id.
    pub fn delete_user_dynamic_skills(&self, user_id: &str) {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM agent_skills WHERE owner_id = ?",
            params![crate::memory::store_user(user_id)],
        )
        .ok();
    }

    // ===== agent_workflows (workflow-store.js) =====

    fn workflow_from_row(&self, row: &rusqlite::Row) -> rusqlite::Result<Value> {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "requestId": row.get::<_, String>(1)?,
            "userId": row.get::<_, String>(2)?,
            "state": row.get::<_, String>(3)?,
            "idempotencyKey": row.get::<_, String>(4)?,
            "checkpoint": parse_json(row.get::<_, Option<String>>(5)?),
            "result": parse_json(row.get::<_, Option<String>>(6)?),
            "error": row.get::<_, Option<String>>(7)?,
            "createdAt": row.get::<_, String>(8)?,
            "updatedAt": row.get::<_, String>(9)?,
            "expiresAt": row.get::<_, String>(10)?,
        }))
    }

    pub fn get_workflow(&self, id: &str, user_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, request_id, user_id, state, idempotency_key, checkpoint_json, result_json, error, created_at, updated_at, expires_at FROM agent_workflows WHERE id = ? AND user_id = ?",
            params![id, user_id],
            |r| self.workflow_from_row(r),
        )
        .optional()
        .ok()?
    }

    pub fn prune_expired_workflows(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM agent_workflows WHERE expires_at <= ?",
            params![now_iso()],
        )
        .unwrap_or(0)
    }

    pub fn create_approval_checkpoint(
        &self,
        request_id: &str,
        user_id: &str,
        tool_name: &str,
        args: &Value,
        reason: &str,
        resume: &Value,
    ) -> Value {
        self.prune_expired_workflows();
        let id = crate::auth::random_base64url(18);
        let step_id = resume
            .get("stepId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("step_{}", now_ms()));
        let idempotency_key = format!(
            "{user_id}:{request_id}:{step_id}:{}",
            normalized_input_hash(tool_name, args)
        );
        let existing_id: Option<String> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT id FROM agent_workflows WHERE idempotency_key = ? AND user_id = ? AND state IN ('awaiting_approval','resuming','succeeded','partially_succeeded')",
                params![idempotency_key, user_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        if let Some(existing_id) = existing_id {
            let workflow = self.get_workflow(&existing_id, user_id);
            let state = workflow
                .as_ref()
                .and_then(|w| w["state"].as_str())
                .unwrap_or("")
                .to_string();
            return json!({
                "id": existing_id,
                "workflowId": existing_id,
                "reason": workflow.as_ref().and_then(|w| w.pointer("/checkpoint/reason")).and_then(|v| v.as_str()).unwrap_or(reason),
                "status": if state == "awaiting_approval" { "pending" } else { "approved" },
                "state": state,
            });
        }
        let now = chrono::Utc::now();
        let checkpoint = json!({
            "toolName": tool_name,
            "args": args,
            "reason": reason,
            "stepId": step_id,
            "resume": resume,
        });
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO agent_workflows (id, request_id, user_id, state, idempotency_key, checkpoint_json, created_at, updated_at, expires_at) VALUES (?, ?, ?, 'awaiting_approval', ?, ?, ?, ?, ?)",
            params![
                id,
                request_id,
                user_id,
                idempotency_key,
                serde_json::to_string(&checkpoint).unwrap_or_default(),
                crate::timeutil::to_iso_z(&now),
                crate::timeutil::to_iso_z(&now),
                crate::timeutil::to_iso_z(&(now + chrono::Duration::hours(24))),
            ],
        );
        json!({ "id": id, "workflowId": id, "reason": reason, "status": "pending", "state": "awaiting_approval" })
    }

    pub fn claim_workflow(&self, id: &str, user_id: &str) -> (bool, String, Option<Value>) {
        let workflow = self.get_workflow(id, user_id);
        let Some(workflow) = workflow else {
            return (false, "not_found".into(), None);
        };
        let expires = crate::timeutil::parse_iso_loose(workflow["expiresAt"].as_str().unwrap_or(""));
        if expires.map(|e| e < chrono::Utc::now()).unwrap_or(true) {
            return (false, "not_found".into(), None);
        }
        let state = workflow["state"].as_str().unwrap_or("");
        if ["succeeded", "partially_succeeded", "failed", "cancelled"].contains(&state) {
            return (false, "completed".into(), Some(workflow));
        }
        let conn = self.conn.lock().unwrap();
        let changes = conn
            .execute(
                "UPDATE agent_workflows SET state = 'resuming', updated_at = ? WHERE id = ? AND user_id = ? AND state = 'awaiting_approval'",
                params![now_iso(), id, user_id],
            )
            .unwrap_or(0);
        drop(conn);
        if changes != 1 {
            return (false, "in_progress".into(), self.get_workflow(id, user_id));
        }
        (true, String::new(), self.get_workflow(id, user_id))
    }

    pub fn set_workflow_state(
        &self,
        id: &str,
        user_id: &str,
        state: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Option<Value> {
        const STATES: [&str; 10] = [
            "queued",
            "planning",
            "running",
            "awaiting_approval",
            "resuming",
            "verifying",
            "succeeded",
            "partially_succeeded",
            "failed",
            "cancelled",
        ];
        if !STATES.contains(&state) {
            return None;
        }
        let conn = self.conn.lock().unwrap();
        let result_json = result.map(|r| serde_json::to_string(r).unwrap_or_default());
        conn.execute(
            "UPDATE agent_workflows SET state = ?, result_json = COALESCE(?, result_json), error = ?, updated_at = ? WHERE id = ? AND user_id = ?",
            params![state, result_json, error, now_iso(), id, user_id],
        )
        .ok()?;
        drop(conn);
        self.get_workflow(id, user_id)
    }

    pub fn cancel_workflow(&self, id: &str, user_id: &str) -> Option<Value> {
        let workflow = self.get_workflow(id, user_id)?;
        let state = workflow["state"].as_str().unwrap_or("");
        if !["awaiting_approval", "queued", "planning"].contains(&state) {
            return Some(workflow);
        }
        self.set_workflow_state(id, user_id, "cancelled", None, None)
    }

    pub fn list_pending_workflows(&self, user_id: &str) -> Vec<Value> {
        self.prune_expired_workflows();
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id FROM agent_workflows WHERE user_id = ? AND state = 'awaiting_approval' AND expires_at > ? ORDER BY created_at DESC LIMIT 10",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let ids: Vec<String> = stmt
            .query_map(params![user_id, now_iso()], |r| r.get(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        drop(stmt);
        drop(conn);
        ids.iter()
            .filter_map(|id| self.get_workflow(id, user_id))
            .collect()
    }

    // ===== dynamic skills (dynamic-skills.js) =====

    pub fn list_dynamic_skills(&self, user_id: &str) -> Vec<Value> {
        let owner: String = user_id.chars().take(80).collect();
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, name, version, description, instructions, tool_allowlist_json, input_schema_json, validation_json, status, created_at, updated_at, owner_type, owner_id FROM agent_skills WHERE owner_id = ? ORDER BY updated_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![owner], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, String>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
            ))
        });
        let mut out = Vec::new();
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                out.push(json!({
                    "id": row.0, "name": row.1, "version": row.2, "description": row.3,
                    "instructions": row.4,
                    "toolAllowlist": parse_json(Some(row.5)),
                    "inputSchema": parse_json(Some(row.6)),
                    "validation": parse_json(Some(row.7)),
                    "status": row.8, "createdAt": row.9, "updatedAt": row.10,
                    "owner_type": row.11, "owner_id": row.12,
                }));
            }
        }
        out
    }

    fn dynamic_skill_row(&self, skill_id: &str, user_id: &str) -> Option<Value> {
        let owner: String = user_id.chars().take(80).collect();
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, instructions, tool_allowlist_json, validation_json, status FROM agent_skills WHERE id = ? AND owner_id = ?",
            params![skill_id, owner],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "instructions": r.get::<_, String>(1)?,
                    "toolAllowlist": parse_json(r.get::<_, Option<String>>(2)?),
                    "validation": parse_json(r.get::<_, Option<String>>(3)?),
                    "status": r.get::<_, String>(4)?,
                }))
            },
        )
        .optional()
        .ok()?
    }

    pub fn create_dynamic_skill(&self, user_id: &str, input: &Value) -> Value {
        let owner: String = user_id.chars().take(80).collect();
        let name = clean_skill_name(input["name"].as_str().unwrap_or(""));
        let description: String = input["description"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(300)
            .collect();
        let instructions: String = input["instructions"]
            .as_str()
            .unwrap_or("")
            .trim()
            .chars()
            .take(5000)
            .collect();
        if name.is_empty() || description.is_empty() || instructions.is_empty() {
            return json!({ "success": false, "error": "Nama, deskripsi, dan instruksi skill wajib diisi." });
        }
        let mut tools: Vec<String> = Vec::new();
        if let Some(list) = input["toolAllowlist"].as_array() {
            for t in list {
                if let Some(s) = t.as_str() {
                    if !tools.contains(&s.to_string()) {
                        tools.push(s.to_string());
                    }
                }
            }
        }
        let denied: Vec<String> = tools
            .iter()
            .filter(|t| !DYNAMIC_SKILL_TOOL_CEILING.contains(&t.as_str()))
            .cloned()
            .collect();
        if !denied.is_empty() {
            return json!({ "success": false, "error": format!("Tool di luar ceiling skill dinamis ditolak: {}.", denied.join(", ")) });
        }
        if contains_injection_pattern(&instructions) || contains_injection_pattern(&description) {
            return json!({ "success": false, "error": "Instruksi skill mengandung pola yang tidak diizinkan (format perintah/instruksi tersembunyi)." });
        }
        let id = format!("skill_{}", uuid::Uuid::new_v4());
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO agent_skills (id, owner_id, name, description, instructions, tool_allowlist_json, input_schema_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                owner,
                name,
                description,
                instructions,
                serde_json::to_string(&tools).unwrap_or_default(),
                serde_json::to_string(&input["inputSchema"]).unwrap_or_else(|_| "{}".into()),
            ],
        );
        drop(conn);
        match inserted {
            Ok(_) => {
                let skill = self
                    .list_dynamic_skills(user_id)
                    .into_iter()
                    .find(|s| s["id"] == json!(id));
                json!({ "success": true, "skill": skill })
            }
            Err(e) => json!({ "success": false, "error": e.to_string() }),
        }
    }

    pub fn validate_dynamic_skill(&self, skill_id: &str, user_id: &str) -> Value {
        let owner: String = user_id.chars().take(80).collect();
        let Some(row) = self.dynamic_skill_row(skill_id, user_id) else {
            return json!({ "success": false, "error": "Skill tidak ditemukan." });
        };
        let tools: Vec<String> = row["toolAllowlist"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let denied: Vec<String> = tools
            .iter()
            .filter(|t| !DYNAMIC_SKILL_TOOL_CEILING.contains(&t.as_str()))
            .cloned()
            .collect();
        let forbidden_re = regex::Regex::new(
            r"(?i)(?:child_process|process\.|require\s*\(|import\s+|filesystem|/root|/home|/var/www|pm2|docker|nginx|ssh|systemd|credential|secret|private\s+port)",
        )
        .unwrap();
        let forbidden = forbidden_re.is_match(row["instructions"].as_str().unwrap_or(""));
        let valid = denied.is_empty() && !forbidden;
        let validation = json!({
            "valid": valid,
            "deniedTools": denied,
            "forbiddenInstruction": forbidden,
            "checkedAt": now_iso(),
            "permissionCeiling": DYNAMIC_SKILL_TOOL_CEILING,
        });
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE agent_skills SET validation_json = ?, status = ?, updated_at = datetime('now') WHERE id = ? AND owner_id = ?",
            params![
                serde_json::to_string(&validation).unwrap_or_default(),
                if valid { "pending_approval" } else { "draft" },
                skill_id,
                owner
            ],
        );
        drop(conn);
        let skill = self
            .list_dynamic_skills(user_id)
            .into_iter()
            .find(|s| s["id"] == json!(skill_id));
        json!({ "success": valid, "validation": validation, "skill": skill })
    }

    pub fn approve_dynamic_skill(&self, skill_id: &str, user_id: &str) -> Value {
        let owner: String = user_id.chars().take(80).collect();
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "SELECT id, validation_json FROM agent_skills WHERE id = ? AND owner_id = ? AND status = 'pending_approval'",
                params![skill_id, owner],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((id, validation_json)) = row else {
            return json!({ "success": false, "error": "Skill belum lolos validasi atau bukan milikmu." });
        };
        let validation = parse_json(validation_json);
        if validation["valid"] != json!(true) {
            return json!({ "success": false, "error": "Hasil validasi skill tidak sah." });
        }
        let _ = conn.execute(
            "UPDATE agent_skills SET status = 'active', updated_at = datetime('now') WHERE id = ? AND owner_id = ?",
            params![id, owner],
        );
        drop(conn);
        let skill = self
            .list_dynamic_skills(user_id)
            .into_iter()
            .find(|s| s["id"] == json!(skill_id));
        json!({ "success": true, "skill": skill })
    }

    pub fn disable_dynamic_skill(&self, skill_id: &str, user_id: &str) -> Value {
        let owner: String = user_id.chars().take(80).collect();
        let conn = self.conn.lock().unwrap();
        let changed = conn
            .execute(
                "UPDATE agent_skills SET status = 'disabled', updated_at = datetime('now') WHERE id = ? AND owner_id = ?",
                params![skill_id, owner],
            )
            .unwrap_or(0);
        if changed > 0 {
            json!({ "success": true })
        } else {
            json!({ "success": false, "error": "Skill tidak ditemukan." })
        }
    }

    // ===== user_notes (memory.js) =====

    pub fn save_user_note(
        &self,
        user_id: &str,
        title: &str,
        content: &str,
        category: &str,
        idempotency_key: &str,
    ) -> Result<Value, String> {
        let u = crate::db::uid(user_id);
        let clean_title: String = title.trim().chars().take(150).collect();
        let clean_content: String = content.trim().chars().take(2000).collect();
        let clean_cat: String = category
            .trim()
            .to_lowercase()
            .chars()
            .take(50)
            .collect::<String>();
        let clean_cat = if clean_cat.is_empty() {
            "general".to_string()
        } else {
            clean_cat
        };
        if clean_title.is_empty() || clean_content.is_empty() {
            return Err("Judul dan isi catatan tidak boleh kosong.".into());
        }
        let key = if !idempotency_key.is_empty() {
            idempotency_key.chars().take(120).collect::<String>()
        } else {
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let mut hasher = Sha256::new();
            hasher.update(format!("{u}:{clean_title}:{clean_content}:{clean_cat}:{today}"));
            format!("{:x}", hasher.finalize())
                .chars()
                .take(120)
                .collect()
        };
        let conn = self.conn.lock().unwrap();
        let existing: Option<Value> = conn
            .query_row(
                "SELECT id, title, content, category, created_at, updated_at FROM user_notes WHERE user_id = ? AND idempotency_key = ?",
                params![u, key],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                        "content": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                        "createdAt": r.get::<_, String>(4)?, "updatedAt": r.get::<_, String>(5)?,
                    }))
                },
            )
            .optional()
            .ok()
            .flatten();
        if let Some(mut note) = existing {
            note["idempotent"] = json!(true);
            return Ok(note);
        }
        conn.execute(
            "INSERT INTO user_notes (user_id, title, content, category, idempotency_key, created_at, updated_at) VALUES (?, ?, ?, ?, ?, datetime('now'), datetime('now'))",
            params![u, clean_title, clean_content, clean_cat, key],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        conn.query_row(
            "SELECT id, title, content, category, created_at, updated_at FROM user_notes WHERE id = ? AND user_id = ?",
            params![id, u],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                    "content": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                    "createdAt": r.get::<_, String>(4)?, "updatedAt": r.get::<_, String>(5)?,
                }))
            },
        )
        .map_err(|e| e.to_string())
    }

    pub fn get_user_note_by_id(&self, user_id: &str, note_id: i64) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, title, content, category, created_at, updated_at FROM user_notes WHERE user_id = ? AND id = ?",
            params![crate::db::uid(user_id), note_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                    "content": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                    "createdAt": r.get::<_, String>(4)?, "updatedAt": r.get::<_, String>(5)?,
                }))
            },
        )
        .optional()
        .ok()?
    }

    pub fn get_user_notes(&self, user_id: &str, category: Option<&str>) -> Vec<Value> {
        let conn = self.conn.lock().unwrap();
        let (sql, cat) = match category {
            Some(c) => (
                "SELECT id, title, content, category, created_at, updated_at FROM user_notes WHERE user_id = ? AND category = ? ORDER BY updated_at DESC",
                Some(c.trim().to_lowercase()),
            ),
            None => (
                "SELECT id, title, content, category, created_at, updated_at FROM user_notes WHERE user_id = ? ORDER BY updated_at DESC",
                None,
            ),
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let map = |r: &rusqlite::Row| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "title": r.get::<_, String>(1)?,
                "content": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                "createdAt": r.get::<_, String>(4)?, "updatedAt": r.get::<_, String>(5)?,
            }))
        };
        let rows = match &cat {
            Some(c) => stmt.query_map(params![crate::db::uid(user_id), c], map),
            None => stmt.query_map(params![crate::db::uid(user_id)], map),
        };
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn delete_user_note(&self, user_id: &str, note_id: i64) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "DELETE FROM user_notes WHERE user_id = ? AND id = ?",
            params![crate::db::uid(user_id), note_id],
        );
    }

    // ===== user_artifacts (memory.js) =====

    pub fn save_user_artifact(&self, user_id: &str, artifact: &Value) -> Option<Value> {
        let u = crate::db::uid(user_id);
        let mut art_id: String = artifact["id"].as_str().unwrap_or("").trim().to_string();
        let title: String = artifact["title"]
            .as_str()
            .unwrap_or("Untitled Artifact")
            .trim()
            .to_string();
        let kind: String = artifact["type"].as_str().unwrap_or("html").trim().to_string();
        let content: String = artifact["content"].as_str().unwrap_or("").trim().to_string();
        let patch_note: String = artifact["patch_note"]
            .as_str()
            .or_else(|| artifact["summary"].as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if content.is_empty() {
            return None;
        }
        let conn = self.conn.lock().unwrap();
        let mut existing: Option<(String, i64, String)> = None;
        if !art_id.is_empty() {
            existing = conn
                .query_row(
                    "SELECT id, version, title FROM user_artifacts WHERE id = ? AND user_id = ?",
                    params![art_id, u],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .ok()
                .flatten();
        }
        if existing.is_none() {
            let latest: Option<(String, i64, String)> = conn
                .query_row(
                    "SELECT id, version, title FROM user_artifacts WHERE user_id = ? ORDER BY updated_at DESC LIMIT 1",
                    params![u],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .ok()
                .flatten();
            if let Some(latest) = latest {
                let same = latest.2.to_lowercase().trim() == title.to_lowercase().trim();
                let related = title
                    .to_lowercase()
                    .contains(&latest.2.to_lowercase().chars().take(10).collect::<String>())
                    || latest
                        .2
                        .to_lowercase()
                        .contains(&title.to_lowercase().chars().take(10).collect::<String>());
                if same || related {
                    art_id = latest.0.clone();
                    existing = Some(latest);
                }
            }
        }
        if let Some((id, version, _)) = existing {
            let next = version + 1;
            conn.execute(
                "UPDATE user_artifacts SET title = ?, type = ?, content = ?, version = ?, patch_note = ?, updated_at = datetime('now') WHERE id = ? AND user_id = ?",
                params![title, kind, content, next, patch_note, id, u],
            )
            .ok()?;
            return Some(json!({ "id": id, "title": title, "type": kind, "content": content, "version": next, "patch_note": patch_note }));
        }
        if art_id.is_empty() {
            art_id = format!(
                "art_{}_{}",
                now_ms(),
                crate::auth::random_alnum(4).to_lowercase()
            );
        }
        conn.execute(
            "INSERT INTO user_artifacts (id, user_id, title, type, content, version, patch_note) VALUES (?, ?, ?, ?, ?, 1, ?)",
            params![art_id, u, title, kind, content, patch_note],
        )
        .ok()?;
        Some(json!({ "id": art_id, "title": title, "type": kind, "content": content, "version": 1, "patch_note": patch_note }))
    }

    pub fn get_latest_user_artifact(&self, user_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, title, type, content, version, patch_note, created_at, updated_at FROM user_artifacts WHERE user_id = ? ORDER BY updated_at DESC LIMIT 1",
            params![crate::db::uid(user_id)],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?,
                    "type": r.get::<_, String>(2)?, "content": r.get::<_, String>(3)?,
                    "version": r.get::<_, i64>(4)?, "patch_note": r.get::<_, Option<String>>(5)?,
                    "created_at": r.get::<_, String>(6)?, "updated_at": r.get::<_, String>(7)?,
                }))
            },
        )
        .optional()
        .ok()?
    }

    pub fn get_user_artifact_by_id(&self, user_id: &str, artifact_id: &str) -> Option<Value> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, title, type, content, version, patch_note, created_at, updated_at FROM user_artifacts WHERE user_id = ? AND id = ?",
            params![crate::db::uid(user_id), artifact_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?,
                    "type": r.get::<_, String>(2)?, "content": r.get::<_, String>(3)?,
                    "version": r.get::<_, i64>(4)?, "patch_note": r.get::<_, Option<String>>(5)?,
                    "created_at": r.get::<_, String>(6)?, "updated_at": r.get::<_, String>(7)?,
                }))
            },
        )
        .optional()
        .ok()?
    }

    // ===== self_improvements (handler self-improvement) =====

    pub fn record_self_improvement(
        &self,
        user_id: &str,
        topic: &str,
        summary: &str,
        skill: &str,
        category: &str,
    ) -> Value {
        let safe_user: String = user_id.chars().take(80).collect();
        let clean_topic: String =
            crate::llm::sanitize_output_secrets(topic.trim()).chars().take(160).collect();
        let clean_topic = if clean_topic.is_empty() {
            "General Improvement".to_string()
        } else {
            clean_topic
        };
        let clean_summary: String = crate::llm::sanitize_output_secrets(summary.trim())
            .chars()
            .take(2400)
            .collect();
        let clean_skill: String = crate::llm::sanitize_output_secrets(skill.trim())
            .chars()
            .take(100)
            .collect();
        let clean_skill = if clean_skill.is_empty() {
            "core".to_string()
        } else {
            clean_skill
        };
        let category: String = crate::llm::sanitize_output_secrets(category.trim())
            .chars()
            .take(60)
            .collect();
        let category = if category.is_empty() {
            "technical".to_string()
        } else {
            category
        };
        if clean_summary.is_empty() {
            return json!({ "success": false, "error": "Parameter \"learning_summary\" tidak boleh kosong." });
        }
        let id = format!("imp_{}", uuid::Uuid::new_v4());
        let timestamp = now_iso();
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO self_improvements (id, user_id, topic, skill, category, scope, summary, created_at) VALUES (?, ?, ?, ?, ?, 'user', ?, ?)",
            params![id, safe_user, clean_topic, clean_skill, category, clean_summary, timestamp],
        );
        if let Err(e) = inserted {
            return json!({ "success": false, "error": format!("Gagal menyimpan self-improvement: {e}") });
        }
        let readback: Option<Value> = conn
            .query_row(
                "SELECT id, topic, skill, category, scope, summary, created_at FROM self_improvements WHERE id = ? AND user_id = ?",
                params![id, safe_user],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "topic": r.get::<_, String>(1)?,
                        "skill": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                        "scope": r.get::<_, String>(4)?, "summary": r.get::<_, String>(5)?,
                        "timestamp": r.get::<_, String>(6)?,
                    }))
                },
            )
            .optional()
            .ok()
            .flatten();
        match readback {
            Some(entry) => json!({
                "success": true,
                "learning_entry_id": id,
                "category": category,
                "scope": "user",
                "timestamp": timestamp,
                "verified": true,
                "entry": entry,
                "message": format!("💾 Self-improvement: Wawasan terverifikasi tersimpan — [{clean_skill}] {clean_topic}"),
            }),
            None => json!({ "success": false, "error": "Gagal menyimpan self-improvement: Read-after-write gagal; entry tidak ditemukan." }),
        }
    }

    pub fn list_self_improvements(&self, user_id: &str, limit: i64) -> Vec<Value> {
        let safe_user: String = user_id.chars().take(80).collect();
        let limit = limit.clamp(1, 50);
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id, topic, skill, category, scope, summary, created_at FROM self_improvements WHERE user_id = ? ORDER BY created_at DESC LIMIT ?",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![safe_user, limit], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "topic": r.get::<_, String>(1)?,
                "skill": r.get::<_, String>(2)?, "category": r.get::<_, String>(3)?,
                "scope": r.get::<_, String>(4)?, "summary": r.get::<_, String>(5)?,
                "timestamp": r.get::<_, String>(6)?,
            }))
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn recent_self_improvements(&self, user_id: &str, limit: i64) -> Vec<Value> {
        self.list_self_improvements(user_id, limit.clamp(1, 12))
    }
}

pub const DYNAMIC_SKILL_TOOL_CEILING: [&str; 7] = [
    "safe_calculate",
    "get_current_time",
    "list_user_notes",
    "list_scheduled_tasks",
    "list_self_improvements",
    "search_ryukomik",
    "get_latest_ryukomik",
];

fn clean_skill_name(value: &str) -> String {
    let lowered = value.trim().to_lowercase();
    let replaced: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Node: replace [^a-z0-9_-]+ -> '-', lalu strip '-' di tepi, potong 60.
    let mut collapsed = String::new();
    let mut last_dash = false;
    for c in replaced.chars() {
        if c == '-' {
            if last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        collapsed.push(c);
    }
    collapsed
        .trim_matches('-')
        .chars()
        .take(60)
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn summarize_approval_args(args: &Value) -> Value {
    let mut pick = json!({});
    for key in ["path", "url", "method", "title", "id", "term", "query"] {
        if let Some(v) = args.get(key) {
            if !v.is_null() && v != &json!("") {
                let text = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                pick[key] = json!(text.chars().take(160).collect::<String>());
            }
        }
    }
    if let Some(patches) = args["patches"].as_array() {
        let files: Vec<Value> = patches
            .iter()
            .filter_map(|p| p["path"].as_str().map(|s| json!(s)))
            .take(10)
            .collect();
        pick["files"] = json!(files);
    }
    if let Some(html) = args["html_content"].as_str() {
        pick["htmlChars"] = json!(html.chars().count());
    }
    if let Some(code) = args["code"].as_str() {
        pick["codeChars"] = json!(code.chars().count());
    }
    if let Some(schedule) = args["schedule"].as_str() {
        pick["schedule"] = json!(schedule.chars().take(80).collect::<String>());
    }
    pick
}

// store_user dipakai memory.rs untuk tabel agent; diekspos ulang di sini
// agar modul agent tidak bergantung pada detail slicing memory.
pub fn agent_store_user(user_id: &str) -> String {
    store_user(user_id)
}
