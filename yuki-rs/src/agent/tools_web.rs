//! Port handler research/* dan media/ryukomik-expert:
//! web_search (DuckDuckGo), read_url, http_api_request, komik Ryukomik,
//! dan browser-agent (fallback fetch — port CDP/Puppeteer ditunda,
//! lihat ruling Fase 4; pemeriksaan ketersediaan Chromium sama).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use crate::agent::security::validate_safe_outbound_url;
use crate::agent::tools::ToolContext;

fn err(message: &str) -> Value {
    json!({ "error": message })
}

const UA_BROWSER: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

// ===== web-search =====

fn search_tokens(value: &str) -> Vec<String> {
    const STOP: [&str; 16] = [
        "yang", "dan", "atau", "untuk", "dari", "di", "ke", "the", "a", "an", "of", "terbaru",
        "berita", "update", "cari", "riset",
    ];
    let normalized = value.to_lowercase();
    let cleaned: String = normalized
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c.is_whitespace() || c == '-' {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned
        .split_whitespace()
        .filter(|t| t.len() > 2 && !STOP.contains(t))
        .map(|s| s.to_string())
        .collect()
}

fn topic_words() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("anime", vec!["anime", "animasi", "episode", "season"]),
        ("game", vec!["game", "gaming", "mobile game", "playstation", "xbox", "nintendo"]),
        ("manga", vec!["manga", "manhwa", "manhua", "komik", "chapter"]),
        ("finance", vec!["saham", "crypto", "pasar", "harga", "ekonomi"]),
    ]
}

pub fn score_search_relevance(query: &str, result: &Value) -> f64 {
    let query_tokens: HashSet<String> = search_tokens(query).into_iter().collect();
    let haystack = format!(
        "{} {}",
        result["title"].as_str().unwrap_or(""),
        result["snippet"].as_str().unwrap_or("")
    )
    .to_lowercase();
    let overlap = query_tokens.iter().filter(|t| haystack.contains(t.as_str())).count();
    let mut score = if query_tokens.is_empty() {
        0.0
    } else {
        overlap as f64 / query_tokens.len() as f64
    };
    let topics = topic_words();
    let query_lower = query.to_lowercase();
    let wanted: Vec<&str> = topics
        .iter()
        .filter(|(_, words)| words.iter().any(|w| query_lower.contains(w)))
        .map(|(topic, _)| *topic)
        .collect();
    if !wanted.is_empty() {
        let matches_wanted = wanted.iter().any(|topic| {
            topics
                .iter()
                .find(|(t, _)| t == topic)
                .map(|(_, words)| words.iter().any(|w| haystack.contains(w)))
                .unwrap_or(false)
        });
        if !matches_wanted {
            score -= 0.55;
        }
        let conflicting: Vec<&str> = topics
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| !wanted.contains(t))
            .filter(|t| {
                topics
                    .iter()
                    .find(|(x, _)| x == t)
                    .map(|(_, words)| words.iter().any(|w| haystack.contains(w)))
                    .unwrap_or(false)
            })
            .collect();
        if !conflicting.is_empty() && !matches_wanted {
            score -= 0.25;
        }
    }
    (score.clamp(0.0, 1.0) * 100.0).round() / 100.0
}

fn publication_date(text: &str) -> Value {
    let re = Regex::new(r"(?i)\b(20\d{2}-\d{2}-\d{2}|\d{1,2}\s+(?:Jan|Feb|Mar|Apr|Mei|Jun|Jul|Agu|Sep|Okt|Nov|Des)[a-z]*\s+20\d{2})\b").unwrap();
    match re.captures(text) {
        Some(c) => json!(c[1].to_string()),
        None => Value::Null,
    }
}

fn strip_tags(html: &str) -> String {
    Regex::new(r"<[^>]+>").unwrap().replace_all(html, "").trim().to_string()
}

/// Buang blok <script>/<style>/dll beserta isinya sebelum ekstraksi teks.
/// Pola JS asli memakai negative look-ahead (?!:</tag>) yang TIDAK
/// didukung crate `regex` — Regex::new-nya gagal dan unwrap() panik di
/// setiap read_url (bug report VPS 2026-11-09). Non-greedy berhenti di
/// tag penutup pertama; setara untuk halaman umum (tag sejenis bersarang
/// sangat jarang).
fn strip_block_tags(html: &str) -> String {
    let mut cleaned = html.to_string();
    for tag in ["script", "style", "nav", "footer", "header", "svg", "noscript"] {
        let re = Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}>")).unwrap();
        cleaned = re.replace_all(&cleaned, "").to_string();
    }
    cleaned
}

fn url_encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        let c = byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

async fn web_search(args: &Value, ctx: &ToolContext) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    if query.trim().is_empty() {
        return err("Query pencarian tidak boleh kosong.");
    }
    let clean_query = query.trim().to_string();
    let limit = args["max_results"].as_i64().unwrap_or(4).clamp(1, 6) as usize;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| ctx.http.clone());
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        url_encode_component(&clean_query)
    );
    let response = client
        .get(&url)
        .header("User-Agent", UA_BROWSER)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "id-ID,id;q=0.9,en-US;q=0.8,en;q=0.7")
        .send()
        .await;
    let html = match response {
        Ok(res) if res.status().is_success() => res.text().await.unwrap_or_default(),
        Ok(res) => {
            return json!({ "query": clean_query, "error": format!("Gagal melakukan pencarian web: DuckDuckGo returned status {}", res.status().as_u16()) });
        }
        Err(e) => {
            return json!({ "query": clean_query, "error": format!("Gagal melakukan pencarian web: {e}") });
        }
    };
    let split_re = Regex::new(r#"(?i)class="result\s+results_links"#).unwrap();
    let blocks: Vec<&str> = split_re.split(&html).skip(1).collect();
    let mut results: Vec<Value> = Vec::new();
    for block in blocks {
        if results.len() >= limit {
            break;
        }
        let title_match = Regex::new(r#"(?is)<a class="result__url"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#)
            .unwrap()
            .captures(block)
            .or_else(|| {
                Regex::new(r#"(?is)<a class="result__snippet[^"]*"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#)
                    .unwrap()
                    .captures(block)
            })
            .or_else(|| {
                Regex::new(r#"(?is)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>([\s\S]*?)</a>"#)
                    .unwrap()
                    .captures(block)
            });
        let snippet_match = Regex::new(r#"(?is)<a[^>]*class="result__snippet"[^>]*>([\s\S]*?)</a>"#)
            .unwrap()
            .captures(block)
            .or_else(|| {
                Regex::new(r#"(?is)class="result__snippet"[^>]*>([\s\S]*?)</div>"#)
                    .unwrap()
                    .captures(block)
            });
        let mut link = title_match.as_ref().map(|c| c[1].to_string()).unwrap_or_default();
        let title = title_match.as_ref().map(|c| strip_tags(&c[2])).unwrap_or_default();
        let snippet = snippet_match.as_ref().map(|c| strip_tags(&c[1])).unwrap_or_default();
        if link.contains("uddg=") {
            if let Some(raw) = link.split("uddg=").nth(1).and_then(|s| s.split('&').next()) {
                if let Ok(decoded) = urlencoding_decode(raw) {
                    link = decoded;
                }
            }
        }
        if !title.is_empty() && !snippet.is_empty() {
            results.push(json!({ "title": title, "snippet": snippet.chars().take(300).collect::<String>(), "url": link }));
        }
    }
    if !results.is_empty() {
        let ranked = rank_results(&clean_query, results.clone(), limit);
        return json!({
            "query": clean_query,
            "total": ranked.len(),
            "rejectedIrrelevant": results.len() - ranked.len(),
            "results": ranked,
            "relevanceStatus": if ranked.is_empty() { "no_relevant_results" } else { "verified" },
        });
    }
    // Fallback Instant Answer
    let api_url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        url_encode_component(&clean_query)
    );
    if let Ok(res) = client.get(&api_url).send().await {
        if res.status().is_success() {
            if let Ok(data) = res.json::<Value>().await {
                let mut fallback: Vec<Value> = Vec::new();
                if let Some(abstract_text) = data["AbstractText"].as_str() {
                    if !abstract_text.is_empty() {
                        fallback.push(json!({
                            "title": data["Heading"].as_str().unwrap_or(&clean_query),
                            "snippet": abstract_text,
                            "url": data["AbstractURL"].as_str().unwrap_or(""),
                        }));
                    }
                }
                if let Some(topics) = data["RelatedTopics"].as_array() {
                    for item in topics.iter().take(limit) {
                        if let (Some(text), Some(url)) = (item["Text"].as_str(), item["FirstURL"].as_str()) {
                            fallback.push(json!({
                                "title": text.chars().take(60).collect::<String>(),
                                "snippet": text,
                                "url": url,
                            }));
                        }
                    }
                }
                if !fallback.is_empty() {
                    let total_fallback = fallback.len();
                    let ranked = rank_results(&clean_query, fallback, limit);
                    return json!({
                        "query": clean_query,
                        "total": ranked.len(),
                        "rejectedIrrelevant": total_fallback - ranked.len(),
                        "results": ranked,
                        "relevanceStatus": if ranked.is_empty() { "no_relevant_results" } else { "verified" },
                    });
                }
            }
        }
    }
    json!({
        "query": clean_query,
        "total": 0,
        "results": [],
        "message": format!("Tidak ditemukan hasil spesifik untuk \"{clean_query}\". Coba kata kunci yang lebih umum."),
    })
}

fn rank_results(query: &str, results: Vec<Value>, limit: usize) -> Vec<Value> {
    let mut ranked: Vec<Value> = results
        .into_iter()
        .map(|mut result| {
            let text = format!(
                "{} {}",
                result["title"].as_str().unwrap_or(""),
                result["snippet"].as_str().unwrap_or("")
            );
            result["publishedAt"] = publication_date(&text);
            result["relevance"] = json!(score_search_relevance(query, &result));
            result
        })
        .filter(|r| {
            r["relevance"].as_f64().unwrap_or(0.0) >= 0.25
                && Regex::new(r"(?i)^https?://").unwrap().is_match(r["url"].as_str().unwrap_or(""))
        })
        .collect();
    ranked.sort_by(|a, b| {
        b["relevance"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["relevance"].as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit);
    ranked
}

fn urlencoding_decode(value: &str) -> Result<String, ()> {
    let mut out: Vec<u8> = Vec::new();
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| ())?;
            let byte = u8::from_str_radix(hex, 16).map_err(|_| ())?;
            out.push(byte);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

// ===== url-reader =====

fn no_redirect_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_default()
}

async fn read_url(args: &Value) -> Value {
    let url = args["url"].as_str().unwrap_or("");
    if url.is_empty() || !url.starts_with("http") {
        return err("URL tidak valid. Harus diawali dengan http:// atau https://");
    }
    let check = validate_safe_outbound_url(url).await;
    if !check.safe {
        return err(&format!("[Keamanan Jaringan]: {}", check.reason));
    }
    let client = no_redirect_client(12);
    let mut current_url = check.clean_url;
    let mut res: Option<reqwest::Response> = None;
    for hop in 0..=5 {
        let response = client
            .get(&current_url)
            .header("User-Agent", UA_BROWSER)
            .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => return err(&format!("Gagal membaca halaman: {e}")),
        };
        let status = response.status().as_u16();
        if ![301, 302, 303, 307, 308].contains(&status) {
            res = Some(response);
            break;
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let Some(location) = location else {
            res = Some(response);
            break;
        };
        let next = match url::Url::parse(&current_url).and_then(|base| base.join(&location)) {
            Ok(u) => u.to_string(),
            Err(_) => {
                res = Some(response);
                break;
            }
        };
        let hop_check = validate_safe_outbound_url(&next).await;
        if !hop_check.safe {
            return err(&format!("[Keamanan Jaringan]: Redirect diblokir — {}", hop_check.reason));
        }
        current_url = hop_check.clean_url;
        if hop == 5 {
            return err("Gagal membaca halaman: terlalu banyak redirect.");
        }
        res = Some(response);
    }
    let Some(res) = res else {
        return err("Gagal membaca halaman: tidak ada respons.");
    };
    if !res.status().is_success() {
        return err(&format!(
            "Gagal mengakses URL ({}): {}",
            res.status().as_u16(),
            res.status().canonical_reason().unwrap_or("")
        ));
    }
    let html = res.text().await.unwrap_or_default();
    let title = Regex::new(r"(?is)<title[^>]*>([\s\S]*?)</title>")
        .unwrap()
        .captures(&html)
        .map(|c| strip_tags(&c[1]))
        .unwrap_or_default();
    let mut cleaned = strip_block_tags(&html);
    cleaned = Regex::new(r"<[^>]+>").unwrap().replace_all(&cleaned, " ").to_string();
    cleaned = cleaned
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"");
    cleaned = Regex::new(r"\s{2,}").unwrap().replace_all(&cleaned, " ").trim().to_string();
    let preview: String = cleaned.chars().take(5000).collect();
    let looks_js = cleaned.chars().count() < 160
        || Regex::new(r"(?i)enable javascript|javascript is required|loading\.\.\.")
            .unwrap()
            .is_match(&cleaned);
    json!({
        "url": url,
        "title": if title.is_empty() { "Halaman Web".to_string() } else { title },
        "content": preview,
        "length": preview.chars().count(),
        "truncated": cleaned.chars().count() > 5000,
        "extraction": "static_html",
        "browserFallback": if looks_js {
            json!({ "required": true, "reason": "Konten statis kosong atau hanya shell JavaScript." })
        } else {
            json!({ "required": false, "reason": "Konten HTML statis terbaca." })
        },
    })
}

// ===== api-tester =====

async fn http_api_request(args: &Value) -> Value {
    let url_in = args["url"].as_str().unwrap_or("");
    if url_in.is_empty() {
        return err("URL target tidak boleh kosong.");
    }
    let mut target = url_in.trim().to_string();
    if !Regex::new(r"(?i)^https?://").unwrap().is_match(&target) {
        target = format!("https://{target}");
    }
    let check = validate_safe_outbound_url(&target).await;
    if !check.safe {
        return err(&format!("[Keamanan Jaringan]: {}", check.reason));
    }
    target = check.clean_url;
    let parsed = url::Url::parse(&target);
    let port_num = match &parsed {
        Ok(u) => u.port().unwrap_or(if u.scheme() == "https" { 443 } else { 80 }),
        Err(_) => return err("URL target tidak valid."),
    };
    if ![80, 443].contains(&port_num) {
        return err(&format!("Port :{port_num} tidak diizinkan untuk uji API. Gunakan port standar 80/443."));
    }
    let method = args["method"].as_str().unwrap_or("GET").to_uppercase();
    const ALLOWED_METHODS: [&str; 6] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD"];
    if !ALLOWED_METHODS.contains(&method.as_str()) {
        return err(&format!(
            "Metode HTTP \"{method}\" tidak didukung. Gunakan: {}",
            ALLOWED_METHODS.join(", ")
        ));
    }
    const ALLOWED_HEADERS: [&str; 6] = [
        "accept", "accept-language", "content-type", "authorization", "x-api-key", "api-key",
    ];
    let mut headers: Vec<(String, String)> = vec![("User-Agent".into(), "Yuki-Agent-API-Tester/1.0".into())];
    if let Some(map) = args["headers"].as_object() {
        for (k, v) in map {
            if ALLOWED_HEADERS.contains(&k.to_lowercase().as_str()) {
                let value = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                headers.push((k.clone(), value.chars().take(500).collect()));
            }
        }
    }
    let body_text: Option<String> = match &args["body"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(serde_json::to_string(other).unwrap_or_default()),
    };
    let client = no_redirect_client(8);
    let started = Instant::now();
    let mut current_url = target.clone();
    let mut redirect_chain: Vec<Value> = Vec::new();
    let mut response: Option<reqwest::Response> = None;
    for redirects in 0..=5 {
        let mut req = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
            &current_url,
        );
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        if let Some(body) = &body_text {
            if ["POST", "PUT", "PATCH"].contains(&method.as_str()) {
                let has_ct = headers.iter().any(|(k, _)| k.to_lowercase() == "content-type");
                if !has_ct && (args["body"].is_object() || body.trim_start().starts_with('{')) {
                    req = req.header("Content-Type", "application/json");
                }
                req = req.body(body.clone());
            }
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let duration = started.elapsed().as_millis() as i64;
                let message = if e.is_timeout() {
                    "Request timeout setelah 8 detik.".to_string()
                } else {
                    format!("Koneksi gagal: {e}")
                };
                return json!({ "success": false, "url": target, "method": method, "durationMs": duration, "error": message });
            }
        };
        let status = res.status().as_u16();
        if ![301, 302, 303, 307, 308].contains(&status) {
            response = Some(res);
            break;
        }
        let location = res.headers().get("location").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
        let Some(location) = location else {
            response = Some(res);
            break;
        };
        let next = match url::Url::parse(&current_url).and_then(|base| base.join(&location)) {
            Ok(u) => u.to_string(),
            Err(_) => {
                response = Some(res);
                break;
            }
        };
        let redirect_check = validate_safe_outbound_url(&next).await;
        if !redirect_check.safe {
            return json!({ "success": false, "error": format!("[Keamanan Redirect]: {}", redirect_check.reason), "redirectChain": redirect_chain });
        }
        let same_origin = url::Url::parse(&next).map(|u| u.origin().ascii_serialization())
            == url::Url::parse(&current_url).map(|u| u.origin().ascii_serialization());
        if !same_origin {
            headers.retain(|(k, _)| !["authorization", "x-api-key", "api-key"].contains(&k.to_lowercase().as_str()));
        }
        redirect_chain.push(json!({ "status": status, "from": current_url, "to": redirect_check.clean_url }));
        current_url = redirect_check.clean_url;
        if redirects == 5 {
            return json!({ "success": false, "error": "Terlalu banyak redirect (maksimal 5).", "redirectChain": redirect_chain });
        }
        response = Some(res);
    }
    let Some(res) = response else {
        return json!({ "success": false, "url": target, "method": method, "durationMs": started.elapsed().as_millis() as i64, "error": "Koneksi gagal: tidak ada respons." });
    };
    let duration = started.elapsed().as_millis() as i64;
    let status = res.status();
    let mut res_headers = json!({});
    for (k, v) in res.headers().iter() {
        let key = k.as_str().to_lowercase();
        if ["content-type", "content-length", "server", "x-powered-by", "date"].contains(&key.as_str()) {
            res_headers[key] = json!(v.to_str().unwrap_or(""));
        }
    }
    let text = res.text().await.unwrap_or_default();
    let parsed_json: Option<Value> = serde_json::from_str(&text).ok();
    let truncated = if text.chars().count() > 2500 {
        format!("{}... (output terpotong)", text.chars().take(2500).collect::<String>())
    } else {
        text.clone()
    };
    let mut out = json!({
        "success": true,
        "url": current_url,
        "requestedUrl": target,
        "method": method,
        "status": status.as_u16(),
        "statusText": status.canonical_reason().unwrap_or(""),
        "durationMs": duration,
        "ok": status.is_success(),
        "responseSize": text.len(),
        "redirectChain": redirect_chain,
        "headers": res_headers,
        "json": parsed_json.clone().unwrap_or(Value::Null),
    });
    if parsed_json.is_none() {
        out["bodyPreview"] = json!(truncated);
    }
    out
}

// ===== ryukomik =====

fn ryukomik_api() -> String {
    std::env::var("RYUKOMIK_API")
        .unwrap_or_else(|_| "https://api.ryukomik.web.id".into())
        .trim_end_matches('/')
        .to_string()
}

fn ryukomik_adult_default() -> bool {
    std::env::var("RYUKOMIK_ADULT")
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false)
}

fn ryukomik_max() -> usize {
    std::env::var("RYUKOMIK_MAX_RESULTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

static COMIC_CACHE: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);

fn cache_get(key: &str) -> Option<Value> {
    let guard = COMIC_CACHE.lock().unwrap();
    let map = guard.as_ref()?;
    let (at, value) = map.get(key)?;
    if at.elapsed() < Duration::from_millis(60_000) {
        Some(value.clone())
    } else {
        None
    }
}

fn cache_set(key: &str, value: Value) {
    let mut guard = COMIC_CACHE.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(key.to_string(), (Instant::now(), value));
}

async fn comic_api_get(ctx: &ToolContext, path: &str) -> Result<Value, String> {
    let url = format!("{}{}", ryukomik_api(), path);
    let res = ctx
        .http
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; YukiBot/1.0)")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("ryukomik api {}", res.status().as_u16()));
    }
    res.json::<Value>().await.map_err(|e| e.to_string())
}

fn to_local_url(url: &str) -> String {
    if let Some(c) = Regex::new(r"(?i)komiku\.org/manga/([^/?#]+)").unwrap().captures(url) {
        return format!("https://ryukomik.my.id/komik/komiku/{}", &c[1]);
    }
    if let Some(c) = Regex::new(r"(?i)doujindesu\.[a-z]+/manga/([^/?#]+)").unwrap().captures(url) {
        return format!("https://ryukomik.my.id/komik/doujindesu/{}", &c[1]);
    }
    url.to_string()
}

pub fn detect_comic_format(item: &Value) -> &'static str {
    let explicit = [
        item["format"].as_str().unwrap_or(""),
        item["comic_type"].as_str().unwrap_or(""),
        item["country"].as_str().unwrap_or(""),
        item["origin"].as_str().unwrap_or(""),
    ]
    .join(" ")
    .to_lowercase();
    for (needle, normalized) in [
        ("manhwa", "MANHWA"), ("korea", "MANHWA"), ("korean", "MANHWA"),
        ("manhua", "MANHUA"), ("china", "MANHUA"), ("chinese", "MANHUA"),
        ("manga", "MANGA"), ("japan", "MANGA"), ("japanese", "MANGA"),
    ] {
        if explicit.contains(needle) {
            return normalized;
        }
    }
    let haystack = format!(
        "{} {} {} {}",
        item["type"].as_str().unwrap_or(""),
        item["type_genre"].as_str().unwrap_or(""),
        item["genre"].as_str().unwrap_or(""),
        item["link"].as_str().unwrap_or("")
    )
    .to_lowercase();
    if Regex::new(r"\bmanhwa\b|\bkorea(?:n)?\b").unwrap().is_match(&haystack) {
        return "MANHWA";
    }
    if Regex::new(r"\bmanhua\b|\bchin(?:a|ese)\b").unwrap().is_match(&haystack) {
        return "MANHUA";
    }
    if Regex::new(r"\bmanga\b|\bjapan(?:ese)?\b").unwrap().is_match(&haystack) {
        return "MANGA";
    }
    "KOMIK"
}

fn normalize_comic(x: &Value) -> Value {
    let format = detect_comic_format(x);
    let score_raw = Regex::new(r"[^\d.]")
        .unwrap()
        .replace_all(x["score"].as_str().unwrap_or(""), "")
        .trim()
        .to_string();
    let score = if !score_raw.is_empty() {
        score_raw
            .parse::<f64>()
            .map(|n| format!("{n:.1}"))
            .unwrap_or(score_raw)
    } else {
        score_raw
    };
    let chapter = Regex::new(r"(?i)^Terbaru:\s*")
        .unwrap()
        .replace(x["chapter_terbaru"].as_str().or_else(|| x["chapterLast"].as_str()).unwrap_or(""), "")
        .trim()
        .to_string();
    let updated_text = x["waktu"]
        .as_str()
        .or_else(|| x["updated"].as_str())
        .or_else(|| x["update"].as_str())
        .unwrap_or("");
    let updated = Regex::new(r"(?i)^Update\s*")
        .unwrap()
        .replace(updated_text, "")
        .trim_end_matches('.')
        .trim()
        .to_string();
    json!({
        "title": x["title"].as_str().unwrap_or("").trim(),
        "url": to_local_url(x["link"].as_str().or_else(|| x["detail_link"].as_str()).unwrap_or("")),
        "type": x["genre"].as_str().or_else(|| x["type_genre"].as_str()).or_else(|| x["typeGenre"].as_str()).unwrap_or("").trim(),
        "chapter": chapter,
        "updated": updated,
        "score": score,
        "format": format,
        "image": x["image"].clone(),
    })
}

fn map_comic_list(data: &[Value], limit: usize) -> Vec<Value> {
    data.iter()
        .map(normalize_comic)
        .filter(|x| !x["title"].as_str().unwrap_or("").is_empty() && !x["url"].as_str().unwrap_or("").is_empty())
        .take(limit)
        .collect()
}

fn genre_table() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("action", vec!["action", "aksi"]), ("adventure", vec!["adventure", "petualangan"]),
        ("boys-love", vec!["boys love", "boy's love", "bl", "shounen ai"]),
        ("comedy", vec!["comedy", "komedi"]), ("crime", vec!["crime", "kriminal"]),
        ("drama", vec!["drama"]), ("ecchi", vec!["ecchi"]), ("fantasy", vec!["fantasy", "fantasi"]),
        ("girls-love", vec!["girls love", "girl's love", "gl", "shoujo ai"]),
        ("harem", vec!["harem"]), ("historical", vec!["historical", "sejarah"]),
        ("horror", vec!["horror", "horor"]), ("isekai", vec!["isekai"]), ("josei", vec!["josei"]),
        ("martial-arts", vec!["martial arts", "bela diri"]), ("mecha", vec!["mecha"]),
        ("medical", vec!["medical", "medis"]), ("music", vec!["music", "musik"]),
        ("mystery", vec!["mystery", "misteri"]), ("psychological", vec!["psychological", "psikologi"]),
        ("romance", vec!["romance", "romantic", "romantis"]), ("school-life", vec!["school life", "sekolah"]),
        ("sci-fi", vec!["sci fi", "sci-fi", "science fiction", "fiksi ilmiah"]),
        ("seinen", vec!["seinen"]), ("shoujo", vec!["shoujo"]), ("shounen", vec!["shounen"]),
        ("slice-of-life", vec!["slice of life", "kehidupan sehari hari"]),
        ("sports", vec!["sports", "olahraga"]), ("supernatural", vec!["supernatural", "supranatural"]),
        ("thriller", vec!["thriller"]), ("tragedy", vec!["tragedy", "tragedi"]),
        ("wuxia", vec!["wuxia"]), ("yuri", vec!["yuri"]),
    ]
}

fn clean_words(value: &str) -> String {
    let lower = value.to_lowercase();
    let cleaned: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned.trim().to_string()
}

pub fn detect_requested_genre(text: &str) -> Option<(String, Vec<String>)> {
    let haystack = format!(" {} ", clean_words(text));
    for (slug, aliases) in genre_table() {
        if let Some(alias) = aliases.iter().find(|a| haystack.contains(&format!(" {} ", clean_words(a)))) {
            let _ = alias;
            return Some((slug.to_string(), aliases.iter().map(|s| s.to_string()).collect()));
        }
    }
    None
}

fn matches_genre(comic: &Value, genre: &Option<(String, Vec<String>)>) -> bool {
    let Some((_, aliases)) = genre else {
        return true;
    };
    let metadata = format!(" {} ", clean_words(comic["type"].as_str().unwrap_or("")));
    aliases.iter().any(|a| metadata.contains(&format!(" {} ", clean_words(a))))
}

async fn genre_comics(ctx: &ToolContext, slug: &str, aliases: &[String]) -> Vec<Value> {
    let key = format!("genre:{slug}");
    if let Some(cached) = cache_get(&key) {
        return cached.as_array().cloned().unwrap_or_default();
    }
    let genre = Some((slug.to_string(), aliases.to_vec()));
    let mut all: Vec<Value> = Vec::new();
    for page in 1..=3 {
        match comic_api_get(ctx, &format!("/genre/{slug}?page={page}")).await {
            Ok(data) => {
                if let Some(results) = data["results"].as_array() {
                    all.extend(results.iter().cloned());
                }
            }
            Err(_) => return Vec::new(),
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let list: Vec<Value> = map_comic_list(&all, 60)
        .into_iter()
        .filter(|item| matches_genre(item, &genre))
        .filter(|item| seen.insert(item["url"].as_str().unwrap_or("").to_string()))
        .take(ryukomik_max())
        .collect();
    cache_set(&key, json!(list));
    list
}

pub async fn search_comics(ctx: &ToolContext, query: &str, adult: bool) -> Vec<Value> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    if !adult {
        if let Some((slug, aliases)) = detect_requested_genre(q) {
            return genre_comics(ctx, &slug, &aliases).await;
        }
    }
    let key = format!("{}{}", if adult { "qx:" } else { "q:" }, q.to_lowercase());
    if let Some(cached) = cache_get(&key) {
        return cached.as_array().cloned().unwrap_or_default();
    }
    let path = if adult {
        format!("/doujindesu/search?q={}", url_encode_component(q))
    } else {
        format!("/komiku/search?q={}", url_encode_component(q))
    };
    let out = match comic_api_get(ctx, &path).await {
        Ok(data) => map_comic_list(data["data"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), ryukomik_max()),
        Err(_) => Vec::new(),
    };
    cache_set(&key, json!(out));
    out
}

pub async fn latest_comics(ctx: &ToolContext, genre_input: &str) -> Vec<Value> {
    let genre = detect_requested_genre(genre_input);
    let cache_key = match &genre {
        Some((slug, _)) => format!("latest:{slug}"),
        None => "latest".to_string(),
    };
    if let Some(cached) = cache_get(&cache_key) {
        return cached.as_array().cloned().unwrap_or_default();
    }
    let out = match comic_api_get(ctx, "/komiku/terbaru").await {
        Ok(data) => map_comic_list(data["data"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), 60)
            .into_iter()
            .filter(|item| matches_genre(item, &genre))
            .take(ryukomik_max())
            .collect(),
        Err(_) => Vec::new(),
    };
    cache_set(&cache_key, json!(out));
    out
}

fn map_comic_out(list: &[Value], count: usize) -> Vec<Value> {
    list.iter()
        .take(count)
        .map(|c| {
            json!({
                "title": c["title"], "url": c["url"],
                "format": if c["format"].as_str().unwrap_or("").is_empty() { json!("KOMIK") } else { c["format"].clone() },
                "type": c["type"], "chapter": c["chapter"], "updated": c["updated"],
                "score": c["score"], "image": c["image"],
            })
        })
        .collect()
}

async fn search_ryukomik(args: &Value, ctx: &ToolContext) -> Value {
    let query = args["query"].as_str().unwrap_or("");
    if query.trim().is_empty() {
        return err("Query pencarian komik tidak boleh kosong.");
    }
    let requested_format = {
        let given = args["format"].as_str().unwrap_or("");
        let detected = if !given.is_empty() {
            given.to_string()
        } else if Regex::new(r"(?i)\bmanhwa\b").unwrap().is_match(query) {
            "MANHWA".into()
        } else if Regex::new(r"(?i)\bmanhua\b").unwrap().is_match(query) {
            "MANHUA".into()
        } else if Regex::new(r"(?i)\bmanga\b").unwrap().is_match(query) {
            "MANGA".into()
        } else {
            String::new()
        };
        detected.to_uppercase()
    };
    let count = args["limit"].as_i64().unwrap_or(6).clamp(1, 10) as usize;
    let adult = args["filter_special"].as_bool().unwrap_or(false);
    let list = search_comics(ctx, query.trim(), adult).await;
    let filtered: Vec<Value> = list
        .into_iter()
        .filter(|c| requested_format.is_empty() || c["format"].as_str().unwrap_or("") == requested_format)
        .collect();
    json!({
        "query": query.trim(),
        "total": filtered.len(),
        "requestedLimit": count,
        "comics": map_comic_out(&filtered, count),
    })
}

async fn get_latest_comics_tool(args: &Value, ctx: &ToolContext) -> Value {
    let genre = args["genre"].as_str().unwrap_or("");
    let list = latest_comics(ctx, genre).await;
    let count = args["limit"].as_i64().unwrap_or(6).clamp(1, 10) as usize;
    json!({
        "total": list.len(),
        "comics": map_comic_out(&list, count),
    })
}

// ===== browser-agent =====

pub fn is_browser_available() -> bool {
    crate::agent::browser_cdp::find_chromium_path().is_some()
}

async fn fetch_page_fallback(url: &str) -> Value {
    let check = validate_safe_outbound_url(url).await;
    if !check.safe {
        return err(&format!("[Keamanan Jaringan]: {}", check.reason));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default();
    let res = client
        .get(&check.clean_url)
        .header("User-Agent", "Mozilla/5.0 Yuki-Agent/1.0")
        .send()
        .await;
    match res {
        Ok(res) => {
            let html = res.text().await.unwrap_or_default();
            let mut text = Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap().replace_all(&html, "").to_string();
            text = Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap().replace_all(&text, "").to_string();
            text = Regex::new(r"<[^>]+>").unwrap().replace_all(&text, " ").to_string();
            text = Regex::new(r"\s{3,}").unwrap().replace_all(&text, "\n").trim().to_string();
            let title = Regex::new(r"(?is)<title[^>]*>([^<]+)</title>")
                .unwrap()
                .captures(&html)
                .map(|c| c[1].trim().to_string())
                .unwrap_or_default();
            json!({
                "url": check.clean_url,
                "title": title,
                "text": text.chars().take(5000).collect::<String>(),
                "fallback": true,
            })
        }
        Err(e) => err(&format!("Fetch fallback gagal: {e}")),
    }
}

pub async fn dispatch_web(tool_name: &str, args: &Value, ctx: &ToolContext) -> Value {
    match tool_name {
        "web_search" => web_search(args, ctx).await,
        "read_url" => read_url(args).await,
        "http_api_request" => http_api_request(args).await,
        "search_ryukomik" => search_ryukomik(args, ctx).await,
        "get_latest_comics" => get_latest_comics_tool(args, ctx).await,
        "browse_page" => {
            let url = args["url"].as_str().unwrap_or("");
            if url.is_empty() {
                return err("URL wajib diisi.");
            }
            // Sama seperti handler Node: tanpa Chromium -> fetch
            // fallback; dengan Chromium -> browser CDP asli (Fase 8).
            if !is_browser_available() {
                return fetch_page_fallback(url).await;
            }
            let wait_for = args["wait_for"].as_str().unwrap_or("networkidle2");
            crate::agent::browser_cdp::browse_page(url, wait_for, &ctx.user_id).await
        }
        "screenshot_url" => {
            let url = args["url"].as_str().unwrap_or("");
            if url.is_empty() {
                return err("URL wajib diisi.");
            }
            if !is_browser_available() {
                return err("Browser tidak tersedia di sistem ini. Tidak bisa mengambil screenshot.");
            }
            let full_page = args["full_page"].as_bool().unwrap_or(false);
            crate::agent::browser_cdp::screenshot_url(url, full_page, &ctx.user_id).await
        }
        "browser_extract" => {
            let url = args["url"].as_str().unwrap_or("");
            if url.is_empty() {
                return err("URL wajib diisi.");
            }
            let selector = args["selector"].as_str().unwrap_or("");
            if selector.is_empty() {
                return err("CSS selector wajib diisi.");
            }
            if !is_browser_available() {
                return err("Browser tidak tersedia di sistem ini.");
            }
            let attribute = args["attribute"].as_str().unwrap_or("text");
            crate::agent::browser_cdp::browser_extract(url, selector, attribute, &ctx.user_id).await
        }
        _ => err(&format!("Tool \"{tool_name}\" tidak ditemukan atau belum terdaftar.")),
    }
}

pub fn adult_default() -> bool {
    ryukomik_adult_default()
}

#[cfg(test)]
mod tests {
    use super::strip_block_tags;

    #[test]
    fn strip_block_tags_tanpa_lookaround() {
        // Regresi: pola lama memakai negative look-ahead (?!:</tag>) yang
        // tidak didukung crate regex -> Regex::new gagal -> unwrap panik
        // di setiap read_url (bug report VPS 2026-11-09).
        let html = "<html><head><style>body{color:red}</style>\
            <script>if (a<b) { go(); }</script></head>\
            <body><nav>Menu</nav><p>Halo dunia</p>\
            <footer>Kaki</footer><script src=\"x.js\"></script></body></html>";
        let out = strip_block_tags(html);
        assert!(out.contains("Halo dunia"), "teks utama hilang: {out}");
        assert!(!out.contains("color:red"), "style bocor: {out}");
        assert!(!out.contains("go();"), "script bocor: {out}");
        assert!(!out.contains("Menu"), "nav bocor: {out}");
        assert!(!out.contains("Kaki"), "footer bocor: {out}");
    }
}
