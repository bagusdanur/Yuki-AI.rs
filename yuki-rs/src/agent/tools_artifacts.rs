//! Port skills/computing/html-canvas-builder/handler.js — artifact
//! interaktif (build/validate/get/read/update/patch) dengan template
//! universalMobileEngine byte-exact dari handler asli.

use std::path::PathBuf;

use regex::Regex;
use serde_json::{json, Value};

use crate::agent::sandbox::embedded_check_js_syntax;
use crate::agent::tools::ToolContext;

const UNIVERSAL_ENGINE: &str = include_str!("../../assets/universal_mobile_engine.txt");

fn public_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YUKI_PUBLIC_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    PathBuf::from("public")
}

fn count_matches(pattern: &str, text: &str) -> usize {
    Regex::new(pattern).unwrap().find_iter(text).count()
}

pub fn inspect_artifact_design(content: &str) -> Value {
    let mut issues: Vec<String> = Vec::new();
    let gradients = count_matches(r"(?i)(?:linear|radial|conic)-gradient\s*\(", content);
    let large_glows = count_matches(
        r"(?i)(?:box-shadow|text-shadow)\s*:[^;]*(?:#[0-9a-f]{3,8}|rgba?\()[^;]*(?:20px|2[1-9]px|[3-9]\dpx)",
        content,
    );
    if Regex::new(r"(?i)(?:#00ffff|#0ff\b|#ff00ff|#f0f\b|neon|cyberpunk)")
        .unwrap()
        .is_match(content)
    {
        issues.push("palet neon/cyberpunk generik".into());
    }
    if gradients >= 3 {
        issues.push(format!("{gradients} gradient dekoratif"));
    }
    if large_glows >= 2 || count_matches(r"(?i)text-shadow\s*:", content) >= 2 {
        issues.push("glow/text-shadow berlebihan".into());
    }
    if count_matches(r"(?i)backdrop-filter\s*:", content) >= 2 {
        issues.push("glassmorphism berulang".into());
    }
    if count_matches(r"(?i)border-radius\s*:\s*(?:2[4-9]|[3-9]\d)px", content) >= 4 {
        issues.push("terlalu banyak bentuk pill".into());
    }
    json!({
        "pass": issues.len() < 2,
        "issues": issues,
        "score": issues.len(),
    })
}

pub async fn validate_interactive_artifact(params: &Value) -> Value {
    let raw = params["html_content"]
        .as_str()
        .or_else(|| params["code"].as_str())
        .or_else(|| params["content"].as_str())
        .unwrap_or("");
    if raw.trim().is_empty() {
        return json!({ "success": false, "error": "Kode artifact tidak boleh kosong." });
    }
    let content = raw.to_string();
    let open_scripts = count_matches(r"(?i)<script\b", &content);
    let close_scripts = count_matches(r"(?i)</script>", &content);
    let open_styles = count_matches(r"(?i)<style\b", &content);
    let close_styles = count_matches(r"(?i)</style>", &content);
    if open_scripts != close_scripts {
        return json!({ "success": false, "error": format!("Tag script tidak seimbang ({open_scripts} buka, {close_scripts} tutup).") });
    }
    if open_styles != close_styles {
        return json!({ "success": false, "error": format!("Tag style tidak seimbang ({open_styles} buka, {close_styles} tutup).") });
    }
    let mut issues: Vec<Value> = Vec::new();
    let script_re = Regex::new(r"(?is)<script([^>]*)>([\s\S]*?)</script>").unwrap();
    let scripts: Vec<(String, String)> = script_re
        .captures_iter(&content)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    for (index, (attrs, source)) in scripts.iter().enumerate() {
        if Regex::new(r#"(?i)type\s*=\s*['"](?:application/json|importmap)['"]"#)
            .unwrap()
            .is_match(attrs)
        {
            continue;
        }
        let mut source = source.clone();
        if Regex::new(r#"(?i)type\s*=\s*['"]module['"]"#).unwrap().is_match(attrs) {
            source = Regex::new(r#"(?ms)^\s*import[\s\S]*?from\s+['"][^'"]+['"]\s*;?\s*$"#)
                .unwrap()
                .replace_all(&source, "")
                .to_string();
            source = Regex::new(r"\bexport\s+(?:default\s+)?")
                .unwrap()
                .replace_all(&source, "")
                .to_string();
        }
        let (ok, message, _) = embedded_check_js_syntax(&source).await;
        if !ok {
            issues.push(json!({ "script": index + 1, "message": message }));
        }
    }
    let controls = count_matches(r"(?i)<(?:button|input|select|textarea)\b", &content);
    let bindings = count_matches(
        r"(?i)addEventListener\s*\(|\bon(?:click|input|change|submit|pointerdown|touchstart)\s*=",
        &content,
    );
    let loops = count_matches(r"(?i)requestAnimationFrame\s*\(|setInterval\s*\(", &content);
    let smoke = json!({
        "mode": "static-safe",
        "controlsFound": controls,
        "eventBindingsFound": bindings,
        "animationLoopsFound": loops,
        "pass": controls == 0 || bindings > 0,
    });
    if smoke["pass"] != json!(true) {
        issues.push(json!({ "type": "InteractionSmokeTest", "message": "Kontrol ditemukan tetapi tidak ada event binding utama." }));
    }
    let mut out = json!({
        "success": issues.is_empty(),
        "valid": issues.is_empty(),
        "scripts_checked": scripts.len(),
        "smoke_test": smoke,
    });
    if !issues.is_empty() {
        out["issues"] = json!(issues);
        out["error"] = json!(format!("Ditemukan {} error sintaks JavaScript.", issues.len()));
    }
    out
}

fn sanitize_segment(value: &str, fallback: &str) -> String {
    let cleaned: String = value
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

pub async fn build_interactive_artifact(params: &Value, ctx: &ToolContext) -> Value {
    let clean_title = params["title"].as_str().unwrap_or("Interactive Canvas App").trim().to_string();
    let type_in = params["type"].as_str().unwrap_or("html");
    let clean_type = if ["html", "javascript", "svg"].contains(&type_in) {
        type_in
    } else {
        "html"
    };
    let raw = params["html_content"]
        .as_str()
        .or_else(|| params["code"].as_str())
        .or_else(|| params["content"].as_str())
        .unwrap_or("");
    if raw.trim().is_empty() {
        return json!({ "error": "Parameter \"html_content\" atau \"code\" tidak boleh kosong." });
    }
    let raw_content = raw.trim().to_string();
    let validation = validate_interactive_artifact(&json!({ "html_content": raw_content })).await;
    if validation["success"] != json!(true) {
        return validation;
    }
    let design = inspect_artifact_design(&raw_content);
    let explicitly_neon = Regex::new(r"(?i)\b(neon|cyberpunk|synthwave|glow)\b")
        .unwrap()
        .is_match(&clean_title);
    if params.get("id").is_none() && design["pass"] != json!(true) && !explicitly_neon {
        let issues = design["issues"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return json!({
            "success": false,
            "error": format!("DESIGN_QUALITY: desain terdeteksi memakai pola AI-slop ({issues}). Gunakan hierarchy, token semantik, satu aksen, dan efek yang lebih terkendali."),
            "design": design,
        });
    }
    let final_code = if raw_content.contains("_yuki_touch_guide") {
        raw_content.clone()
    } else if raw_content.to_lowercase().contains("<!doctype") || raw_content.to_lowercase().contains("<html") {
        if Regex::new(r"(?i)</body>").unwrap().is_match(&raw_content) {
            Regex::new(r"(?i)</body>")
                .unwrap()
                .replace(&raw_content, |_: &regex::Captures| format!("{UNIVERSAL_ENGINE}\n</body>"))
                .to_string()
        } else {
            format!("{raw_content}\n{UNIVERSAL_ENGINE}")
        }
    } else {
        format!(
            "<!DOCTYPE html>\n<html lang=\"id\">\n<head>\n  <meta charset=\"UTF-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no\">\n  <title>{clean_title}</title>\n  <style>\n    * {{ box-sizing: border-box; margin: 0; padding: 0; }}\n    html, body {{ width: 100%; height: 100%; background: var(--background); color: var(--foreground); font-family: Inter, ui-sans-serif, system-ui, sans-serif; overflow: hidden; display: flex; flex-direction: column; align-items: center; justify-content: center; }}\n  </style>\n</head>\n<body>\n  {raw_content}\n  {UNIVERSAL_ENGINE}\n</body>\n</html>"
        )
    };

    let raw_id = params["id"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "art_{}_{}",
                chrono::Utc::now().timestamp_millis(),
                crate::auth::random_alnum(5).to_lowercase()
            )
        });
    let artifact_id: String = sanitize_segment(&raw_id, &format!("art_{}", chrono::Utc::now().timestamp_millis()))
        .chars()
        .take(80)
        .collect();

    let clean_username = sanitize_segment(if ctx.username.is_empty() { "user" } else { &ctx.username }, "user");
    let clean_title_slug = sanitize_segment(&clean_title, "app");
    let user_file_name = format!("{clean_title_slug}.html");
    let relative_url = format!("/artifacts/{clean_username}/{user_file_name}");

    let artifacts_root = public_dir().join("artifacts");
    let user_dir = artifacts_root.join(&clean_username);
    let _ = std::fs::create_dir_all(&user_dir);
    let _ = std::fs::write(user_dir.join(&user_file_name), &final_code);
    let _ = std::fs::write(user_dir.join(format!("{artifact_id}.html")), &final_code);
    let _ = std::fs::write(artifacts_root.join(format!("{artifact_id}.html")), &final_code);

    let mut saved = json!({
        "id": artifact_id, "title": clean_title, "type": clean_type,
        "content": final_code, "version": 1,
    });
    if !ctx.user_id.is_empty() {
        if let Some(record) = ctx.db.save_user_artifact(
            &ctx.user_id,
            &json!({
                "id": artifact_id, "title": clean_title, "type": clean_type,
                "content": final_code, "patch_note": "Initial build",
            }),
        ) {
            saved = record;
        }
    }
    json!({
        "success": true,
        "validated": true,
        "validation": validation,
        "message": format!("Artifact \"{clean_title}\" (v{}) berhasil disimpan di folder \"{clean_username}/{user_file_name}\".", saved["version"].as_i64().unwrap_or(1)),
        "design_review": design,
        "artifact": {
            "id": saved["id"].as_str().unwrap_or(&artifact_id),
            "title": saved["title"].as_str().unwrap_or(&clean_title),
            "type": clean_type,
            "content": final_code,
            "version": saved["version"].as_i64().unwrap_or(1),
            "url": relative_url,
            "folder": clean_username,
            "filename": user_file_name,
        },
    })
}

pub fn get_active_artifact(params: &Value, ctx: &ToolContext) -> Value {
    let user_id = if !ctx.user_id.is_empty() {
        ctx.user_id.clone()
    } else {
        params["userId"].as_str().unwrap_or("").to_string()
    };
    if user_id.is_empty() {
        return json!({ "success": false, "error": "User ID tidak ditemukan dalam konteks." });
    }
    let Some(active) = ctx.db.get_latest_user_artifact(&user_id) else {
        return json!({
            "success": true,
            "has_active_artifact": false,
            "message": "Belum ada file artifact yang dibuat oleh user dalam sesi ini.",
        });
    };
    json!({
        "success": true,
        "has_active_artifact": true,
        "artifact": {
            "id": active["id"], "title": active["title"], "type": active["type"],
            "version": active["version"], "patch_note": active["patch_note"],
            "updated_at": active["updated_at"], "content": active["content"],
        },
        "message": format!("Artifact aktif ditemukan: \"{}\" (Versi {}). Gunakan kode ini sebagai basis modifikasi/penambahan level.", active["title"].as_str().unwrap_or(""), active["version"].as_i64().unwrap_or(1)),
    })
}

pub fn read_artifact_file(params: &Value, ctx: &ToolContext) -> Value {
    let user_id = if !ctx.user_id.is_empty() {
        ctx.user_id.clone()
    } else {
        params["userId"].as_str().unwrap_or("").to_string()
    };
    let artifact_id = params["id"].as_str().unwrap_or("");
    if user_id.is_empty() || artifact_id.is_empty() {
        return json!({ "success": false, "error": "Parameter \"id\" dan user ID harus disertakan." });
    }
    let Some(art) = ctx.db.get_user_artifact_by_id(&user_id, artifact_id) else {
        return json!({ "success": false, "error": format!("Artifact dengan ID \"{artifact_id}\" tidak ditemukan.") });
    };
    json!({
        "success": true,
        "artifact": art,
        "message": format!("File artifact \"{}\" (v{}) berhasil dibaca.", art["title"].as_str().unwrap_or(""), art["version"].as_i64().unwrap_or(1)),
    })
}

pub async fn update_interactive_artifact(params: &Value, ctx: &ToolContext) -> Value {
    let user_id = if !ctx.user_id.is_empty() {
        ctx.user_id.clone()
    } else {
        params["userId"].as_str().unwrap_or("").to_string()
    };
    let mut target_id = params["id"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| ctx.active_artifact_id.clone());
    let mut active_title: Option<String> = None;
    if !user_id.is_empty() {
        let active = match &target_id {
            Some(id) => ctx.db.get_user_artifact_by_id(&user_id, id),
            None => ctx.db.get_latest_user_artifact(&user_id),
        };
        if let Some(active) = active {
            target_id = active["id"].as_str().map(|s| s.to_string());
            active_title = active["title"].as_str().map(|s| s.to_string());
        }
    }
    let raw = params["html_content"]
        .as_str()
        .or_else(|| params["code"].as_str())
        .or_else(|| params["content"].as_str())
        .unwrap_or("");
    if raw.is_empty() {
        return json!({ "success": false, "error": "Parameter \"html_content\" atau \"code\" yang diperbarui tidak boleh kosong." });
    }
    let final_title = params["title"]
        .as_str()
        .map(|s| s.to_string())
        .or(active_title)
        .unwrap_or_else(|| "Updated Canvas App".to_string());
    let patch_note = params["patch_note"]
        .as_str()
        .or_else(|| params["summary"].as_str())
        .unwrap_or("Incremental update / level extension");
    let mut build_params = json!({
        "title": final_title,
        "html_content": raw,
        "patch_note": patch_note,
    });
    if let Some(id) = target_id {
        build_params["id"] = json!(id);
    }
    build_interactive_artifact(&build_params, ctx).await
}

pub async fn patch_interactive_artifact(params: &Value, ctx: &ToolContext) -> Value {
    let user_id = if !ctx.user_id.is_empty() {
        ctx.user_id.clone()
    } else {
        params["userId"].as_str().unwrap_or("").to_string()
    };
    let target_id = params["id"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| ctx.active_artifact_id.clone());
    let active = match &target_id {
        Some(id) => ctx.db.get_user_artifact_by_id(&user_id, id),
        None => ctx.db.get_latest_user_artifact(&user_id),
    };
    let Some(active) = active else {
        return json!({ "success": false, "error": "Artifact aktif tidak ditemukan." });
    };
    let edits: Vec<(String, String)> = match params["replacements"].as_array() {
        Some(list) => list
            .iter()
            .map(|e| {
                (
                    e["old_text"].as_str().unwrap_or("").to_string(),
                    e["new_text"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect(),
        None => vec![(
            params["old_text"].as_str().unwrap_or("").to_string(),
            params["new_text"].as_str().unwrap_or("").to_string(),
        )],
    };
    if edits.is_empty() || edits.len() > 20 || edits.iter().any(|(old, _)| old.is_empty()) {
        return json!({ "success": false, "error": "Patch membutuhkan 1-20 pasangan old_text dan new_text." });
    }
    let mut updated = active["content"].as_str().unwrap_or("").to_string();
    for (old, new) in &edits {
        let count = updated.matches(old.as_str()).count();
        if count != 1 {
            return json!({ "success": false, "error": format!("old_text harus cocok tepat satu kali; ditemukan {count}. Artifact tidak diubah.") });
        }
        updated = updated.replacen(old.as_str(), new, 1);
    }
    let patch_note = params["patch_note"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("Patch minimal pada {} bagian artifact aktif", edits.len()));
    let mut sub_ctx = ctx.clone();
    sub_ctx.active_artifact_id = active["id"].as_str().map(|s| s.to_string());
    let mut result = update_interactive_artifact(
        &json!({
            "id": active["id"],
            "title": active["title"],
            "html_content": updated,
            "patch_note": patch_note,
        }),
        &sub_ctx,
    )
    .await;
    if result["success"] == json!(true) {
        result["replacements"] = json!(edits.len());
    }
    result
}

pub async fn dispatch_artifacts(tool_name: &str, args: &Value, ctx: &ToolContext) -> Value {
    match tool_name {
        "validate_interactive_artifact" => validate_interactive_artifact(args).await,
        "build_interactive_artifact" => build_interactive_artifact(args, ctx).await,
        "get_active_artifact" => get_active_artifact(args, ctx),
        "read_artifact_file" => read_artifact_file(args, ctx),
        "update_interactive_artifact" => update_interactive_artifact(args, ctx).await,
        "patch_interactive_artifact" => patch_interactive_artifact(args, ctx).await,
        _ => json!({ "error": format!("Tool \"{tool_name}\" tidak ditemukan atau belum terdaftar.") }),
    }
}
