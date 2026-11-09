//! Port skills/computing/workspace-files/handler.js — workspace coding
//! terisolasi per user dengan snapshot/rollback dan validasi statis.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::agent::policy::{
    legacy_workspace_user_segment, workspace_root, workspace_user_segment,
};
use crate::agent::sandbox::{embedded_check_js_syntax, embedded_check_ts_syntax, run_sandboxed_node_test, SandboxLimits};
use crate::agent::tools::ToolContext;

const MAX_BYTES: usize = 512 * 1024;

fn digest(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

fn blocked_names() -> HashSet<&'static str> {
    [
        ".env", ".git", ".ssh", ".runtime-secrets", "node_modules", "data", "backups", "id_rsa",
        "id_ed25519", "authorized_keys", "known_hosts",
    ]
    .into_iter()
    .collect()
}

fn blocked_extensions() -> HashSet<&'static str> {
    [".pem", ".key", ".p12", ".pfx", ".sqlite", ".sqlite3", ".db"]
        .into_iter()
        .collect()
}

fn history_root() -> Result<PathBuf, String> {
    Ok(workspace_root()?.join(".yuki-history"))
}

fn rename_legacy(from: &Path, to: &Path) -> Result<(), String> {
    if !to.exists() && from.exists() {
        if from.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            return Err("Workspace lama berupa symlink dan ditolak.".into());
        }
        std::fs::rename(from, to).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn user_directory(user_id: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let segment = workspace_user_segment(user_id)?;
    let target = root.join(&segment);
    let legacy_segment = legacy_workspace_user_segment(user_id);
    if !legacy_segment.is_empty() {
        let legacy = root.join(&legacy_segment);
        if legacy != target {
            rename_legacy(&legacy, &target)?;
        }
    }
    Ok(target)
}

fn user_history_directory(user_id: &str) -> Result<PathBuf, String> {
    let root = history_root()?;
    let segment = workspace_user_segment(user_id)?;
    let target = root.join(&segment);
    let legacy_segment = legacy_workspace_user_segment(user_id);
    if !legacy_segment.is_empty() {
        let legacy = root.join(&legacy_segment);
        if legacy != target {
            rename_legacy(&legacy, &target)
                .map_err(|_| "Riwayat workspace lama berupa symlink dan ditolak.".to_string())?;
        }
    }
    Ok(target)
}

fn assert_allowed(relative: &Path) -> Result<(), String> {
    let names = blocked_names();
    for part in relative.components() {
        let part = part.as_os_str().to_string_lossy().to_string();
        let lower = part.to_lowercase();
        if names.contains(lower.as_str()) || lower.starts_with(".env.") {
            return Err(format!("Path terlarang: {part}"));
        }
    }
    let ext = relative
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if blocked_extensions().contains(ext.as_str()) {
        return Err("File rahasia/database ditolak.".into());
    }
    Ok(())
}

fn ensure_no_symlink(root: &Path, target: &Path, allow_missing: bool) -> Result<(), String> {
    let relative = target.strip_prefix(root).map_err(|_| "Path di luar workspace ditolak.".to_string())?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part.as_os_str());
        if !current.exists() {
            if allow_missing {
                return Ok(());
            }
            return Err("File atau folder tidak ditemukan.".into());
        }
        if current
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err("Symlink tidak diizinkan.".into());
        }
    }
    Ok(())
}

struct Resolved {
    user_root: PathBuf,
    target: PathBuf,
    relative: String,
}

fn resolve_target(input: &str, user_id: &str, allow_missing: bool) -> Result<Resolved, String> {
    let user_root = user_directory(user_id)?;
    std::fs::create_dir_all(&user_root).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&user_root, std::fs::Permissions::from_mode(0o700));
    }
    let raw = if input.is_empty() { ".".to_string() } else { input.replace('\\', "/") };
    if raw.contains('\0') || Path::new(&raw).is_absolute() {
        return Err("Path absolut ditolak.".into());
    }
    // path.resolve leksikal
    let mut target = user_root.clone();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                target.pop();
            }
            other => target.push(other),
        }
    }
    let relative_path = target
        .strip_prefix(&user_root)
        .map_err(|_| "Path di luar workspace ditolak.".to_string())?
        .to_path_buf();
    assert_allowed(&relative_path)?;
    ensure_no_symlink(&user_root, &target, allow_missing)?;
    let relative = if relative_path.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative_path.to_string_lossy().replace('\\', "/")
    };
    Ok(Resolved {
        user_root,
        target,
        relative,
    })
}

fn read_text(target: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(target).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Target bukan file.".into());
    }
    if meta.len() as usize > MAX_BYTES {
        return Err("File terlalu besar.".into());
    }
    let content = std::fs::read_to_string(target).map_err(|e| e.to_string())?;
    if content.contains('\0') {
        return Err("File biner tidak didukung.".into());
    }
    Ok(content)
}

fn atomic_write(user_root: &Path, target: &Path, content: &str) -> Result<(), String> {
    let parent = target.parent().ok_or("Path tidak valid.")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    ensure_no_symlink(user_root, parent, false)?;
    let tmp = format!(
        "{}.yuki-{}.tmp",
        target.display(),
        crate::auth::random_base64url(5)
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(10)
            .collect::<String>()
    );
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, target).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Snapshot {
    id: String,
    path: String,
    action: String,
    hash: String,
    content: String,
    created_at: String,
}

fn save_snapshot(user_id: &str, relative: &str, content: &str, action: &str) -> Result<String, String> {
    let directory = user_history_directory(user_id)?;
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let id = format!(
        "{}_{}",
        chrono::Utc::now().timestamp_millis(),
        crate::auth::random_base64url(4)
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect::<String>()
            .to_lowercase()
    );
    let record = json!({
        "id": id,
        "path": relative.replace('\\', "/"),
        "action": action,
        "hash": digest(content),
        "content": content,
        "createdAt": crate::timeutil::to_iso_z(&chrono::Utc::now()),
    });
    std::fs::write(
        directory.join(format!("{id}.json")),
        serde_json::to_string(&record).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    let mut entries: Vec<String> = std::fs::read_dir(&directory)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    entries.reverse();
    for stale in entries.iter().skip(30) {
        let _ = std::fs::remove_file(directory.join(stale));
    }
    Ok(id)
}

fn snapshots(user_id: &str, relative: &str) -> Vec<Snapshot> {
    let Ok(directory) = user_history_directory(user_id) else {
        return Vec::new();
    };
    if !directory.exists() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.reverse();
    let wanted = relative.replace('\\', "/");
    names
        .iter()
        .filter_map(|name| {
            let text = std::fs::read_to_string(directory.join(name)).ok()?;
            let v: Value = serde_json::from_str(&text).ok()?;
            let snap = Snapshot {
                id: v["id"].as_str().unwrap_or("").to_string(),
                path: v["path"].as_str().unwrap_or("").to_string(),
                action: v["action"].as_str().unwrap_or("").to_string(),
                hash: v["hash"].as_str().unwrap_or("").to_string(),
                content: v["content"].as_str().unwrap_or("").to_string(),
                created_at: v["createdAt"].as_str().unwrap_or("").to_string(),
            };
            if wanted.is_empty() || snap.path == wanted {
                Some(snap)
            } else {
                None
            }
        })
        .collect()
}

fn compact_diff(before: &str, after: &str) -> String {
    let left: Vec<&str> = Regex::new(r"\r?\n").unwrap().split(before).collect();
    let right: Vec<&str> = Regex::new(r"\r?\n").unwrap().split(after).collect();
    let mut start = 0usize;
    while start < left.len() && start < right.len() && left[start] == right[start] {
        start += 1;
    }
    let mut left_end = left.len() as i64 - 1;
    let mut right_end = right.len() as i64 - 1;
    while left_end >= start as i64
        && right_end >= start as i64
        && left[left_end as usize] == right[right_end as usize]
    {
        left_end -= 1;
        right_end -= 1;
    }
    let context_start = start.saturating_sub(3);
    let mut output = vec![format!("@@ line {} @@", context_start + 1)];
    for index in context_start..start {
        output.push(format!("  {}", left[index]));
    }
    for index in start as i64..=left_end {
        output.push(format!("- {}", left[index as usize]));
    }
    for index in start as i64..=right_end {
        output.push(format!("+ {}", right[index as usize]));
    }
    let suffix_end = (right.len() as i64).min(right_end + 4);
    for index in (right_end + 1)..suffix_end {
        output.push(format!("  {}", right[index as usize]));
    }
    let truncated = output.len() > 160;
    let mut text = output[..output.len().min(160)].join("\n");
    if truncated {
        text.push_str("\n... [diff dipadatkan]");
    }
    text
}

fn fail(error: String) -> Value {
    json!({ "success": false, "error": error })
}

pub async fn dispatch_workspace(tool_name: &str, args: &Value, ctx: &ToolContext) -> Value {
    let user_id = ctx.user_id.clone();
    match tool_name {
        "list_workspace_files" => list_files(args, &user_id),
        "read_workspace_file" => read_file(args, &user_id),
        "search_workspace_code" => search_code(args, &user_id),
        "create_workspace_file" => create_file(args, &user_id),
        "replace_workspace_text" => replace_text(args, &user_id),
        "get_workspace_diff" => get_diff(args, &user_id),
        "rollback_workspace_file" => rollback_file(args, &user_id),
        "patch_workspace_files" => patch_files(args, &user_id),
        "validate_workspace_project" => validate_project(args, &user_id).await,
        "run_workspace_tests" => run_tests(args, &user_id).await,
        _ => fail(format!("Tool \"{tool_name}\" tidak ditemukan atau belum terdaftar.")),
    }
}

fn list_files(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or(".");
    let depth = args["depth"].as_i64().unwrap_or(3);
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let max_depth = depth.clamp(1, 5);
    let mut files: Vec<Value> = Vec::new();
    fn visit(
        current: &Path,
        user_root: &Path,
        level: i64,
        max_depth: i64,
        files: &mut Vec<Value>,
    ) {
        if level > max_depth || files.len() >= 200 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if files.len() >= 200 {
                return;
            }
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            let full = entry.path();
            let rel = full
                .strip_prefix(user_root)
                .unwrap_or(&full)
                .to_path_buf();
            if assert_allowed(&rel).is_err() {
                continue;
            }
            let rel_text = rel.to_string_lossy().replace('\\', "/");
            if file_type.is_dir() {
                files.push(json!({ "path": format!("{rel_text}/"), "type": "directory" }));
                visit(&full, user_root, level + 1, max_depth, files);
            } else if file_type.is_file() {
                let size = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
                files.push(json!({ "path": rel_text, "type": "file", "size": size }));
            }
        }
    }
    let target = &resolved.target;
    match std::fs::metadata(target) {
        Ok(meta) if meta.is_dir() => {
            visit(target, &resolved.user_root, 1, max_depth, &mut files)
        }
        Ok(meta) => files.push(json!({
            "path": resolved.relative.replace('\\', "/"),
            "type": "file",
            "size": meta.len(),
        })),
        Err(e) => return fail(e.to_string()),
    }
    json!({
        "success": true,
        "root": ".",
        "files": files,
        "truncated": files.len() >= 200,
    })
}

fn read_file(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or("");
    if input.is_empty() {
        return fail("Parameter path wajib diisi.".into());
    }
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let content = match read_text(&resolved.target) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let lines: Vec<&str> = Regex::new(r"\r?\n").unwrap().split(&content).collect();
    let start = (args["start_line"].as_i64().unwrap_or(1)).max(1) as usize;
    let default_end = args["end_line"].as_i64().unwrap_or((start + 239) as i64);
    let end = (lines.len() as i64)
        .min((start as i64).max(default_end))
        .min(start as i64 + 399) as usize;
    let numbered: Vec<String> = (start..=end)
        .filter_map(|n| lines.get(n - 1).map(|line| format!("{n}: {line}")))
        .collect();
    json!({
        "success": true,
        "path": resolved.relative.replace('\\', "/"),
        "hash": digest(&content),
        "total_lines": lines.len(),
        "start_line": start,
        "end_line": end,
        "content": numbered.join("\n"),
    })
}

fn search_code(args: &Value, user_id: &str) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    if query.is_empty() || query.chars().count() > 200 {
        return fail("Query wajib diisi dan maksimal 200 karakter.".into());
    }
    let input = args["path"].as_str().unwrap_or(".");
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let needle = query.to_lowercase();
    let limit = args["max_results"].as_i64().unwrap_or(50).clamp(1, 100) as usize;
    let mut results: Vec<Value> = Vec::new();
    fn inspect(
        file: &Path,
        user_root: &Path,
        needle: &str,
        limit: usize,
        results: &mut Vec<Value>,
    ) {
        if results.len() >= limit {
            return;
        }
        if std::fs::metadata(file).map(|m| m.len() as usize).unwrap_or(usize::MAX) > MAX_BYTES {
            return;
        }
        let Ok(content) = read_text(file) else {
            return;
        };
        let rel = file
            .strip_prefix(user_root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        for (index, line) in Regex::new(r"\r?\n").unwrap().split(&content).enumerate() {
            if results.len() < limit && line.to_lowercase().contains(needle) {
                results.push(json!({
                    "path": rel,
                    "line": index + 1,
                    "text": line.chars().take(500).collect::<String>(),
                }));
            }
        }
    }
    fn visit(
        current: &Path,
        user_root: &Path,
        needle: &str,
        limit: usize,
        results: &mut Vec<Value>,
    ) {
        let Ok(meta) = std::fs::metadata(current) else {
            return;
        };
        if meta.is_file() {
            inspect(current, user_root, needle, limit, results);
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if results.len() >= limit {
                return;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let full = entry.path();
            let rel = full.strip_prefix(user_root).unwrap_or(&full).to_path_buf();
            if assert_allowed(&rel).is_err() {
                continue;
            }
            if ft.is_dir() {
                visit(&full, user_root, needle, limit, results);
            } else if ft.is_file() {
                inspect(&full, user_root, needle, limit, results);
            }
        }
    }
    visit(&resolved.target, &resolved.user_root, &needle, limit, &mut results);
    json!({
        "success": true,
        "query": query,
        "results": results,
        "truncated": results.len() >= limit,
    })
}

fn create_file(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or("");
    if input.is_empty() {
        return fail("Parameter path wajib diisi.".into());
    }
    let text = args["content"].as_str().unwrap_or("");
    if text.len() > MAX_BYTES {
        return fail("Isi file terlalu besar.".into());
    }
    let resolved = match resolve_target(input, user_id, true) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    if resolved.target.exists() {
        return fail("File sudah ada. Gunakan replace_workspace_text.".into());
    }
    // Cek kandidat duplikat berdasarkan stem nama file.
    let relative_path = Path::new(&resolved.relative);
    let stem = relative_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase().replace(['-', '_', '.'], ""))
        .unwrap_or_default();
    let mut similar: Vec<String> = Vec::new();
    fn inspect_existing(current: &Path, user_root: &Path, stem: &str, similar: &mut Vec<String>) {
        if similar.len() >= 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if similar.len() >= 8 {
                return;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let full = entry.path();
            if ft.is_dir() {
                inspect_existing(&full, user_root, stem, similar);
            } else if ft.is_file() {
                let existing_stem = full
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_lowercase().replace(['-', '_', '.'], ""))
                    .unwrap_or_default();
                if !stem.is_empty() && existing_stem == stem {
                    similar.push(
                        full.strip_prefix(user_root)
                            .unwrap_or(&full)
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    inspect_existing(&resolved.user_root, &resolved.user_root, &stem, &mut similar);
    if !similar.is_empty() {
        return fail(format!(
            "DUPLICATE_CANDIDATE: ditemukan file serupa ({}). Baca dan gunakan kembali file tersebut sebelum membuat file baru.",
            similar.join(", ")
        ));
    }
    let snapshot_id = match save_snapshot(user_id, &resolved.relative, "", "create") {
        Ok(id) => id,
        Err(e) => return fail(e),
    };
    if let Err(e) = atomic_write(&resolved.user_root, &resolved.target, text) {
        return fail(e);
    }
    json!({
        "success": true,
        "path": resolved.relative.replace('\\', "/"),
        "hash": digest(text),
        "created": true,
        "snapshot_id": snapshot_id,
    })
}

/// Dipakai endpoint import proyek (server.js) — bukan tool LLM.
pub fn import_workspace_files(args: &Value, user_id: &str) -> Value {
    let files = args["files"].as_array().cloned().unwrap_or_default();
    let overwrite = args["overwrite"].as_bool().unwrap_or(false);
    if files.is_empty() || files.len() > 100 {
        return fail("files harus berisi 1-100 file workspace.".into());
    }
    let mut seen: HashSet<String> = HashSet::new();
    struct Prepared {
        resolved: Resolved,
        content: String,
        exists: bool,
        before: String,
    }
    let mut prepared: Vec<Prepared> = Vec::new();
    for item in &files {
        let (Some(path), Some(content)) = (item["path"].as_str(), item["content"].as_str()) else {
            return fail("Setiap file wajib memiliki path dan content berupa teks.".into());
        };
        let normalized = path.replace('\\', "/").to_lowercase();
        if !seen.insert(normalized) {
            return fail(format!("Path duplikat dalam import: {path}"));
        }
        if content.len() > MAX_BYTES {
            return fail(format!("{path}: file terlalu besar."));
        }
        let resolved = match resolve_target(path, user_id, true) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let exists = resolved.target.exists();
        if exists && !overwrite {
            return fail(format!("{path}: file sudah ada; overwrite harus diizinkan eksplisit."));
        }
        if exists && !std::fs::metadata(&resolved.target).map(|m| m.is_file()).unwrap_or(false) {
            return fail(format!("{path}: target bukan file."));
        }
        let before = if exists {
            match read_text(&resolved.target) {
                Ok(c) => c,
                Err(e) => return fail(e),
            }
        } else {
            String::new()
        };
        prepared.push(Prepared {
            resolved,
            content: content.to_string(),
            exists,
            before,
        });
    }
    let transaction_id = format!(
        "{}_{}",
        chrono::Utc::now().timestamp_millis(),
        crate::auth::random_alnum(8).to_lowercase()
    );
    let mut written: Vec<(&Prepared, String)> = Vec::new();
    for item in &prepared {
        let action = if item.exists {
            format!("import:{transaction_id}")
        } else {
            "create".to_string()
        };
        match save_snapshot(user_id, &item.resolved.relative, &item.before, &action) {
            Ok(snapshot_id) => {
                if let Err(e) = atomic_write(&item.resolved.user_root, &item.resolved.target, &item.content) {
                    for (done, _) in written.iter().rev() {
                        if done.exists {
                            let _ = atomic_write(&done.resolved.user_root, &done.resolved.target, &done.before);
                        } else if done.resolved.target.exists() {
                            let _ = std::fs::remove_file(&done.resolved.target);
                        }
                    }
                    return fail(format!("Import dibatalkan dan dipulihkan: {e}"));
                }
                written.push((item, snapshot_id));
            }
            Err(e) => {
                for (done, _) in written.iter().rev() {
                    if done.exists {
                        let _ = atomic_write(&done.resolved.user_root, &done.resolved.target, &done.before);
                    } else if done.resolved.target.exists() {
                        let _ = std::fs::remove_file(&done.resolved.target);
                    }
                }
                return fail(format!("Import dibatalkan dan dipulihkan: {e}"));
            }
        }
    }
    let files_out: Vec<Value> = written
        .iter()
        .map(|(item, snapshot_id)| {
            json!({
                "path": item.resolved.relative.replace('\\', "/"),
                "hash": digest(&item.content),
                "overwritten": item.exists,
                "snapshot_id": snapshot_id,
            })
        })
        .collect();
    json!({
        "success": true,
        "transaction_id": transaction_id,
        "imported": prepared.len(),
        "files": files_out,
    })
}

async fn run_tests(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or("");
    let pattern = Regex::new(r"(?i)(^|/)(test|tests|__tests__)(/|$)|\.(test|spec)\.[cm]?js$").unwrap();
    if input.is_empty() || !pattern.is_match(&input.replace('\\', "/")) {
        return fail("Hanya file JavaScript pada folder test/tests/__tests__ atau *.test.js/*.spec.js yang boleh dijalankan.".into());
    }
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    if !std::fs::metadata(&resolved.target).map(|m| m.is_file()).unwrap_or(false) {
        return fail("Test runner hanya menerima satu file test.".into());
    }
    match run_sandboxed_node_test(&resolved.target, &resolved.user_root, &SandboxLimits::default()).await {
        Ok(result) => {
            let mut out = result.to_json();
            out["success"] = json!(!result.timed_out && !result.output_truncated && result.code == Some(0));
            out["path"] = json!(resolved.relative.replace('\\', "/"));
            out["timeout_ms"] = json!(result.policy["timeoutMs"]);
            out
        }
        Err(e) => fail(e),
    }
}

fn replace_text(args: &Value, user_id: &str) -> Value {
    let edits: Vec<(String, String)> = match args["replacements"].as_array() {
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
            args["old_text"].as_str().unwrap_or("").to_string(),
            args["new_text"].as_str().unwrap_or("").to_string(),
        )],
    };
    let input = args["path"].as_str().unwrap_or("");
    if input.is_empty()
        || edits.is_empty()
        || edits.len() > 20
        || edits.iter().any(|(old, _)| old.is_empty())
    {
        return fail("path dan 1-20 replacement wajib diisi.".into());
    }
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let content = match read_text(&resolved.target) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let before_hash = digest(&content);
    let expected = args["expected_hash"].as_str().unwrap_or("");
    if expected.is_empty() {
        return fail("expected_hash dari read_workspace_file wajib disertakan.".into());
    }
    if expected != before_hash {
        return fail("File berubah sejak dibaca. Baca ulang sebelum edit.".into());
    }
    let mut updated = content.clone();
    for (old, new) in &edits {
        let count = updated.matches(old.as_str()).count();
        if count != 1 {
            return fail(format!(
                "old_text harus cocok tepat satu kali; ditemukan {count}. Tidak ada perubahan yang ditulis."
            ));
        }
        updated = updated.replacen(old.as_str(), new, 1);
    }
    if updated.len() > MAX_BYTES {
        return fail("Hasil file terlalu besar.".into());
    }
    let snapshot_id = match save_snapshot(user_id, &resolved.relative, &content, "edit") {
        Ok(id) => id,
        Err(e) => return fail(e),
    };
    if let Err(e) = atomic_write(&resolved.user_root, &resolved.target, &updated) {
        return fail(e);
    }
    json!({
        "success": true,
        "path": resolved.relative.replace('\\', "/"),
        "replacements": edits.len(),
        "before_hash": before_hash,
        "after_hash": digest(&updated),
        "snapshot_id": snapshot_id,
    })
}

fn get_diff(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or("");
    if input.is_empty() {
        return fail("Parameter path wajib diisi.".into());
    }
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let current = match read_text(&resolved.target) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let previous = snapshots(user_id, &resolved.relative).into_iter().next();
    let Some(previous) = previous else {
        return json!({
            "success": true,
            "path": resolved.relative,
            "has_changes": false,
            "message": "Belum ada snapshot sebelumnya.",
        });
    };
    json!({
        "success": true,
        "path": resolved.relative.replace('\\', "/"),
        "snapshot_id": previous.id,
        "before_hash": previous.hash,
        "after_hash": digest(&current),
        "has_changes": previous.content != current,
        "diff": compact_diff(&previous.content, &current),
    })
}

fn rollback_file(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or("");
    if input.is_empty() {
        return fail("Parameter path wajib diisi.".into());
    }
    let resolved = match resolve_target(input, user_id, true) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let records = snapshots(user_id, &resolved.relative);
    let snapshot = match args["snapshot_id"].as_str() {
        Some(id) => records.iter().find(|r| r.id == id),
        None => records.first(),
    };
    let Some(snapshot) = snapshot else {
        return fail("Snapshot rollback tidak ditemukan.".into());
    };
    if resolved.target.exists() {
        if let Ok(current) = read_text(&resolved.target) {
            let _ = save_snapshot(user_id, &resolved.relative, &current, "rollback");
        }
    }
    if snapshot.action == "create" {
        if resolved.target.exists() {
            let _ = std::fs::remove_file(&resolved.target);
        }
        return json!({
            "success": true,
            "path": resolved.relative.replace('\\', "/"),
            "rolled_back_to": snapshot.id,
            "deleted_created_file": true,
        });
    }
    if let Err(e) = atomic_write(&resolved.user_root, &resolved.target, &snapshot.content) {
        return fail(e);
    }
    json!({
        "success": true,
        "path": resolved.relative.replace('\\', "/"),
        "rolled_back_to": snapshot.id,
        "hash": digest(&snapshot.content),
    })
}

fn patch_files(args: &Value, user_id: &str) -> Value {
    let patches = args["patches"].as_array().cloned().unwrap_or_default();
    if patches.len() < 2 || patches.len() > 10 {
        return fail("patches harus berisi 2-10 perubahan file.".into());
    }
    let unique: HashSet<String> = patches
        .iter()
        .map(|p| p["path"].as_str().unwrap_or("").replace('\\', "/").to_lowercase())
        .collect();
    if unique.len() != patches.len() {
        return fail("Satu transaksi tidak boleh memuat path yang sama lebih dari sekali.".into());
    }
    struct Prepared {
        resolved: Resolved,
        content: String,
        updated: String,
    }
    let mut prepared: Vec<Prepared> = Vec::new();
    for item in &patches {
        let path = item["path"].as_str().unwrap_or("");
        let old_text = item["old_text"].as_str().unwrap_or("");
        let expected = item["expected_hash"].as_str().unwrap_or("");
        if path.is_empty() || old_text.is_empty() || expected.is_empty() {
            return fail("Setiap patch wajib memiliki path, old_text, new_text, dan expected_hash.".into());
        }
        let resolved = match resolve_target(path, user_id, false) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let content = match read_text(&resolved.target) {
            Ok(c) => c,
            Err(e) => return fail(e),
        };
        if digest(&content) != expected {
            return fail(format!("{path}: file berubah sejak dibaca."));
        }
        let count = content.matches(old_text).count();
        if count != 1 {
            return fail(format!("{path}: old_text ditemukan {count} kali."));
        }
        let updated = content.replacen(old_text, item["new_text"].as_str().unwrap_or(""), 1);
        if updated.len() > MAX_BYTES {
            return fail(format!("{path}: hasil terlalu besar."));
        }
        prepared.push(Prepared {
            resolved,
            content,
            updated,
        });
    }
    let transaction_id = format!(
        "{}_{}",
        chrono::Utc::now().timestamp_millis(),
        crate::auth::random_alnum(8).to_lowercase()
    );
    let mut written: Vec<&Prepared> = Vec::new();
    for item in &prepared {
        let _ = save_snapshot(user_id, &item.resolved.relative, &item.content, &format!("transaction:{transaction_id}"));
        if let Err(e) = atomic_write(&item.resolved.user_root, &item.resolved.target, &item.updated) {
            for done in written.iter().rev() {
                let _ = atomic_write(&done.resolved.user_root, &done.resolved.target, &done.content);
            }
            return fail(format!("Transaksi dibatalkan dan perubahan dipulihkan: {e}"));
        }
        written.push(item);
    }
    json!({
        "success": true,
        "transaction_id": transaction_id,
        "files": prepared.iter().map(|item| json!({
            "path": item.resolved.relative.replace('\\', "/"),
            "hash": digest(&item.updated),
        })).collect::<Vec<_>>(),
    })
}

async fn validate_content(relative: &str, content: &str) -> (bool, bool, Option<String>) {
    // (supported, valid, error)
    let ext = Path::new(relative)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let as_script = |source: &str| -> String {
        let mut s = source.to_string();
        let patterns: Vec<(Regex, &str)> = vec![
            (Regex::new(r#"(?ms)^\s*import[\s\S]*?from\s+['"][^'"]+['"]\s*;?\s*$"#).unwrap(), ""),
            (Regex::new(r#"(?ms)^\s*import\s+['"][^'"]+['"]\s*;?\s*$"#).unwrap(), ""),
            (Regex::new(r"(?ms)^\s*export\s*\{[^}]*\}\s*;?\s*$").unwrap(), ""),
            (Regex::new(r"\bexport\s+default\s+").unwrap(), ""),
            (Regex::new(r"\bexport\s+((?:async\s+)?(?:const|let|var|function|class)\b)").unwrap(), "$1"),
        ];
        for (pattern, replacement) in patterns {
            s = pattern.replace_all(&s, replacement).to_string();
        }
        s
    };
    match ext.as_str() {
        ".json" => match serde_json::from_str::<Value>(content) {
            Ok(_) => (true, true, None),
            Err(e) => (true, false, Some(format!("{e}"))),
        },
        ".js" | ".mjs" | ".cjs" => {
            let (ok, message, _) = embedded_check_js_syntax(&as_script(content)).await;
            (true, ok, if ok { None } else { Some(message) })
        }
        ".ts" | ".mts" | ".cts" => {
            let (ok, message, _) = embedded_check_ts_syntax(&as_script(content)).await;
            (true, ok, if ok { None } else { Some(message) })
        }
        ".html" | ".htm" => {
            for tag in ["script", "style", "body", "html"] {
                let open = Regex::new(&format!(r"(?i)<{tag}\b")).unwrap().find_iter(content).count();
                let close = Regex::new(&format!(r"(?i)</{tag}>")).unwrap().find_iter(content).count();
                if open != close {
                    return (true, false, Some(format!("Tag <{tag}> tidak seimbang.")));
                }
            }
            (true, true, None)
        }
        ".css" => {
            if content.matches('{').count() != content.matches('}').count() {
                return (true, false, Some("Kurung kurawal CSS tidak seimbang.".into()));
            }
            (true, true, None)
        }
        _ => (false, false, None),
    }
}

async fn validate_project(args: &Value, user_id: &str) -> Value {
    let input = args["path"].as_str().unwrap_or(".");
    let resolved = match resolve_target(input, user_id, false) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    let mut results: Vec<Value> = Vec::new();
    async fn inspect(
        file: &Path,
        user_root: &Path,
        results: &mut Vec<Value>,
    ) {
        if results.len() >= 120 {
            return;
        }
        if std::fs::metadata(file).map(|m| m.len() as usize).unwrap_or(usize::MAX) > MAX_BYTES {
            return;
        }
        let Ok(content) = read_text(file) else {
            return;
        };
        let relative = file
            .strip_prefix(user_root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let (supported, valid, error) = validate_content(&relative, &content).await;
        if supported {
            results.push(json!({ "path": relative, "valid": valid, "error": error }));
        }
    }
    async fn visit(
        current: &Path,
        user_root: &Path,
        results: &mut Vec<Value>,
    ) {
        let Ok(meta) = std::fs::metadata(current) else {
            return;
        };
        if meta.is_file() {
            inspect(current, user_root, results).await;
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if results.len() >= 120 {
                return;
            }
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let full = entry.path();
            let rel = full.strip_prefix(user_root).unwrap_or(&full).to_path_buf();
            if assert_allowed(&rel).is_err() {
                continue;
            }
            if ft.is_dir() {
                Box::pin(visit(&full, user_root, results)).await;
            } else if ft.is_file() {
                inspect(&full, user_root, results).await;
            }
        }
    }
    visit(&resolved.target, &resolved.user_root, &mut results).await;
    let valid = results.iter().all(|r| r["valid"].as_bool().unwrap_or(false));
    json!({
        "success": true,
        "checked": results.len(),
        "valid": valid,
        "files": results,
        "note": "Validasi statis aman; tidak menjalankan shell atau script proyek.",
    })
}

/// Hapus data workspace user (dipakai endpoint penghapusan akun).
pub fn delete_workspace_data(user_id: &str) {
    let Ok(root) = workspace_root() else {
        return;
    };
    let Ok(segment) = workspace_user_segment(user_id) else {
        return;
    };
    let history = root.join(".yuki-history");
    let legacy = legacy_workspace_user_segment(user_id);
    let targets = [
        root.join(&segment),
        history.join(&segment),
        root.join(&legacy),
        history.join(&legacy),
    ];
    for target in targets {
        if target.as_os_str().is_empty() {
            continue;
        }
        let relative = target.strip_prefix(&root).map(|r| r.to_path_buf());
        if let Ok(rel) = relative {
            if rel.as_os_str().is_empty() {
                continue;
            }
            if target.exists()
                && !target
                    .symlink_metadata()
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(true)
            {
                let _ = std::fs::remove_dir_all(&target);
            }
        }
    }
}
