//! Route agent (Fase 4): katalog skill, dynamic skills, progress/run,
//! workspace import, workflow approvals — port dari server.js.
//! Jalur chat mode agent ada di `agent_chat` (dipanggil routes/chat.rs).

use std::sync::{Arc, OnceLock};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use regex::Regex;
use serde_json::{json, Value};

use crate::agent::runner::{run_agent, AgentDeps, RunAgentArgs};
use crate::agent::skills;
use crate::agent::stores::summarize_approval_args;
use crate::agent::toolresult::trace_event;
use crate::agent::tools::{execute_tool, ToolContext};
use crate::agent::tools_workspace::import_workspace_files;
use crate::auth::{check_rate_limit, SessionUser};
use crate::llm::{allowed_emotions, feeling_for};
use crate::state::AppState;

fn err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn ok(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn request_id_valid(value: &str) -> bool {
    Regex::new(r"^[a-zA-Z0-9_-]{8,80}$").unwrap().is_match(value)
}

pub fn agent_worker_id() -> &'static str {
    static WORKER: OnceLock<String> = OnceLock::new();
    WORKER.get_or_init(|| format!("{}:{}", std::process::id(), uuid::Uuid::new_v4()))
}

fn deps(state: &AppState) -> AgentDeps {
    AgentDeps {
        db: state.db.clone(),
        http: state.http.clone(),
        llm: state.llm.clone(),
    }
}

// ===== Katalog skill (tanpa sesi) =====

pub async fn list_skills_route(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    // Tanpa sesi: bucket rate-limit per IP seperti Node (req.ip).
    let ip = crate::routes::auth_routes::client_ip(&headers);
    if let Err(res) = check_rate_limit(&state, &ip, "/api/agent/skills", 60_000, 60) {
        return res;
    }
    ok(json!({ "skills": skills::list_skills() }))
}

// ===== Dynamic skills =====

pub async fn list_dynamic_skills_route(
    State(state): State<AppState>,
    user: SessionUser,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agent/dynamic-skills", 60_000, 60) {
        return res;
    }
    ok(json!({ "skills": state.db.list_dynamic_skills(&user.0) }))
}

pub async fn create_dynamic_skill_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agent/dynamic-skills", 60_000, 10) {
        return res;
    }
    let result = state.db.create_dynamic_skill(&user.0, &body);
    if result["success"] != json!(true) {
        return err(
            StatusCode::BAD_REQUEST,
            result["error"].as_str().unwrap_or("Skill gagal dibuat."),
        );
    }
    (StatusCode::CREATED, Json(result)).into_response()
}

pub async fn validate_dynamic_skill_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(skill_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/dynamic-skills/{skill_id}/validate"), 60_000, 20) {
        return res;
    }
    let result = state.db.validate_dynamic_skill(&skill_id, &user.0);
    if result["success"] != json!(true) {
        let mut body = json!({
            "error": result["error"].as_str().unwrap_or("Skill ditolak oleh permission ceiling."),
        });
        if result.get("validation").is_some() {
            body["validation"] = result["validation"].clone();
        }
        return (StatusCode::BAD_REQUEST, Json(body)).into_response();
    }
    ok(result)
}

pub async fn approve_dynamic_skill_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(skill_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/dynamic-skills/{skill_id}/approve"), 60_000, 10) {
        return res;
    }
    let result = state.db.approve_dynamic_skill(&skill_id, &user.0);
    if result["success"] != json!(true) {
        return err(
            StatusCode::BAD_REQUEST,
            result["error"].as_str().unwrap_or("Skill gagal disetujui."),
        );
    }
    ok(result)
}

pub async fn disable_dynamic_skill_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(skill_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/dynamic-skills/{skill_id}/disable"), 60_000, 20) {
        return res;
    }
    let result = state.db.disable_dynamic_skill(&skill_id, &user.0);
    if result["success"] != json!(true) {
        return err(
            StatusCode::NOT_FOUND,
            result["error"].as_str().unwrap_or("Skill tidak ditemukan."),
        );
    }
    ok(result)
}

// ===== Progress & runs =====

pub async fn progress_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(request_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/progress/{request_id}"), 60_000, 240) {
        return res;
    }
    if !request_id_valid(&request_id) {
        return err(StatusCode::BAD_REQUEST, "Request ID tidak valid.");
    }
    match state.db.get_agent_run(&request_id, &user.0) {
        Some(run) => ok(run),
        None => ok(json!({ "steps": [], "done": false, "state": "queued" })),
    }
}

pub async fn runs_active_route(State(state): State<AppState>, user: SessionUser) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agent/runs/active", 60_000, 120) {
        return res;
    }
    let run = state
        .db
        .get_active_agent_run(&user.0)
        .unwrap_or(Value::Null);
    ok(json!({ "run": run }))
}

pub async fn cancel_run_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(request_id): Path<String>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/runs/{request_id}/cancel"), 60_000, 20) {
        return res;
    }
    if !request_id_valid(&request_id) {
        return err(StatusCode::BAD_REQUEST, "Request ID tidak valid.");
    }
    let Some(run) = state.db.request_agent_run_cancellation(&request_id, &user.0) else {
        return err(StatusCode::NOT_FOUND, "Run tidak ditemukan.");
    };
    trace_event(
        "agent_run_cancel_requested",
        &json!({ "requestId": request_id, "userId": user.0 }),
    );
    ok(json!({ "success": true, "run": run }))
}

// ===== Workspace import =====

pub async fn workspace_import_route(
    State(state): State<AppState>,
    user: SessionUser,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agent/workspace/import", 60_000, 10) {
        return res;
    }
    let result = import_workspace_files(&body, &user.0);
    if result["success"] != json!(true) {
        return err(
            StatusCode::BAD_REQUEST,
            result["error"].as_str().unwrap_or("Import workspace gagal."),
        );
    }
    ok(result)
}

// ===== Workflows & approvals =====

pub async fn workflows_pending_route(
    State(state): State<AppState>,
    user: SessionUser,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, "/api/agent/workflows/pending", 60_000, 60) {
        return res;
    }
    let workflows: Vec<Value> = state
        .db
        .list_pending_workflows(&user.0)
        .iter()
        .map(|item| {
            let checkpoint = &item["checkpoint"];
            let mut out = json!({
                "id": item["id"],
                "requestId": item["requestId"],
                "state": item["state"],
                "createdAt": item["createdAt"],
                "expiresAt": item["expiresAt"],
                "args": summarize_approval_args(&checkpoint["args"]),
            });
            for (key, field) in [
                ("toolName", "toolName"),
                ("reason", "reason"),
                ("stepId", "stepId"),
            ] {
                if !checkpoint[field].is_null() {
                    out[key] = checkpoint[field].clone();
                }
            }
            out
        })
        .collect();
    ok(json!({ "workflows": workflows }))
}

pub async fn approvals_route(
    State(state): State<AppState>,
    user: SessionUser,
    Path(approval_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Err(res) = check_rate_limit(&state, &user.0, &format!("/api/agent/approvals/{approval_id}"), 60_000, 20) {
        return res;
    }
    let user_id = user.0.clone();
    let db = state.db.clone();
    let Some(existing) = db.get_workflow(&approval_id, &user_id) else {
        return err(
            StatusCode::NOT_FOUND,
            "Approval tidak ditemukan atau sudah kedaluwarsa.",
        );
    };
    if body["decision"] != json!("approve") {
        db.cancel_workflow(&approval_id, &user_id);
        if let Some(request_id) = existing["requestId"].as_str() {
            if !request_id.is_empty() {
                db.set_agent_run_state(request_id, &user_id, "cancelled", None, None);
            }
        }
        trace_event(
            "approval_rejected",
            &json!({ "workflowId": approval_id, "requestId": existing["requestId"].clone() }),
        );
        return ok(json!({
            "success": true, "status": "rejected", "state": "cancelled",
            "workflowId": approval_id,
        }));
    }
    let (claimed_ok, reason, claimed_workflow) = db.claim_workflow(&approval_id, &user_id);
    if !claimed_ok {
        if reason == "completed" {
            let workflow = claimed_workflow.unwrap_or(Value::Null);
            return ok(json!({
                "success": true, "status": "approved", "duplicate": true,
                "workflowId": approval_id,
                "state": workflow["state"].clone(),
                "result": workflow["result"].clone(),
            }));
        }
        if reason == "in_progress" {
            return err(
                StatusCode::CONFLICT,
                "Workflow sedang dilanjutkan. Permintaan kedua tidak menjalankan tool lagi.",
            );
        }
        return err(
            StatusCode::NOT_FOUND,
            "Approval tidak ditemukan atau kedaluwarsa.",
        );
    }
    let workflow = claimed_workflow.unwrap_or(Value::Null);
    let checkpoint = workflow["checkpoint"].clone();
    let request_id = workflow["requestId"].as_str().unwrap_or("").to_string();
    trace_event(
        "approval_approved",
        &json!({
            "workflowId": approval_id, "requestId": request_id,
            "toolName": checkpoint["toolName"].clone(),
        }),
    );
    let resume = checkpoint["resume"].clone();
    let username = resume["username"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| db.username(&user_id))
        .unwrap_or_else(|| "User".to_string());
    let mut ctx = ToolContext::new(db.clone(), state.http.clone(), state.llm.clone(), &user_id);
    ctx.username = username.clone();
    ctx.tool_call_id = checkpoint["stepId"].as_str().map(|s| s.to_string());
    let tool_result = execute_tool(
        checkpoint["toolName"].as_str().unwrap_or(""),
        &checkpoint["args"],
        &ctx,
    )
    .await;
    if let Some(error) = tool_result["error"].as_str() {
        let error = error.to_string();
        let result = json!({
            "tool": checkpoint["toolName"].clone(),
            "contract": tool_result["contract"].clone(),
        });
        db.set_workflow_state(&approval_id, &user_id, "failed", Some(&result), Some(&error));
        trace_event(
            "tool_call_failed",
            &json!({
                "workflowId": approval_id, "requestId": request_id,
                "toolName": checkpoint["toolName"].clone(),
            }),
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": error, "workflowId": approval_id, "state": "failed" })),
        )
            .into_response();
    }
    trace_event(
        "workflow_resumed",
        &json!({
            "workflowId": approval_id, "requestId": request_id,
            "toolName": checkpoint["toolName"].clone(),
        }),
    );
    if !request_id.is_empty() {
        db.set_agent_run_state(&request_id, &user_id, "resuming", None, None);
    }
    let mut approved_tool = json!({
        "toolName": checkpoint["toolName"].clone(),
        "args": checkpoint["args"].clone(),
        "stepId": checkpoint["stepId"].clone(),
        "durationMs": tool_result["durationMs"].clone(),
        "evidence": tool_result["evidence"].clone(),
        "contract": tool_result["contract"].clone(),
    });
    if !tool_result["data"].is_null() {
        approved_tool["data"] = tool_result["data"].clone();
    }
    if !tool_result["skillName"].is_null() {
        approved_tool["skillName"] = tool_result["skillName"].clone();
    }
    if !tool_result["skillTitle"].is_null() {
        approved_tool["skillTitle"] = tool_result["skillTitle"].clone();
    }
    let resume_messages: Vec<Value> = resume["messages"].as_array().cloned().unwrap_or_default();
    let resume_memory = resume["memoryContext"].as_str().unwrap_or("").to_string();
    let resume_bond = resume["bondName"].as_str().unwrap_or("mulai terbiasa").to_string();
    let db_progress = db.clone();
    let uid_progress = user_id.clone();
    let rid_progress = request_id.clone();
    let on_progress: crate::agent::runner::ProgressFn = Arc::new(move |step: Value| {
        if !rid_progress.is_empty() {
            db_progress.upsert_agent_run_step(&rid_progress, &uid_progress, &step);
        }
    });
    let db_cancel = db.clone();
    let uid_cancel = user_id.clone();
    let rid_cancel = request_id.clone();
    let should_cancel: crate::agent::runner::CancelFn = Arc::new(move || {
        !rid_cancel.is_empty() && db_cancel.is_agent_run_cancellation_requested(&rid_cancel, &uid_cancel)
    });
    let db_approval = db.clone();
    let uid_approval = user_id.clone();
    let rid_approval = request_id.clone();
    let resume_base = resume.clone();
    let on_approval: crate::agent::runner::ApprovalFn = Arc::new(move |request: Value| {
        let mut next_resume = resume_base.clone();
        if next_resume.is_object() {
            next_resume["stepId"] = request["stepId"].clone();
        }
        let checkpoint = db_approval.create_approval_checkpoint(
            &rid_approval,
            &uid_approval,
            request["toolName"].as_str().unwrap_or(""),
            &request["args"],
            request["reason"].as_str().unwrap_or(""),
            &next_resume,
        );
        Some(checkpoint)
    });
    let run_args = RunAgentArgs {
        user_id: user_id.clone(),
        username: resume["username"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| username.clone()),
        messages: resume_messages,
        memory_context: resume_memory,
        bond_name: resume_bond,
        max_turns: 6,
        resume: Some(json!({ "approvedTool": approved_tool })),
    };
    let agent_result = match run_agent(&deps(&state), run_args, on_progress, on_approval, should_cancel).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("[approvals] runAgent gagal: {e}");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Terjadi kesalahan internal pada server.",
            );
        }
    };
    let run_state = agent_result["workflowState"].as_str().unwrap_or("succeeded").to_string();
    let message_id = db.save_chat_message(&user_id, "assistant", agent_result["reply"].as_str().unwrap_or(""));
    db.set_workflow_state(&approval_id, &user_id, &run_state, Some(&agent_result), None);
    if !request_id.is_empty() {
        db.set_agent_run_state(&request_id, &user_id, &run_state, Some(&agent_result), None);
    }
    trace_event(
        "agent_run_completed",
        &json!({ "workflowId": approval_id, "requestId": request_id, "state": run_state }),
    );
    let mut response = json!({
        "success": true, "status": "approved", "workflowId": approval_id,
        "state": run_state, "messageId": message_id,
    });
    if let Some(obj) = agent_result.as_object() {
        for (key, value) in obj {
            response[key] = value.clone();
        }
    }
    ok(response)
}

// ===== Chat mode agent (dipanggil dari routes/chat.rs) =====

#[allow(clippy::too_many_arguments)]
pub async fn agent_chat(
    state: &AppState,
    user_id: &str,
    body: &Value,
    messages_to_send: &[Value],
    user_text: &str,
    memory_context: &str,
    bond_name: &str,
    bond_value: f64,
    kw_mood: &str,
    emotion_state: Value,
) -> Response {
    let db = state.db.clone();
    let raw_request_id = body["requestId"].as_str().unwrap_or("");
    let request_id = if request_id_valid(raw_request_id) {
        raw_request_id.to_string()
    } else {
        String::new()
    };
    let username = db
        .username(user_id)
        .or_else(|| body["username"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "User".to_string());

    if !request_id.is_empty() {
        let run = db.create_or_get_agent_run(&request_id, user_id, user_text);
        let Some(run) = run else {
            return err(StatusCode::CONFLICT, "Request ID sudah dipakai.");
        };
        if run["done"] == json!(true) && !run["result"].is_null() {
            return ok(run["result"].clone());
        }
        if !db.claim_agent_run(&request_id, user_id, agent_worker_id(), 180_000) {
            return err(
                StatusCode::CONFLICT,
                "Run ini sedang diproses atau sudah dibatalkan.",
            );
        }
    }
    trace_event(
        "agent_run_started",
        &json!({ "requestId": request_id, "userId": user_id, "mode": "agent" }),
    );

    let db_progress = db.clone();
    let uid_progress = user_id.to_string();
    let rid_progress = request_id.clone();
    let on_progress: crate::agent::runner::ProgressFn = Arc::new(move |step: Value| {
        if !rid_progress.is_empty() {
            db_progress.upsert_agent_run_step(&rid_progress, &uid_progress, &step);
        }
    });
    let db_cancel = db.clone();
    let uid_cancel = user_id.to_string();
    let rid_cancel = request_id.clone();
    let should_cancel: crate::agent::runner::CancelFn = Arc::new(move || {
        !rid_cancel.is_empty() && db_cancel.is_agent_run_cancellation_requested(&rid_cancel, &uid_cancel)
    });
    let db_approval = db.clone();
    let uid_approval = user_id.to_string();
    let rid_approval = request_id.clone();
    let approval_messages = messages_to_send.to_vec();
    let approval_memory = memory_context.to_string();
    let approval_bond = bond_name.to_string();
    let approval_username = username.clone();
    let on_approval: crate::agent::runner::ApprovalFn = Arc::new(move |request: Value| {
        let checkpoint_request_id = if rid_approval.is_empty() {
            format!("agent_{}", uuid::Uuid::new_v4().simple())
        } else {
            rid_approval.clone()
        };
        let resume = json!({
            "messages": approval_messages,
            "memoryContext": approval_memory,
            "bondName": approval_bond,
            "bondValue": bond_value,
            "username": approval_username,
            "stepId": request["stepId"].clone(),
        });
        let checkpoint = db_approval.create_approval_checkpoint(
            &checkpoint_request_id,
            &uid_approval,
            request["toolName"].as_str().unwrap_or(""),
            &request["args"],
            request["reason"].as_str().unwrap_or(""),
            &resume,
        );
        trace_event(
            "approval_requested",
            &json!({
                "workflowId": checkpoint["workflowId"].clone(),
                "requestId": checkpoint_request_id,
                "toolName": request["toolName"].clone(),
            }),
        );
        Some(checkpoint)
    });

    let run_args = RunAgentArgs {
        user_id: user_id.to_string(),
        username: username.clone(),
        messages: messages_to_send.to_vec(),
        memory_context: memory_context.to_string(),
        bond_name: bond_name.to_string(),
        max_turns: 6,
        resume: None,
    };
    let agent_result = match run_agent(&deps(state), run_args, on_progress, on_approval, should_cancel).await {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("[chat agent] runAgent gagal: {e}");
            state
                .metrics
                .chat_failures
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if !request_id.is_empty() {
                db.set_agent_run_state(&request_id, user_id, "failed", None, Some("RUN_FAILED"));
            }
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Terjadi kesalahan internal pada server.",
            );
        }
    };

    let mut agent_mood = agent_result["mood"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(kw_mood)
        .to_string();
    if !allowed_emotions(bond_name).contains(&agent_mood.as_str()) {
        agent_mood = "tenang".to_string();
    }

    // Fase 5: cabang agent Node menutup dengan updateFromLLMMood +
    // saveEmotion atas objek emosi yang sudah bereaksi di chat.rs.
    {
        let mut emotion = crate::emotion::Emotion::from_state(Some(emotion_state));
        emotion.update_from_llm_mood(&agent_mood);
        db.save_emotion(user_id, &emotion.serialize());
    }

    let user_message_id = db.save_chat_message(user_id, "user", user_text);
    crate::memory::capture_memory_candidates(&db, user_id, user_text, Some(user_message_id));
    let message_id = db.save_chat_message(
        user_id,
        "assistant",
        agent_result["reply"].as_str().unwrap_or(""),
    );

    // extractFacts -> addFacts di latar (fire-and-forget seperti Node),
    // digerbang worthRemembering sama seperti server.js.
    if crate::memory::worth_remembering(user_text) {
        let (db2, llm2, embedder2) = (db.clone(), state.llm.clone(), state.embedder.clone());
        let (uid2, text2, reply2) = (
            user_id.to_string(),
            user_text.to_string(),
            agent_result["reply"].as_str().unwrap_or("").to_string(),
        );
        tokio::spawn(async move {
            let facts = llm2.extract_facts(&text2, &reply2).await;
            if !facts.is_empty() {
                crate::memory::add_facts(&db2, &embedder2, &uid2, &facts).await;
            }
        });
    }

    let milestones = db.sync_bond_milestones(user_id, bond_value);
    let steps = if !request_id.is_empty() {
        db.get_agent_run(&request_id, user_id)
            .map(|run| run["steps"].clone())
            .filter(|s| s.is_array())
            .unwrap_or_else(|| agent_result["steps"].clone())
    } else {
        agent_result["steps"].clone()
    };
    let workflow_state = agent_result["workflowState"]
        .as_str()
        .unwrap_or("succeeded")
        .to_string();
    let payload = json!({
        "reply": agent_result["reply"].clone(),
        "mood": agent_mood,
        "bond": bond_name,
        "bondValue": bond_value,
        "feeling": feeling_for(&agent_mood),
        "mode": "agent",
        "gesture": agent_result["gesture"].clone(),
        "markdown": agent_result["markdown"].clone(),
        "workflowState": workflow_state,
        "awaitingApproval": agent_result["awaitingApproval"].clone(),
        "steps": steps,
        "evidence": agent_result["evidence"].clone(),
        "truncated": agent_result["truncated"].as_bool().unwrap_or(false),
        "artifacts": agent_result["artifacts"].clone(),
        "comics": agent_result["comics"].clone(),
        "messageId": message_id,
        "milestones": milestones,
    });
    if !request_id.is_empty() {
        db.set_agent_run_state(&request_id, user_id, &workflow_state, Some(&payload), None);
    }
    trace_event(
        "agent_run_completed",
        &json!({ "requestId": request_id, "state": workflow_state }),
    );
    ok(payload)
}
