//! Port lib/agent/workspace-policy.js.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const FORBIDDEN_ROOTS: [&str; 10] = [
    "/etc", "/home", "/opt", "/proc", "/root", "/srv", "/sys", "/usr", "/var", "/var/www",
];

fn contains_path(parent: &Path, child: &Path) -> bool {
    match child.strip_prefix(parent) {
        Ok(rel) => !rel.as_os_str().to_string_lossy().starts_with(".."),
        Err(_) => false,
    }
}

/// Mengembalikan root workspace yang lolos kebijakan, atau pesan error Node.
pub fn assert_safe_workspace_root(input: &str) -> Result<PathBuf, String> {
    let root = absolute(input);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var("HOME").unwrap_or_default();
    let user_home = absolute(&home);
    if root.parent().is_none() || root == Path::new("/") {
        return Err("YUKI_AGENT_WORKSPACE tidak boleh menunjuk ke root filesystem.".into());
    }
    if root == cwd || contains_path(&root, &cwd) {
        return Err("YUKI_AGENT_WORKSPACE tidak boleh menunjuk ke project atau parent project.".into());
    }
    if !home.is_empty() && (root == user_home || contains_path(&root, &user_home)) {
        return Err("YUKI_AGENT_WORKSPACE tidak boleh menunjuk ke home atau parent home.".into());
    }
    if FORBIDDEN_ROOTS.iter().any(|f| root == Path::new(f)) {
        return Err("YUKI_AGENT_WORKSPACE menunjuk ke lokasi host yang dilarang.".into());
    }
    Ok(root)
}

fn absolute(path: &str) -> PathBuf {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    // Normalisasi . dan .. secara leksikal (path.resolve Node).
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Root workspace efektif (env YUKI_AGENT_WORKSPACE, default 'agent-workspace').
/// Node melempar saat modul dimuat bila root tidak aman; di sini error
/// ditunda ke pemanggilan tool pertama agar server tetap bisa menyala.
pub fn workspace_root() -> Result<PathBuf, String> {
    let input = std::env::var("YUKI_AGENT_WORKSPACE").unwrap_or_else(|_| "agent-workspace".into());
    assert_safe_workspace_root(&input)
}

pub fn workspace_user_segment(user_id: &str) -> Result<String, String> {
    let value = user_id.trim();
    if value.is_empty() {
        return Err("Context user terautentikasi wajib untuk mengakses workspace.".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    Ok(format!("u_{}", &format!("{:x}", hasher.finalize())[..32]))
}

pub fn legacy_workspace_user_segment(user_id: &str) -> String {
    let replaced: String = user_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    replaced.chars().take(80).collect()
}
