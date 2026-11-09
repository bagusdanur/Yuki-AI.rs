//! Port lib/agent/skills-engine.js: katalog dari folder skills/ (SKILL.md),
//! registry tool (pemetaan handler default persis), dan TOOL_SCHEMAS
//! (diekstrak verbatim dari skills-engine Node ke assets/tool_schemas.json).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub title: String,
    pub category: String,
    pub description: String,
    pub version: String,
    pub instructions: String,
}

/// Pemetaan skill -> tools dari ekspor default handler.js (urutan scan
/// direktori Node: alfabetis per kategori).
pub const SKILL_TOOLS: [(&str, &[&str]); 16] = [
    ("code-analyzer", &["analyze_code_syntax"]),
    ("code-scratchpad", &["run_javascript_code"]),
    ("delegate-task", &["delegate_tasks"]),
    (
        "html-canvas-builder",
        &[
            "validate_interactive_artifact",
            "build_interactive_artifact",
            "get_active_artifact",
            "read_artifact_file",
            "update_interactive_artifact",
            "patch_interactive_artifact",
        ],
    ),
    ("safe-calculator", &["calculate_expression"]),
    (
        "workspace-files",
        &[
            "list_workspace_files",
            "read_workspace_file",
            "search_workspace_code",
            "create_workspace_file",
            "replace_workspace_text",
            "get_workspace_diff",
            "rollback_workspace_file",
            "patch_workspace_files",
            "validate_workspace_project",
            "run_workspace_tests",
        ],
    ),
    (
        "self-improvement",
        &["record_self_improvement", "list_self_improvements"],
    ),
    ("ryukomik-expert", &["search_ryukomik", "get_latest_comics"]),
    (
        "datetime-helper",
        &["get_current_time", "calculate_date_difference"],
    ),
    (
        "task-planner",
        &["add_user_note", "list_user_notes", "delete_user_note"],
    ),
    (
        "task-scheduler",
        &[
            "schedule_task",
            "reschedule_task",
            "list_scheduled_tasks",
            "cancel_scheduled_task",
        ],
    ),
    ("api-tester", &["http_api_request"]),
    (
        "browser-agent",
        &["browse_page", "screenshot_url", "browser_extract"],
    ),
    ("url-reader", &["read_url"]),
    ("web-search", &["web_search"]),
    ("skill-health", &["run_skill_health_check"]),
];

pub fn tool_skill(tool_name: &str) -> Option<(&'static str, &'static str)> {
    for (skill, tools) in SKILL_TOOLS.iter() {
        if tools.contains(&tool_name) {
            return Some((skill, skill_title_static(skill)));
        }
    }
    None
}

fn skill_title_static(name: &str) -> &'static str {
    match name {
        "code-analyzer" => "Code Analyzer & Debugger",
        "code-scratchpad" => "Code Scratchpad & Runner",
        "delegate-task" => "Yuki Subagent — Paralel Task Delegation",
        "html-canvas-builder" => "Interactive HTML/Canvas Artifact Builder & Workspace",
        "safe-calculator" => "Safe Mathematical Calculator",
        "workspace-files" => "Isolated Coding Workspace",
        "self-improvement" => "Self-Improvement & Continuous Learning",
        "ryukomik-expert" => "Ryukomik Manga/Manhwa Expert",
        "datetime-helper" => "Datetime & Timezone Assistant",
        "task-planner" => "Task & Notes Planner",
        "task-scheduler" => "Yuki Scheduler — Pengingat & Tugas Terjadwal",
        "api-tester" => "REST API & HTTP Tester",
        "browser-agent" => "Yuki Browser — Web Automation & Scraping",
        "url-reader" => "Web Page & Article Reader",
        "web-search" => "Web Search & Research",
        "skill-health" => "Registry & Functional Skill Health",
        _ => "",
    }
}

pub fn skills_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YUKI_SKILLS_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    let cwd_skills = PathBuf::from("skills");
    if cwd_skills.is_dir() {
        return cwd_skills;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("workspace/yuki-ai-rust/original/skills")
}

fn parse_skill_file(content: &str) -> (HashMap<String, String>, String) {
    let mut metadata = HashMap::new();
    let re = regex::Regex::new(r"(?s)^---\r?\n(.*?)\r?\n---\r?\n([\s\S]*)$").unwrap();
    let Some(caps) = re.captures(content) else {
        return (metadata, content.to_string());
    };
    let yaml = caps[1].to_string();
    let body = caps[2].trim().to_string();
    for line in yaml.lines() {
        for key in ["name", "category", "title", "description", "version", "author"] {
            if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
                metadata.insert(key.to_string(), rest.trim().to_string());
            }
        }
    }
    (metadata, body)
}

fn scan_skills() -> Vec<SkillEntry> {
    let base = skills_dir();
    let mut dirs: Vec<PathBuf> = Vec::new();
    fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut items: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        items.sort();
        for item in items {
            if item.is_dir() {
                walk(&item, out);
            } else if item.file_name().map(|n| n == "SKILL.md").unwrap_or(false) {
                if let Some(parent) = item.parent() {
                    out.push(parent.to_path_buf());
                }
            }
        }
    }
    if base.is_dir() {
        walk(&base, &mut dirs);
    }
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(content) = std::fs::read_to_string(dir.join("SKILL.md")) else {
            continue;
        };
        let (metadata, body) = parse_skill_file(&content);
        let fallback_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = metadata
            .get("name")
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback_name);
        out.push(SkillEntry {
            title: metadata
                .get("title")
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| name.clone()),
            category: metadata
                .get("category")
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "general".into()),
            description: metadata.get("description").cloned().unwrap_or_default(),
            version: metadata
                .get("version")
                .cloned()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "1.0.0".into()),
            instructions: body,
            name,
        });
    }
    out
}

pub fn skills_catalog() -> &'static Vec<SkillEntry> {
    static CATALOG: OnceLock<Vec<SkillEntry>> = OnceLock::new();
    CATALOG.get_or_init(scan_skills)
}

pub fn list_skills() -> Vec<Value> {
    skills_catalog()
        .iter()
        .map(|s| {
            json!({
                "name": s.name, "title": s.title, "category": s.category,
                "description": s.description, "version": s.version,
            })
        })
        .collect()
}

fn tool_schemas() -> &'static Vec<Value> {
    static SCHEMAS: OnceLock<Vec<Value>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        serde_json::from_str(include_str!("../../assets/tool_schemas.json"))
            .unwrap_or_default()
    })
}

pub fn schema_for(tool_name: &str) -> Option<Value> {
    tool_schemas()
        .iter()
        .find(|t| t.pointer("/function/name").and_then(|v| v.as_str()) == Some(tool_name))
        .cloned()
}

/// getAvailableTools: skema untuk tool yang terdaftar (registry ∩ skema),
/// urutan mengikuti registry (scan skill alfabetis).
pub fn available_tools() -> Vec<Value> {
    let mut out = Vec::new();
    for (_, tools) in SKILL_TOOLS.iter() {
        for name in *tools {
            if let Some(schema) = schema_for(name) {
                out.push(schema);
            }
        }
    }
    out
}

pub fn skills_health() -> Value {
    let catalog = skills_catalog();
    let mut missing: Vec<String> = Vec::new();
    let mut skills_json = Vec::new();
    let mut total_tools = 0usize;
    for skill in catalog {
        let tools: Vec<&str> = SKILL_TOOLS
            .iter()
            .find(|(name, _)| *name == skill.name)
            .map(|(_, t)| t.iter().copied().collect())
            .unwrap_or_default();
        total_tools += tools.len();
        for tool in &tools {
            if schema_for(tool).is_none() {
                missing.push(tool.to_string());
            }
        }
        let healthy = !tools.is_empty() && tools.iter().all(|t| schema_for(t).is_some());
        skills_json.push(json!({ "name": skill.name, "tools": tools.len(), "healthy": healthy }));
    }
    json!({
        "success": missing.is_empty(),
        "healthy": missing.is_empty(),
        "skills_count": catalog.len(),
        "tools_count": total_tools,
        "missing_schemas": missing,
        "skills": skills_json,
    })
}

pub fn build_skills_catalog_prompt() -> String {
    let list: Vec<String> = skills_catalog()
        .iter()
        .map(|s| format!("- **{}** ({}): {}", s.title, s.name, s.description))
        .collect();
    format!("### SKILLS & CAPABILITIES AKTIF (Yuki Engine):\n{}", list.join("\n"))
}
