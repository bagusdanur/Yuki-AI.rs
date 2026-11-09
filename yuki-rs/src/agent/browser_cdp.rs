//! Browser agent asli (Fase 8): klien CDP (Chrome DevTools Protocol)
//! ditulis tangan di atas tokio-tungstenite — pengganti puppeteer-core
//! Node tanpa dependensi chromiumoxide. Kontrak mengikuti
//! lib/browser.js persis: deteksi executable yang sama, argumen
//! launch yang sama, konteks terisolasi per panggilan (proses Chromium
//! baru per panggilan di sini — isolasi lebih kuat dari context Node),
//! cookie persisten per user (data/browser-profiles/<user>/cookies.json
//! 0600), validasi SSRF di URL awal + Fetch interception per request
//! (redirect/subresource ke IP privat dibatalkan sebelum fetch), dan
//! bentuk hasil/error yang sama untuk browse_page, screenshot_url,
//! browser_extract.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::agent::security::validate_safe_outbound_url;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const UA_BROWSE: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const UA_SCREENSHOT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36";

/// findChromiumPath Node: kandidat tetap, CHROMIUM_PATH, lalu cache
/// ~/.agent-browser/browsers/chrome-*/chrome.
pub fn find_chromium_path() -> Option<String> {
    let candidates = [
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
        "/snap/bin/chromium",
        "/usr/lib/chromium/chromium",
        "/usr/lib/chromium-browser/chromium-browser",
    ];
    for p in candidates {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }
    if let Ok(path) = std::env::var("CHROMIUM_PATH") {
        if !path.is_empty() && std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let base = std::path::Path::new(&home).join(".agent-browser/browsers");
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut dirs: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|d| d.starts_with("chrome-"))
            .collect();
        dirs.sort();
        dirs.reverse();
        for dir in dirs {
            let bin = base.join(&dir).join("chrome");
            if bin.exists() {
                return Some(bin.to_string_lossy().to_string());
            }
            let alt = base.join(&dir).join("chrome-linux/chrome");
            if alt.exists() {
                return Some(alt.to_string_lossy().to_string());
            }
        }
    }
    None
}

// ===== Profil cookie per user (port 4.4 lib/browser.js) =====

fn profile_base() -> PathBuf {
    if let Ok(dir) = std::env::var("BROWSER_PROFILE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("data/browser-profiles")
}

fn safe_profile_dir(user_id: &str) -> Option<PathBuf> {
    if user_id.is_empty()
        || user_id.len() > 64
        || !user_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(profile_base().join(user_id))
}

fn cookie_shape_ok(c: &Value) -> bool {
    c.get("name").map(|v| v.is_string()).unwrap_or(false)
        && c.get("value").map(|v| v.is_string()).unwrap_or(false)
        && c.get("domain").map(|v| v.is_string()).unwrap_or(false)
}

fn load_user_cookies(user_id: &str) -> Vec<Value> {
    let Some(dir) = safe_profile_dir(user_id) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(dir.join("cookies.json")) else {
        return Vec::new();
    };
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    list.into_iter()
        .filter(cookie_shape_ok)
        .take(500)
        .collect()
}

fn save_user_cookies(user_id: &str, cookies: &[Value]) {
    let Some(dir) = safe_profile_dir(user_id) else {
        return;
    };
    let clean: Vec<Value> = cookies
        .iter()
        .filter(|c| cookie_shape_ok(c))
        .take(500)
        .cloned()
        .collect();
    let text = serde_json::to_string(&clean).unwrap_or_default();
    if text.len() > 256 * 1024 {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        if std::fs::write(dir.join("cookies.json"), &text).is_ok() {
            let _ = std::fs::set_permissions(
                dir.join("cookies.json"),
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("cookies.json"), &text);
    }
    prune_stale_profiles();
}

fn prune_stale_profiles() {
    use std::sync::Mutex as StdMutex;
    static LAST: StdMutex<i64> = StdMutex::new(0);
    let now = chrono::Utc::now().timestamp_millis();
    {
        let mut last = LAST.lock().unwrap();
        if now - *last < 3_600_000 {
            return;
        }
        *last = now;
    }
    let base = profile_base();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified.elapsed().map(|d| d.as_millis() as i64).unwrap_or(0)
                    > 90 * 24 * 3600 * 1000
                {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
    }
}

// ===== Klien CDP =====

struct Cdp {
    sink: futures_util::stream::SplitSink<WsStream, Message>,
    stream: futures_util::stream::SplitStream<WsStream>,
    next_id: u64,
    responses: HashMap<u64, Result<Value, String>>,
    events: Vec<Value>,
    fetch_queue: Vec<Value>,
    block_heavy: bool,
    depth: usize,
    processed_events: usize,
    navigated: bool,
    load_fired: bool,
    dom_fired: bool,
    inflight: i64,
}

impl Cdp {
    async fn connect(ws_url: &str) -> Result<Self, String> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("CDP connect gagal: {e}"))?;
        let (sink, stream) = ws.split();
        Ok(Self {
            sink,
            stream,
            next_id: 0,
            responses: HashMap::new(),
            events: Vec::new(),
            fetch_queue: Vec::new(),
            block_heavy: false,
            depth: 0,
            processed_events: 0,
            navigated: false,
            load_fired: false,
            dom_fired: false,
            inflight: 0,
        })
    }

    async fn read_one(&mut self) -> Result<(), String> {
        let msg = self
            .stream
            .next()
            .await
            .ok_or_else(|| "CDP: koneksi tertutup".to_string())?;
        let msg = msg.map_err(|e| format!("CDP read gagal: {e}"))?;
        let Message::Text(text) = msg else {
            return Ok(());
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Ok(());
        };
        if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
            let result = if let Some(err) = value.get("error") {
                Err(err["message"]
                    .as_str()
                    .unwrap_or("CDP error")
                    .to_string())
            } else {
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            };
            self.responses.insert(id, result);
        } else if let Some(method) = value.get("method").and_then(|v| v.as_str()) {
            if method == "Fetch.requestPaused" {
                self.fetch_queue.push(value);
            } else {
                self.events.push(value);
            }
        }
        Ok(())
    }

    async fn cmd(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, String> {
        self.depth += 1;
        let result = self.cmd_inner(method, params, session).await;
        self.depth -= 1;
        result
    }

    async fn cmd_inner(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.sink
            .send(Message::Text(msg.to_string().into()))
            .await
            .map_err(|e| format!("CDP send gagal: {e}"))?;
        loop {
            if let Some(r) = self.responses.remove(&id) {
                return r;
            }
            self.read_one().await?;
            if self.depth <= 1 {
                Box::pin(self.drain_fetch()).await?;
            }
        }
    }

    async fn drain_fetch(&mut self) -> Result<(), String> {
        while let Some(paused) = self.fetch_queue.pop() {
            let session = paused
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let params = &paused["params"];
            let request_id = params["requestId"].as_str().unwrap_or("").to_string();
            let resource_type = params["resourceType"].as_str().unwrap_or("");
            let url = params["request"]["url"].as_str().unwrap_or("");
            let heavy = ["Image", "Media", "Font", "Stylesheet"].contains(&resource_type);
            let mut abort = self.block_heavy && heavy;
            if !abort && (url.starts_with("http://") || url.starts_with("https://")) {
                let check = validate_safe_outbound_url(url).await;
                if !check.safe {
                    abort = true;
                }
            }
            if abort {
                let _ = self
                    .cmd(
                        "Fetch.failRequest",
                        json!({ "requestId": request_id, "errorReason": "Aborted" }),
                        session.as_deref(),
                    )
                    .await;
            } else {
                let _ = self
                    .cmd(
                        "Fetch.continueRequest",
                        json!({ "requestId": request_id }),
                        session.as_deref(),
                    )
                    .await;
            }
        }
        Ok(())
    }

    fn scan_events(&mut self, session: &str) {
        while self.processed_events < self.events.len() {
            let ev = &self.events[self.processed_events];
            self.processed_events += 1;
            if ev.get("sessionId").and_then(|v| v.as_str()) != Some(session) {
                continue;
            }
            match ev.get("method").and_then(|v| v.as_str()) {
                Some("Page.frameNavigated") => {
                    // Navigasi ke dokumen baru mereset flag lifecycle —
                    // DCL/load milik about:blank awal tidak boleh
                    // memuaskan wait navigasi halaman target.
                    let url = ev["params"]["frame"]["url"].as_str().unwrap_or("");
                    if !url.is_empty() && url != "about:blank" {
                        self.navigated = true;
                        self.load_fired = false;
                        self.dom_fired = false;
                    }
                }
                Some("Page.loadEventFired") => self.load_fired = true,
                Some("Page.domContentEventFired") => self.dom_fired = true,
                Some("Network.requestWillBeSent") => {
                    let url = ev["params"]["request"]["url"].as_str().unwrap_or("");
                    if !url.starts_with("data:") {
                        self.inflight += 1;
                    }
                }
                Some("Network.loadingFinished") | Some("Network.loadingFailed") => {
                    if self.inflight > 0 {
                        self.inflight -= 1;
                    }
                }
                _ => {}
            }
        }
    }

    /// waitUntil ala Puppeteer: domcontentloaded | load |
    /// networkidle0 | networkidle2 (default).
    async fn wait_navigation(
        &mut self,
        session: &str,
        wait_until: &str,
        timeout_ms: u64,
    ) -> Result<(), String> {
        let started = Instant::now();
        let deadline = Duration::from_millis(timeout_ms);
        let mut idle_since: Option<Instant> = None;
        loop {
            self.scan_events(session);
            let ready = match wait_until {
                "domcontentloaded" => self.navigated && self.dom_fired,
                "load" => self.navigated && self.load_fired,
                "networkidle0" => {
                    if self.navigated && self.load_fired && self.inflight == 0 {
                        match idle_since {
                            Some(t) => t.elapsed() >= Duration::from_millis(500),
                            None => {
                                idle_since = Some(Instant::now());
                                false
                            }
                        }
                    } else {
                        idle_since = None;
                        false
                    }
                }
                _ => {
                    // networkidle2
                    if self.navigated && self.load_fired && self.inflight <= 2 {
                        match idle_since {
                            Some(t) => t.elapsed() >= Duration::from_millis(500),
                            None => {
                                idle_since = Some(Instant::now());
                                false
                            }
                        }
                    } else {
                        idle_since = None;
                        false
                    }
                }
            };
            if ready {
                return Ok(());
            }
            if started.elapsed() > deadline {
                return Err(format!(
                    "Navigation timeout of {timeout_ms} ms exceeded"
                ));
            }
            match tokio::time::timeout(Duration::from_millis(100), self.read_one()).await {
                Ok(result) => result?,
                Err(_) => {}
            }
            if self.depth == 0 {
                self.drain_fetch().await?;
            }
        }
    }

    async fn evaluate(&mut self, session: &str, expression: &str) -> Result<Value, String> {
        let result = self
            .cmd(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": false,
                }),
                Some(session),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            let msg = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("evaluate gagal")
                .to_string();
            return Err(msg);
        }
        Ok(result["result"]["value"].clone())
    }
}

// ===== Sesi browser (satu proses Chromium per panggilan) =====

struct BrowserSession {
    child: tokio::process::Child,
    cdp: Cdp,
    user_data_dir: PathBuf,
    page_session: String,
    target_id: String,
}

async fn launch() -> Result<(tokio::process::Child, String, PathBuf), String> {
    let exe = find_chromium_path().ok_or_else(|| {
        "Chromium tidak ditemukan di sistem. Install: apt-get install -y chromium-browser"
            .to_string()
    })?;
    let dir = std::env::temp_dir().join(format!(
        "yuki-browser-{}",
        crate::auth::random_alnum(10).to_lowercase()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args([
        "--headless",
        "--no-sandbox",
        "--disable-setuid-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
        "--window-size=1280,900",
        "--remote-debugging-port=0",
    ]);
    // Opt-in eksplisit untuk lingkungan ber-proxy MITM (CA lokal tidak
    // dikenal Chrome): YUKI_BROWSER_IGNORE_CERT_ERRORS=1. Default mati
    // — sama seperti Puppeteer, error sertifikat tetap menggagalkan.
    if std::env::var("YUKI_BROWSER_IGNORE_CERT_ERRORS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        cmd.arg("--ignore-certificate-errors");
    }
    // Di belakang proxy perusahaan (env standar): teruskan ke Chromium
    // agar navigasi bisa keluar. Proxy ber-kredensial dilewati —
    // Chromium tidak bisa memakai kredensial dari URL --proxy-server.
    let proxy = std::env::var("HTTPS_PROXY")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("https_proxy").ok().filter(|v| !v.is_empty()));
    if let Some(proxy) = proxy {
        let authority = proxy.split("://").nth(1).unwrap_or(&proxy);
        if !authority.contains('@') {
            cmd.arg(format!("--proxy-server={proxy}"));
        }
    }
    cmd.arg(format!("--user-data-dir={}", dir.display()))
    .arg("about:blank")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("Gagal launch Chromium: {e}"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "stderr Chromium tidak tersedia".to_string())?;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    let ws_url = tokio::time::timeout(Duration::from_secs(15), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(rest) = line.strip_prefix("DevTools listening on ") {
                return Ok::<String, String>(rest.trim().to_string());
            }
        }
        Err("Chromium keluar sebelum DevTools siap".to_string())
    })
    .await
    .map_err(|_| "Timeout menunggu DevTools Chromium".to_string())??;
    // Kuras sisa stderr agar pipe tidak penuh.
    tokio::spawn(async move {
        let mut lines = lines;
        while let Ok(Some(_)) = lines.next_line().await {}
    });
    Ok((child, ws_url, dir))
}

async fn open_session(
    user_id: &str,
    block_heavy: bool,
    user_agent: Option<&str>,
    viewport: bool,
) -> Result<BrowserSession, String> {
    let (child, ws_url, dir) = launch().await?;
    let mut session = BrowserSession {
        child,
        cdp: Cdp::connect(&ws_url).await?,
        user_data_dir: dir,
        page_session: String::new(),
        target_id: String::new(),
    };
    session.cdp.block_heavy = block_heavy;
    let target = session
        .cdp
        .cmd("Target.createTarget", json!({ "url": "about:blank" }), None)
        .await?;
    session.target_id = target["targetId"].as_str().unwrap_or("").to_string();
    let attached = session
        .cdp
        .cmd(
            "Target.attachToTarget",
            json!({ "targetId": session.target_id, "flatten": true }),
            None,
        )
        .await?;
    session.page_session = attached["sessionId"].as_str().unwrap_or("").to_string();
    let sid = session.page_session.clone();
    session.cdp.cmd("Page.enable", json!({}), Some(&sid)).await?;
    session
        .cdp
        .cmd("Network.enable", json!({}), Some(&sid))
        .await?;
    session
        .cdp
        .cmd(
            "Fetch.enable",
            json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] }),
            Some(&sid),
        )
        .await?;
    let cookies = load_user_cookies(user_id);
    if !cookies.is_empty() {
        let params: Vec<Value> = cookies
            .iter()
            .map(|c| {
                let mut out = json!({
                    "name": c["name"], "value": c["value"], "domain": c["domain"],
                });
                for key in ["path", "expires", "httpOnly", "secure", "sameSite"] {
                    if !c[key].is_null() {
                        out[key] = c[key].clone();
                    }
                }
                out
            })
            .collect();
        let _ = session
            .cdp
            .cmd("Storage.setCookies", json!({ "cookies": params }), None)
            .await;
    }
    if let Some(ua) = user_agent {
        session
            .cdp
            .cmd(
                "Emulation.setUserAgentOverride",
                json!({ "userAgent": ua }),
                Some(&sid),
            )
            .await?;
    }
    if viewport {
        session
            .cdp
            .cmd(
                "Emulation.setDeviceMetricsOverride",
                json!({ "width": 1280, "height": 900, "deviceScaleFactor": 1, "mobile": false }),
                Some(&sid),
            )
            .await?;
    }
    Ok(session)
}

impl BrowserSession {
    async fn navigate(&mut self, url: &str, wait_until: &str, timeout_ms: u64) -> Result<(), String> {
        let sid = self.page_session.clone();
        let result = self
            .cdp
            .cmd("Page.navigate", json!({ "url": url }), Some(&sid))
            .await?;
        if let Some(err) = result.get("errorText").and_then(|v| v.as_str()) {
            if !err.is_empty() {
                return Err(err.to_string());
            }
        }
        self.cdp.wait_navigation(&sid, wait_until, timeout_ms).await
    }

    async fn close(mut self, user_id: &str) {
        if let Ok(result) = self.cdp.cmd("Storage.getCookies", json!({}), None).await {
            if let Some(cookies) = result["cookies"].as_array() {
                save_user_cookies(user_id, cookies);
            }
        }
        let _ = self
            .cdp
            .cmd(
                "Target.closeTarget",
                json!({ "targetId": self.target_id }),
                None,
            )
            .await;
        let _ = self.child.kill().await;
        let _ = std::fs::remove_dir_all(&self.user_data_dir);
    }
}

// ===== Tiga tool (kontrak lib/browser.js) =====

pub async fn browse_page(url: &str, wait_for: &str, user_id: &str) -> Value {
    let check = validate_safe_outbound_url(url).await;
    if !check.safe {
        return json!({ "error": format!("[Keamanan Jaringan]: {}", check.reason) });
    }
    let target_url = check.clean_url.clone();
    let started = Instant::now();
    let run = async {
        let mut session = open_session(user_id, true, Some(UA_BROWSE), false).await?;
        let wait = if ["domcontentloaded", "load", "networkidle0", "networkidle2"]
            .contains(&wait_for)
        {
            wait_for
        } else {
            "networkidle2"
        };
        session.navigate(&target_url, wait, 25_000).await?;
        let sid = session.page_session.clone();
        let title = session
            .cdp
            .evaluate(&sid, "document.title")
            .await
            .unwrap_or(Value::Null);
        let text = session
            .cdp
            .evaluate(
                &sid,
                "(() => { const remove = document.querySelectorAll('script, style, nav, footer, header, aside, .ad, [class*=\"cookie\"], [id*=\"cookie\"]'); remove.forEach(el => el.remove()); const body = document.body; return body ? body.innerText.replace(/\\s{3,}/g, '\\n\\n').trim().slice(0, 6000) : ''; })()",
            )
            .await
            .unwrap_or(Value::Null);
        let links = session
            .cdp
            .evaluate(
                &sid,
                "(() => Array.from(document.querySelectorAll('a[href]')).map(a => ({ text: a.innerText.trim().slice(0, 80), href: a.href })).filter(l => l.href.startsWith('http') && l.text.length > 2).slice(0, 15))()",
            )
            .await
            .unwrap_or_else(|_| json!([]));
        let out = json!({
            "url": target_url,
            "title": title.as_str().unwrap_or(""),
            "text": text.as_str().unwrap_or(""),
            "links": links,
            "durationMs": started.elapsed().as_millis() as u64,
        });
        session.close(user_id).await;
        Ok::<Value, String>(out)
    };
    match tokio::time::timeout(Duration::from_secs(60), run).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => json!({
            "error": format!("Gagal membuka halaman: {e}"),
            "url": target_url,
            "durationMs": started.elapsed().as_millis() as u64,
        }),
        Err(_) => json!({
            "error": "Gagal membuka halaman: operasi browser timeout",
            "url": target_url,
            "durationMs": started.elapsed().as_millis() as u64,
        }),
    }
}

pub async fn screenshot_url(url: &str, full_page: bool, user_id: &str) -> Value {
    let check = validate_safe_outbound_url(url).await;
    if !check.safe {
        return json!({ "error": format!("[Keamanan Jaringan]: {}", check.reason) });
    }
    let target_url = check.clean_url.clone();
    let run = async {
        let mut session = open_session(user_id, false, Some(UA_SCREENSHOT), true).await?;
        session.navigate(&target_url, "networkidle2", 20_000).await?;
        let sid = session.page_session.clone();
        let shot = session
            .cdp
            .cmd(
                "Page.captureScreenshot",
                json!({ "format": "jpeg", "quality": 80, "captureBeyondViewport": full_page }),
                Some(&sid),
            )
            .await?;
        let base64 = shot["data"].as_str().unwrap_or("").to_string();
        let out = json!({
            "url": target_url,
            "format": "jpeg",
            "base64": base64,
            "dataUrl": format!("data:image/jpeg;base64,{base64}"),
            "message": format!("Screenshot {target_url} berhasil diambil."),
        });
        session.close(user_id).await;
        Ok::<Value, String>(out)
    };
    match tokio::time::timeout(Duration::from_secs(50), run).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => json!({ "error": format!("Gagal screenshot: {e}") }),
        Err(_) => json!({ "error": "Gagal screenshot: operasi browser timeout" }),
    }
}

pub async fn browser_extract(url: &str, selector: &str, attribute: &str, user_id: &str) -> Value {
    let check = validate_safe_outbound_url(url).await;
    if !check.safe {
        return json!({ "error": format!("[Keamanan Jaringan]: {}", check.reason) });
    }
    let target_url = check.clean_url.clone();
    let run = async {
        let mut session = open_session(user_id, false, None, false).await?;
        session.navigate(&target_url, "domcontentloaded", 20_000).await?;
        let sid = session.page_session.clone();
        let expression = format!(
            "(() => {{ const sel = {}; const attr = {}; const elements = Array.from(document.querySelectorAll(sel)); return elements.slice(0, 20).map(el => {{ if (attr === 'text') return el.innerText?.trim() || el.textContent?.trim(); if (attr === 'href') return el.getAttribute('href'); if (attr === 'src') return el.getAttribute('src'); return el.getAttribute(attr); }}).filter(Boolean); }})()",
            serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into()),
            serde_json::to_string(attribute).unwrap_or_else(|_| "\"text\"".into()),
        );
        let results = session.cdp.evaluate(&sid, &expression).await?;
        let list = if results.is_array() {
            results
        } else {
            json!([])
        };
        let out = json!({
            "url": target_url,
            "selector": selector,
            "attribute": attribute,
            "results": list,
            "count": list.as_array().map(|a| a.len()).unwrap_or(0),
        });
        session.close(user_id).await;
        Ok::<Value, String>(out)
    };
    match tokio::time::timeout(Duration::from_secs(50), run).await {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => json!({ "error": format!("Gagal extract: {e}") }),
        Err(_) => json!({ "error": "Gagal extract: operasi browser timeout" }),
    }
}
