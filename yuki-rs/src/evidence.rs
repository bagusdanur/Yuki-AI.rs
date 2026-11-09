//! Port `lib/web-evidence.js` (Fase 5): fetch bukti situs eksternal
//! (status + header pilihan + penanda techstack) untuk /api/analyze-url.
//! URL difilter validateSafeOutboundUrl yang sama seperti Node.

use serde_json::{json, Value};

const MAX_HTML: usize = 200 * 1024;
const KEEP_HEADERS: [&str; 5] = [
    "server",
    "x-powered-by",
    "cf-cache-status",
    "cf-ray",
    "content-type",
];

fn detect_markers(html: &str, headers: &[(String, String)]) -> Value {
    let h = html.to_lowercase();
    let hdrs = headers
        .iter()
        .map(|(k, v)| format!("{}: {}", k.to_lowercase(), v.to_lowercase()))
        .collect::<Vec<_>>()
        .join("\n");
    let has = |hay: &str, needles: &[&str]| needles.iter().any(|n| hay.contains(n));
    json!({
        "cloudflare": has(&hdrs, &["cf-cache-status", "cf-ray"]),
        "svelte": has(&h, &["svelte", "__sveltekit", "data-sveltekit"]),
        "laravel": has(&hdrs, &["laravel_session", "x-xsrf-token"]) || hdrs.contains("x-powered-by: php") || h.contains("laravel"),
        "vite": has(&h, &["@vite", "/assets/index-"]),
        "tailwind": h.contains("tailwind"),
    })
}

pub async fn fetch_site_evidence(_http: &reqwest::Client, raw_url: &str) -> Value {
    let check = crate::agent::security::validate_safe_outbound_url(raw_url).await;
    if !check.safe {
        let reason = if check.reason.is_empty() { "URL ditolak.".to_string() } else { check.reason };
        return json!({ "ok": false, "reason": reason });
    }
    let clean_url = check.clean_url;
    // Node: redirect manual + timeout 10 detik — klien khusus di sini.
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    });
    let response = client
        .get(&clean_url)
        .header("User-Agent", "Mozilla/5.0 (compatible; YukiEvidence/1.0)")
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.is_timeout() {
                "Fetch gagal: timeout 10 detik".to_string()
            } else {
                format!("Fetch gagal: {}", e.to_string().chars().take(200).collect::<String>())
            };
            return json!({ "ok": false, "reason": reason });
        }
    };
    let status = response.status().as_u16();
    let mut headers: Vec<(String, String)> = Vec::new();
    for name in KEEP_HEADERS {
        if let Some(v) = response.headers().get(name).and_then(|v| v.to_str().ok()) {
            headers.push((name.to_string(), v.chars().take(200).collect()));
        }
    }
    let html = response.text().await.unwrap_or_default();
    let truncated = html.len() > MAX_HTML;
    let html_kept: String = if truncated {
        html.chars().take(MAX_HTML).collect()
    } else {
        html
    };
    let mut headers_json = serde_json::Map::new();
    for (k, v) in &headers {
        headers_json.insert(k.clone(), json!(v));
    }
    json!({
        "ok": true,
        "status": status,
        "headers": headers_json,
        "htmlLength": html_kept.chars().count(),
        "markers": detect_markers(&html_kept, &headers),
        "truncated": truncated,
    })
}
