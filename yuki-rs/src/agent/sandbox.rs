//! Port lib/agent/sandbox-runner.js — menjalankan file/snippet Node di
//! child process `node --permission` yang sama seperti versi Node
//! (bukan implementasi ulang): flag, preload penolak jaringan, prlimit,
//! batas output dan timeout identik.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct SandboxLimits {
    pub timeout_ms: u64,
    pub output_bytes: usize,
    pub memory_mb: u64,
    pub virtual_memory_mb: u64,
    pub cpu_seconds: u64,
    pub file_bytes: u64,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            output_bytes: 16_000,
            memory_mb: 256,
            virtual_memory_mb: 2048,
            cpu_seconds: 8,
            file_bytes: 1_048_576,
        }
    }
}

pub fn sandbox_policy(limits: &SandboxLimits) -> Value {
    json!({
        "timeoutMs": limits.timeout_ms,
        "outputBytes": limits.output_bytes,
        "memoryMb": limits.memory_mb,
        "virtualMemoryMb": limits.virtual_memory_mb,
        "cpuSeconds": limits.cpu_seconds,
        "fileBytes": limits.file_bytes,
        "network": false,
        "childProcess": false,
        "workers": false,
        "addons": false,
        "writableFilesystem": false,
        "environmentAllowlist": ["LANG", "TZ"],
        "shell": false,
    })
}

const NETWORK_BUILTINS: [&str; 14] = [
    "node:net", "net", "node:http", "http", "node:https", "https", "node:http2", "http2",
    "node:dgram", "dgram", "node:dns", "dns", "node:tls", "tls",
];

fn preload_source() -> String {
    let denied = serde_json::to_string(&NETWORK_BUILTINS).unwrap();
    format!(
        "import {{ registerHooks }} from 'node:module';\nconst denied = new Set({denied});\nregisterHooks({{ resolve(specifier, context, nextResolve) {{ if (denied.has(specifier)) throw new Error('SANDBOX_NETWORK_DENIED'); return nextResolve(specifier, context); }} }});\nconst deny = () => {{ throw new Error('SANDBOX_NETWORK_DENIED'); }};\nObject.defineProperty(globalThis, 'fetch', {{ value: deny, configurable: false, writable: false }});\nif ('WebSocket' in globalThis) Object.defineProperty(globalThis, 'WebSocket', {{ value: class {{ constructor() {{ deny(); }} }}, configurable: false, writable: false }});\nif (process.getBuiltinModule) Object.defineProperty(process, 'getBuiltinModule', {{ value(name) {{ if (denied.has(name) || denied.has('node:' + name)) deny(); throw new Error('SANDBOX_BUILTIN_ACCESS_DENIED'); }}, configurable: false, writable: false }});"
    )
}

/// encodeURIComponent JavaScript.
fn encode_uri_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub fn node_binary() -> String {
    if let Ok(bin) = std::env::var("YUKI_NODE_BIN") {
        if !bin.is_empty() {
            return bin;
        }
    }
    if Path::new("/usr/bin/node").exists() {
        return "/usr/bin/node".into();
    }
    "node".into()
}

#[derive(Debug, Clone)]
pub struct SandboxResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub output_truncated: bool,
    pub sandbox: String,
    pub policy: Value,
}

impl SandboxResult {
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "timedOut": self.timed_out,
            "outputTruncated": self.output_truncated,
            "sandbox": self.sandbox,
            "policy": self.policy,
        })
    }
}

fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("/bin/kill")
            .arg("-KILL")
            .arg(format!("-{pid}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub async fn run_sandboxed_node_test(
    entry: &Path,
    workspace_root: &Path,
    limits: &SandboxLimits,
) -> Result<SandboxResult, String> {
    let policy = sandbox_policy(limits);
    let root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let target = entry.canonicalize().unwrap_or_else(|_| entry.to_path_buf());
    let relative = target.strip_prefix(&root).map_err(|_| "SANDBOX_ENTRY_OUTSIDE_WORKSPACE".to_string())?;
    if relative.as_os_str().is_empty() {
        return Err("SANDBOX_ENTRY_OUTSIDE_WORKSPACE".into());
    }

    let node = node_binary();
    let node_args: Vec<String> = vec![
        "--permission".into(),
        "--no-addons".into(),
        format!("--max-old-space-size={}", limits.memory_mb),
        format!("--allow-fs-read={}", root.display()),
        format!(
            "--import=data:text/javascript,{}",
            encode_uri_component(&preload_source())
        ),
        target.display().to_string(),
    ];
    let (command, args, resource_limits) = if Path::new("/usr/bin/prlimit").exists() {
        let mut a = vec![
            format!("--as={}", limits.virtual_memory_mb * 1024 * 1024),
            format!("--cpu={}", limits.cpu_seconds),
            format!("--fsize={}", limits.file_bytes),
            "--".to_string(),
            node.clone(),
        ];
        a.extend(node_args.clone());
        ("/usr/bin/prlimit".to_string(), a, "linux-prlimit+node-runtime")
    } else {
        (node.clone(), node_args, "node-runtime")
    };

    let mut cmd = Command::new(&command);
    cmd.args(&args)
        .current_dir(&root)
        .env_clear()
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{e}"))?;
    let pid = child.id();

    let out_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let truncated = Arc::new(AtomicBool::new(false));
    let total = Arc::new(Mutex::new(0usize));
    let limit = limits.output_bytes;

    let mut make_reader = |stream: Option<tokio::process::ChildStdout>| {
        let out_buf = out_buf.clone();
        let truncated = truncated.clone();
        let total = total.clone();
        tokio::spawn(async move {
            if let Some(mut s) = stream {
                let mut chunk = [0u8; 4096];
                loop {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut total_lock = total.lock().unwrap();
                            let remaining = limit.saturating_sub(*total_lock);
                            let take = remaining.min(n);
                            *total_lock += take;
                            if n > remaining {
                                truncated.store(true, Ordering::SeqCst);
                            }
                            drop(total_lock);
                            out_buf
                                .lock()
                                .unwrap()
                                .extend_from_slice(&chunk[..take]);
                            if truncated.load(Ordering::SeqCst) {
                                kill_group(pid);
                                break;
                            }
                        }
                    }
                }
            }
        })
    };
    let out_task = make_reader(child.stdout.take());
    let err_task = {
        let err_buf = err_buf.clone();
        let truncated = truncated.clone();
        let total = total.clone();
        let mut stream = child.stderr.take();
        tokio::spawn(async move {
            if let Some(mut s) = stream.take() {
                let mut chunk = [0u8; 4096];
                loop {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut total_lock = total.lock().unwrap();
                            let remaining = limit.saturating_sub(*total_lock);
                            let take = remaining.min(n);
                            *total_lock += take;
                            if n > remaining {
                                truncated.store(true, Ordering::SeqCst);
                            }
                            drop(total_lock);
                            err_buf.lock().unwrap().extend_from_slice(&chunk[..take]);
                            if truncated.load(Ordering::SeqCst) {
                                kill_group(pid);
                                break;
                            }
                        }
                    }
                }
            }
        })
    };

    let timed_out = AtomicBool::new(false);
    let status = match tokio::time::timeout(
        Duration::from_millis(limits.timeout_ms),
        child.wait(),
    )
    .await
    {
        Ok(Ok(status)) => Some(status),
        Ok(Err(_)) => None,
        Err(_) => {
            timed_out.store(true, Ordering::SeqCst);
            kill_group(pid);
            let _ = child.wait().await;
            None
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(300), out_task).await;
    let _ = tokio::time::timeout(Duration::from_millis(300), err_task).await;

    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).to_string();
    let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).to_string();
    Ok(SandboxResult {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out: timed_out.load(Ordering::SeqCst),
        output_truncated: truncated.load(Ordering::SeqCst),
        sandbox: resource_limits.to_string(),
        policy,
    })
}

pub async fn run_sandboxed_node_snippet(code: &str, limits: &SandboxLimits) -> Result<SandboxResult, String> {
    // Node: sandboxPolicy({ timeoutMs: 5000, memoryMb: 128,
    // virtualMemoryMb: 3072, cpuSeconds: 4, ...limits }) — limit pemanggil
    // menimpa default snippet. Di Rust, field yang berbeda dari
    // SandboxLimits::default() dianggap ditetapkan pemanggil.
    let defaults = SandboxLimits::default();
    let mut merged = SandboxLimits {
        timeout_ms: 5_000,
        output_bytes: defaults.output_bytes,
        memory_mb: 128,
        virtual_memory_mb: 3072,
        cpu_seconds: 4,
        file_bytes: defaults.file_bytes,
    };
    if limits.timeout_ms != defaults.timeout_ms {
        merged.timeout_ms = limits.timeout_ms;
    }
    if limits.output_bytes != defaults.output_bytes {
        merged.output_bytes = limits.output_bytes;
    }
    if limits.memory_mb != defaults.memory_mb {
        merged.memory_mb = limits.memory_mb;
    }
    if limits.virtual_memory_mb != defaults.virtual_memory_mb {
        merged.virtual_memory_mb = limits.virtual_memory_mb;
    }
    if limits.cpu_seconds != defaults.cpu_seconds {
        merged.cpu_seconds = limits.cpu_seconds;
    }
    if limits.file_bytes != defaults.file_bytes {
        merged.file_bytes = limits.file_bytes;
    }
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "yuki-snippet-{}",
        crate::auth::random_alnum(10).to_lowercase()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let entry = dir.join("run.mjs");
    let wrapped = format!(
        "const __yuki_result__ = await (async () => {{\n{code}\n}})();\nprocess.stdout.write('\\n__YUKI_RESULT__' + JSON.stringify(__yuki_result__ === undefined ? null : __yuki_result__) + '\\n');\n"
    );
    if let Err(e) = std::fs::write(&entry, wrapped) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e.to_string());
    }
    let result = run_sandboxed_node_test(&entry, &dir, &merged).await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

// === Mesin JS tertanam (Fase 8): QuickJS menggantikan subprocess node ===
//
// Snippet run_javascript_code dan cek sintaks JS kini berjalan di
// QuickJS yang tertanam di binary — tanpa proses anak, tanpa Node.
// Isolasi lebih kuat secara konstruksi: tidak ada fs, network, atau
// builtin apa pun yang bisa dijangkau kode user. Cek sintaks TS
// memakai parser tree-sitter-typescript (deteksi node ERROR),
// menggantikan stripTypeScriptTypes + vm.Script. Pesan error
// mengikuti mesin baru (bukan V8) — kelas deviasi redaksional yang
// sudah tercatat. run_workspace_tests tetap memakai node eksternal
// (file tes adalah program Node: node:test/fs/modul).

fn snippet_limits(limits: &SandboxLimits) -> SandboxLimits {
    let defaults = SandboxLimits::default();
    let mut merged = SandboxLimits {
        timeout_ms: 5_000,
        output_bytes: defaults.output_bytes,
        memory_mb: 128,
        virtual_memory_mb: 3072,
        cpu_seconds: 4,
        file_bytes: defaults.file_bytes,
    };
    if limits.timeout_ms != defaults.timeout_ms {
        merged.timeout_ms = limits.timeout_ms;
    }
    if limits.output_bytes != defaults.output_bytes {
        merged.output_bytes = limits.output_bytes;
    }
    if limits.memory_mb != defaults.memory_mb {
        merged.memory_mb = limits.memory_mb;
    }
    if limits.virtual_memory_mb != defaults.virtual_memory_mb {
        merged.virtual_memory_mb = limits.virtual_memory_mb;
    }
    if limits.cpu_seconds != defaults.cpu_seconds {
        merged.cpu_seconds = limits.cpu_seconds;
    }
    if limits.file_bytes != defaults.file_bytes {
        merged.file_bytes = limits.file_bytes;
    }
    merged
}

const QJS_PRELUDE: &str = r#"
globalThis.__yuki_timers__ = [];
globalThis.__yuki_tid__ = 0;
globalThis.setTimeout = (cb, ms, ...args) => { const id = ++globalThis.__yuki_tid__; globalThis.__yuki_timers__.push({ id, at: Date.now() + Math.max(0, Number(ms) || 0), cb, args }); return id; };
globalThis.clearTimeout = (id) => { globalThis.__yuki_timers__ = globalThis.__yuki_timers__.filter(t => t.id !== id); };
globalThis.setInterval = (cb, ms, ...args) => { const id = ++globalThis.__yuki_tid__; const every = Math.max(1, Number(ms) || 0); globalThis.__yuki_timers__.push({ id, at: Date.now() + every, cb, args, every }); return id; };
globalThis.clearInterval = globalThis.clearTimeout;
const __yuki_fmt = (v) => { if (typeof v === 'string') return v; if (v === undefined) return 'undefined'; try { const s = JSON.stringify(v); return s === undefined ? String(v) : s; } catch (e) { return String(v); } };
const __yuki_line = (args) => __yuki_print(args.map(__yuki_fmt).join(' '));
globalThis.console = { log: (...a) => __yuki_line(a), info: (...a) => __yuki_line(a), warn: (...a) => __yuki_line(a), error: (...a) => __yuki_line(a), debug: (...a) => __yuki_line(a) };
"#;

const QJS_FIRE_TIMERS: &str = r#"(() => { const now = Date.now(); let next = -1; const due = []; globalThis.__yuki_timers__ = globalThis.__yuki_timers__.filter(t => { if (t.at <= now) { due.push(t); return !!t.every; } if (next < 0 || t.at < next) next = t.at; return true; }); for (const t of due) { if (t.every) { t.at = now + t.every; if (next < 0 || t.at < next) next = t.at; } try { t.cb(...t.args); } catch (e) { if (!globalThis.__yuki_timer_error__) globalThis.__yuki_timer_error__ = String((e && e.stack) || e); } } return next; })()"#;

/// Jalankan snippet di QuickJS tertanam. Format stdout meniru wrapper
/// Node persis (penanda \n__YUKI_RESULT__<json>\n) agar pemanggil
/// (tools.rs) tidak berubah.
pub async fn run_embedded_snippet(
    code: &str,
    limits: &SandboxLimits,
) -> Result<SandboxResult, String> {
    let merged = snippet_limits(limits);
    let code = code.to_string();
    tokio::task::spawn_blocking(move || run_quickjs_snippet(&code, &merged))
        .await
        .map_err(|e| format!("sandbox task gagal: {e}"))?
}

fn run_quickjs_snippet(code: &str, limits: &SandboxLimits) -> Result<SandboxResult, String> {
    use rquickjs::{Context, Function, Runtime};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Instant;

    let policy = sandbox_policy(limits);
    let base = |code: Option<i32>, stdout: String, stderr: String, timed_out: bool, truncated: bool| {
        SandboxResult {
            code,
            stdout,
            stderr,
            timed_out,
            output_truncated: truncated,
            sandbox: "quickjs".to_string(),
            policy: policy.clone(),
        }
    };

    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    runtime.set_memory_limit((limits.memory_mb as usize).saturating_mul(1024 * 1024));
    runtime.set_max_stack_size(1024 * 1024);
    let started = Instant::now();
    let timeout = Duration::from_millis(limits.timeout_ms);
    runtime.set_interrupt_handler(Some(Box::new(move || started.elapsed() > timeout)));
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;

    let logs = Rc::new(RefCell::new(String::new()));
    let truncated = Rc::new(RefCell::new(false));
    let settled: Rc<RefCell<Option<(String, String)>>> = Rc::new(RefCell::new(None));

    let output_cap = limits.output_bytes;
    let logs_w = logs.clone();
    let truncated_w = truncated.clone();
    let settled_w = settled.clone();

    let program = format!(
        "{prelude}\nglobalThis.__yuki_run__ = (async () => {{\n{code}\n}})();\nglobalThis.__yuki_run__.then(\n  (v) => {{ let s; try {{ s = JSON.stringify(v === undefined ? null : v); }} catch (e) {{ s = undefined; }} __yuki_settle('ok', s === undefined ? 'null' : s); }},\n  (e) => {{ const m = String((e && e.message) || e); if (m === 'interrupted') {{ __yuki_settle('timeout', ''); }} else {{ __yuki_settle('err', (e && e.stack) ? (String(e) + '\\n' + e.stack) : String(e)); }} }}\n);\n",
        prelude = QJS_PRELUDE,
        code = code
    );

    let eval_result = context.with(|ctx| {
        let globals = ctx.globals();
        let logs_p = logs_w.clone();
        let truncated_p = truncated_w.clone();
        globals.set(
            "__yuki_print",
            Function::new(ctx.clone(), move |line: String| {
                let mut buf = logs_p.borrow_mut();
                if *truncated_p.borrow() {
                    return;
                }
                buf.push_str(&line);
                buf.push('\n');
                if buf.len() > output_cap {
                    let cut = buf.floor_char_boundary(output_cap);
                    buf.truncate(cut);
                    *truncated_p.borrow_mut() = true;
                }
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let settled_s = settled_w.clone();
        globals.set(
            "__yuki_settle",
            Function::new(ctx.clone(), move |kind: String, payload: String| {
                let mut slot = settled_s.borrow_mut();
                if slot.is_none() {
                    *slot = Some((kind, payload));
                }
            })
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        ctx.eval::<(), _>(program.as_bytes()).map_err(|e| {
            if e.is_exception() {
                let exc = ctx.catch();
                let msg = exc
                    .as_object()
                    .and_then(|o| o.get::<_, String>("stack").ok())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{e}"));
                msg
            } else {
                e.to_string()
            }
        })
    });

    if let Err(message) = eval_result {
        // Interupsi QuickJS hanya terpicu saat deadline — error eval
        // yang tiba setelah batas waktu = timeout (Node: timedOut).
        let timed_out = started.elapsed() >= timeout;
        return Ok(base(
            if timed_out { None } else { Some(1) },
            logs.borrow().clone(),
            if timed_out { String::new() } else { message },
            timed_out,
            *truncated.borrow(),
        ));
    }

    // Pompa antrean job + timer sampai promise utama selesai/deadline.
    let mut timed_out = false;
    loop {
        loop {
            match runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => break,
            }
        }
        if settled.borrow().is_some() {
            break;
        }
        // Error dari callback timer diperlakukan seperti exception
        // tak tertangkap di Node (proses gagal).
        let timer_error = context.with(|ctx| {
            ctx.eval::<Option<String>, _>("globalThis.__yuki_timer_error__")
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
        });
        if let Some(te) = timer_error {
            *settled.borrow_mut() = Some(("err".to_string(), te));
            break;
        }
        if started.elapsed() > timeout {
            timed_out = true;
            break;
        }
        let next_at = context.with(|ctx| {
            ctx.eval::<f64, _>(QJS_FIRE_TIMERS).unwrap_or(-1.0)
        });
        let wait = if next_at > 0.0 {
            let delta = next_at - (chrono::Utc::now().timestamp_millis() as f64);
            Duration::from_millis(delta.clamp(1.0, 25.0) as u64)
        } else {
            Duration::from_millis(5)
        };
        std::thread::sleep(wait);
    }

    let stdout = logs.borrow().clone();
    let was_truncated = *truncated.borrow();
    if timed_out {
        return Ok(base(None, stdout, String::new(), true, was_truncated));
    }
    let final_result = match settled.borrow().clone() {
        Some((kind, _)) if kind == "timeout" => {
            Ok(base(None, stdout.clone(), String::new(), true, was_truncated))
        }
        Some((kind, _)) if kind == "err" && started.elapsed() >= timeout => {
            // Penolakan yang tiba tepat di deadline = interupsi.
            Ok(base(None, stdout.clone(), String::new(), true, was_truncated))
        }
        Some((kind, payload)) if kind == "ok" => Ok(base(
            Some(0),
            format!("{stdout}\n__YUKI_RESULT__{payload}\n"),
            String::new(),
            false,
            was_truncated,
        )),
        Some((_, payload)) => Ok(base(
            Some(1),
            stdout,
            payload,
            false,
            was_truncated,
        )),
        None => Ok(base(None, stdout, String::new(), true, was_truncated)),
    };
    final_result
}

/// Cek sintaks JavaScript via konstruktor Function QuickJS
/// (kompilasi tanpa eksekusi; import/export ditolak seperti
/// vm.Script — beda kecil: `return` di tingkat atas lolos di sini,
/// ditolak vm.Script). (ok, pesan, stack).
pub async fn embedded_check_js_syntax(code: &str) -> (bool, String, String) {
    let code = code.to_string();
    tokio::task::spawn_blocking(move || {
        use rquickjs::{Context, Runtime};
        let runtime = match Runtime::new() {
            Ok(r) => r,
            Err(e) => return (false, e.to_string(), String::new()),
        };
        let context = match Context::full(&runtime) {
            Ok(c) => c,
            Err(e) => return (false, e.to_string(), String::new()),
        };
        context.with(|ctx| {
            if ctx.globals().set("__yuki_check_code__", code).is_err() {
                return (false, "Pemeriksaan sintaks gagal.".into(), String::new());
            }
            match ctx.eval::<(), _>("new Function(__yuki_check_code__)") {
                Ok(()) => (true, String::new(), String::new()),
                Err(e) => {
                    if e.is_exception() {
                        let exc = ctx.catch();
                        let obj = exc.as_object();
                        let message = obj
                            .and_then(|o| o.get::<_, String>("message").ok())
                            .unwrap_or_else(|| format!("{e}"));
                        let stack = obj
                            .and_then(|o| o.get::<_, String>("stack").ok())
                            .unwrap_or_default();
                        (false, message, stack)
                    } else {
                        (false, e.to_string(), String::new())
                    }
                }
            }
        })
    })
    .await
    .unwrap_or((false, "Pemeriksaan sintaks gagal.".into(), String::new()))
}

/// Cek sintaks TypeScript dengan parser tree-sitter (node ERROR /
/// missing = tidak valid). Menggantikan stripTypeScriptTypes + vm.
/// (ok, pesan, stack-kosong).
pub async fn embedded_check_ts_syntax(code: &str) -> (bool, String, String) {
    let code = code.to_string();
    tokio::task::spawn_blocking(move || {
        let mut parser = tree_sitter::Parser::new();
        let language: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        if parser.set_language(&language).is_err() {
            return (false, "Parser TypeScript tidak tersedia.".into(), String::new());
        }
        let Some(tree) = parser.parse(&code, None) else {
            return (false, "Parse TypeScript gagal.".into(), String::new());
        };
        let root = tree.root_node();
        if !root.has_error() {
            return (true, String::new(), String::new());
        }
        let mut stack = vec![root];
        let mut bad = root;
        while let Some(node) = stack.pop() {
            if node.is_error() || node.is_missing() {
                bad = node;
                break;
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        let pos = bad.start_position();
        (
            false,
            format!(
                "Syntax error at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ),
            String::new(),
        )
    })
    .await
    .unwrap_or((false, "Pemeriksaan sintaks gagal.".into(), String::new()))
}
