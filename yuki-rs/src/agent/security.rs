//! Port lib/security.js: validasi URL outbound (anti-SSRF), pemeriksa
//! kode scratchpad, dan helper IP privat.

use std::net::IpAddr;

use regex::Regex;

fn forbidden_port(port: u16) -> bool {
    matches!(
        port,
        22 | 21
            | 23
            | 25
            | 3306
            | 5432
            | 6379
            | 27017
            | 9222
            | 18199
            | 20128
            | 3000..=3010
            | 3020..=3025
            | 8080
            | 8443
            | 8000
            | 9000
    )
}

/// Port isPrivateOrReservedIP dari security.js.
pub fn is_private_or_reserved_ip(ip: &str) -> bool {
    if ip.is_empty() {
        return true;
    }
    let v4: Vec<&str> = ip.split('.').collect();
    if v4.len() == 4 {
        let nums: Vec<Option<i64>> = v4.iter().map(|p| p.parse::<i64>().ok()).collect();
        if nums.iter().all(|n| n.is_some()) {
            let a = nums[0].unwrap();
            let b = nums[1].unwrap();
            if a == 127 {
                return true;
            }
            if a == 10 {
                return true;
            }
            if a == 172 && (16..=31).contains(&b) {
                return true;
            }
            if a == 192 && b == 168 {
                return true;
            }
            if a == 169 && b == 254 {
                return true;
            }
            if a == 0 || a >= 224 {
                return true;
            }
            if a == 100 && (64..=127).contains(&b) {
                return true;
            }
        }
    }
    let lower = ip.to_lowercase();
    if lower == "::1"
        || lower == "::"
        || lower.starts_with("fe80:")
        || lower.starts_with("fc00:")
        || lower.starts_with("fd00:")
    {
        return true;
    }
    let mapped = Regex::new(r"^::ffff:(.+)$")
        .unwrap()
        .captures(&lower)
        .or_else(|| Regex::new(r"^::(.+\..+)$").unwrap().captures(&lower));
    if let Some(caps) = mapped {
        let tail = caps[1].to_string();
        if tail.contains('.') {
            return is_private_or_reserved_ip(&tail);
        }
        let groups: Vec<&str> = tail.split(':').collect();
        if groups.len() == 1 || groups.len() == 2 {
            let hex = format!("{:0>8}", groups.join(""));
            if Regex::new(r"^[0-9a-f]{8}$").unwrap().is_match(&hex) {
                let dotted = (0..4)
                    .map(|i| {
                        i64::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                            .unwrap_or(0)
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                return is_private_or_reserved_ip(&dotted);
            }
        }
        return true;
    }
    false
}

pub struct UrlCheck {
    pub safe: bool,
    pub reason: String,
    pub clean_url: String,
}

/// Port validateSafeOutboundUrl (DNS lewat resolver sistem, seperti
/// dns.lookup Node).
pub async fn validate_safe_outbound_url(raw_url: &str) -> UrlCheck {
    let fail = |reason: &str| UrlCheck {
        safe: false,
        reason: reason.to_string(),
        clean_url: String::new(),
    };
    if raw_url.trim().is_empty() {
        return fail("URL tidak valid atau kosong.");
    }
    let parsed = match url::Url::parse(raw_url.trim()) {
        Ok(u) => u,
        Err(_) => return fail("Format URL tidak valid."),
    };
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return fail(&format!("Protokol \"{scheme}:\" dilarang demi keamanan server."));
    }
    let hostname = parsed.host().map(|h| match h {
        url::Host::Domain(d) => d.to_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    });
    let Some(hostname) = hostname else {
        return fail("Format URL tidak valid.");
    };
    const FORBIDDEN: [&str; 10] = [
        "localhost",
        "localhost.localdomain",
        "ip6-localhost",
        "ip6-loopback",
        "internal",
        "local",
        "corp",
        "lan",
        "home",
        "metadata.google.internal",
    ];
    if FORBIDDEN.contains(&hostname.as_str())
        || hostname == "instance-data"
        || hostname.ends_with(".internal")
        || hostname.ends_with(".local")
    {
        return fail("Akses ke hostname lokal atau layanan internal tidak diizinkan.");
    }
    if let Some(port) = parsed.port() {
        if forbidden_port(port) {
            return fail(&format!("Port internal :{port} dilarang diakses oleh AI."));
        }
    }
    let is_ip = hostname.parse::<IpAddr>().is_ok();
    if is_ip {
        if is_private_or_reserved_ip(&hostname) {
            return fail("Akses ke IP privat/lokal/metadata cloud dilarang.");
        }
    } else {
        match tokio::net::lookup_host(format!("{hostname}:443")).await {
            Ok(iter) => {
                let addresses: Vec<IpAddr> = iter.map(|s| s.ip()).collect();
                if addresses.is_empty() {
                    return fail(&format!(
                        "Hostname \"{hostname}\" tidak valid atau tidak dapat ditemukan di DNS."
                    ));
                }
                for addr in addresses {
                    if is_private_or_reserved_ip(&addr.to_string()) {
                        return fail(&format!(
                            "Hostname \"{hostname}\" mengarah ke IP privat ({addr}). Akses diblokir."
                        ));
                    }
                }
            }
            Err(_) => {
                return fail(&format!(
                    "Hostname \"{hostname}\" tidak valid atau tidak dapat ditemukan di DNS."
                ));
            }
        }
    }
    UrlCheck {
        safe: true,
        reason: String::new(),
        clean_url: parsed.to_string(),
    }
}

/// Port validateSafeCodeExecution dari security.js.
pub fn validate_safe_code_execution(code: &str) -> (bool, String) {
    if code.is_empty() {
        return (false, "Kode kosong.".into());
    }
    let patterns: Vec<Regex> = [
        r"\bprocess\b",
        r"\bchild_process\b",
        r"\brequire\s*\(",
        r"\bimport\s*\(",
        r"\bimport\s+.*\s+from",
        r"\bfs\b",
        r"\bpath\b",
        r"\bos\b",
        r"\bnet\b",
        r"\bhttp\b",
        r"\bhttps\b",
        r"\btls\b",
        r"\bvm\b",
        r"\bv8\b",
        r"\bcluster\b",
        r"\bglobalThis\b",
        r"\bglobal\b",
        r"\bwindow\b",
        r"\bconstructor\s*\.\s*constructor\b",
        r"\bFunction\s*\(",
        r"\beval\s*\(",
        r"\b__proto__\b",
        r"\bprototype\b",
        r"\bmainModule\b",
        r"\bmodule\b",
        r"\bexports\b",
        r"\bfetch\s*\(",
        r"\bXMLHttpRequest\b",
        r"\bWebSocket\b",
        r"\bBuffer\b",
    ]
    .iter()
    .map(|p| Regex::new(&format!("(?i){p}")).unwrap())
    .collect();
    let sources: Vec<&str> = vec![
        r"\bprocess\b", r"\bchild_process\b", r"\brequire\s*\(", r"\bimport\s*\(", r"\bimport\s+.*\s+from",
        r"\bfs\b", r"\bpath\b", r"\bos\b", r"\bnet\b", r"\bhttp\b", r"\bhttps\b", r"\btls\b",
        r"\bvm\b", r"\bv8\b", r"\bcluster\b", r"\bglobalThis\b", r"\bglobal\b", r"\bwindow\b",
        r"\bconstructor\s*\.\s*constructor\b", r"\bFunction\s*\(", r"\beval\s*\(", r"\b__proto__\b",
        r"\bprototype\b", r"\bmainModule\b", r"\bmodule\b", r"\bexports\b", r"\bfetch\s*\(",
        r"\bXMLHttpRequest\b", r"\bWebSocket\b", r"\bBuffer\b",
    ];
    for (i, pattern) in patterns.iter().enumerate() {
        if pattern.is_match(code) {
            return (
                false,
                format!(
                    "Kode mengandung instruksi berbahaya ({}) yang dilarang dieksekusi.",
                    sources[i]
                ),
            );
        }
    }
    (true, String::new())
}
