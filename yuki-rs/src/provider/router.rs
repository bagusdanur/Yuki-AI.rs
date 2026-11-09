//! Port `lib/provider-router.js`: failover maksimal 2 percobaan,
//! circuit breaker dengan cooldown, budget token/biaya per run, dan
//! statistik persisten ke file JSON (format sama seperti Node).
//!
//! Penyimpangan kecil: Node menulis statistik dengan throttle 5 detik;
//! Rust menulis segera setiap perubahan (flush juga tersedia). Isi file
//! dan bentuk snapshot operasi sama.

use super::settings::ProviderSlot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub id: String,
    pub name: String,
    pub model: String,
    pub state: String, // closed | open | half_open
    pub calls: u64,
    pub successes: u64,
    pub failures: u64,
    #[serde(rename = "consecutiveFailures")]
    pub consecutive_failures: u64,
    #[serde(rename = "latencyTotalMs")]
    pub latency_total_ms: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "estimatedCostUsd")]
    pub estimated_cost_usd: f64,
    #[serde(rename = "lastError", default)]
    pub last_error: String,
    #[serde(rename = "lastAttemptAt", default)]
    pub last_attempt_at: Option<String>,
    #[serde(rename = "openedAt", default)]
    pub opened_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    #[serde(rename = "type")]
    pub kind: String,
    pub provider: String,
    pub message: String,
    pub at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DayTotal {
    pub calls: u64,
    pub tokens: u64,
    pub cost: f64,
    pub failures: u64,
}

#[derive(Debug, Clone)]
pub struct RunBudget {
    pub max_tokens: u64,
    pub max_cost_usd: f64,
    pub used_tokens: u64,
    pub estimated_cost_usd: f64,
    pub exhausted: bool,
}

impl Default for RunBudget {
    fn default() -> Self {
        Self {
            max_tokens: std::env::var("YUKI_AGENT_TOKEN_BUDGET")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120_000),
            max_cost_usd: std::env::var("YUKI_AGENT_COST_BUDGET_USD")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25),
            used_tokens: 0,
            estimated_cost_usd: 0.0,
            exhausted: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}

impl Usage {
    pub fn from_json(usage: &serde_json::Value) -> Self {
        let pick = |keys: &[&str]| -> u64 {
            for key in keys {
                if let Some(v) = usage.get(key).and_then(|v| v.as_u64()) {
                    return v;
                }
            }
            0
        };
        Self {
            input: pick(&["prompt_tokens", "input_tokens"]),
            output: pick(&["completion_tokens", "output_tokens"]),
            total: pick(&["total_tokens"]),
        }
    }
}

pub fn cost_of(provider: &ProviderSlot, usage: &Usage) -> f64 {
    (usage.input as f64 * provider.input_cost_per_million
        + usage.output as f64 * provider.output_cost_per_million)
        / 1_000_000.0
}

struct Inner {
    // Vec agar urutan provider sama seperti Map di Node (urutan pertama terlihat).
    records: Vec<ProviderRecord>,
    alerts: Vec<Alert>,
    daily_totals: BTreeMap<String, DayTotal>,
    loaded_at: Option<String>,
}

pub struct ProviderRouter {
    inner: Mutex<Inner>,
    stats_file: PathBuf,
    pub failure_threshold: u64,
    pub cooldown_ms: i64,
    max_history_days: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn iso_of(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn today_key() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

impl ProviderRouter {
    pub fn new(stats_file: PathBuf) -> Self {
        let failure_threshold = std::env::var("YUKI_PROVIDER_CIRCUIT_FAILURES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3)
            .max(1);
        let cooldown_ms = std::env::var("YUKI_PROVIDER_COOLDOWN_MS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(60_000)
            .max(10_000);
        let max_history_days = std::env::var("YUKI_PROVIDER_STATS_HISTORY_DAYS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(90)
            .max(7);
        let router = Self {
            inner: Mutex::new(Inner {
                records: Vec::new(),
                alerts: Vec::new(),
                daily_totals: BTreeMap::new(),
                loaded_at: None,
            }),
            stats_file,
            failure_threshold,
            cooldown_ms,
            max_history_days,
        };
        router.load_stats();
        router
    }

    fn load_stats(&self) {
        let Ok(raw) = std::fs::read_to_string(&self.stats_file) else {
            return;
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return;
        };
        let mut inner = self.inner.lock().unwrap();
        if let Some(items) = parsed["providers"].as_array() {
            for item in items {
                if let Ok(record) = serde_json::from_value::<ProviderRecord>(item.clone()) {
                    if !inner.records.iter().any(|r| r.id == record.id) {
                        inner.records.push(record);
                    }
                }
            }
        }
        if let Some(items) = parsed["alerts"].as_array() {
            for item in items.iter().take(30) {
                if let Ok(alert) = serde_json::from_value::<Alert>(item.clone()) {
                    inner.alerts.push(alert);
                }
            }
        }
        if let Some(obj) = parsed["dailyTotals"].as_object() {
            for (key, value) in obj {
                if let Ok(day) = serde_json::from_value::<DayTotal>(value.clone()) {
                    inner.daily_totals.insert(key.clone(), day);
                }
            }
        }
        inner.loaded_at = parsed["savedAt"].as_str().map(|s| s.to_string());
        self.prune_daily(&mut inner);
    }

    fn prune_daily(&self, inner: &mut Inner) {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(self.max_history_days))
            .format("%Y-%m-%d")
            .to_string();
        inner.daily_totals.retain(|key, _| key.as_str() >= cutoff.as_str());
    }

    fn persist(&self, inner: &Inner) {
        if let Some(parent) = self.stats_file.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let payload = serde_json::json!({
            "savedAt": iso_of(now_ms()),
            "providers": inner.records.iter().collect::<Vec<_>>(),
            "alerts": inner.alerts.iter().take(30).collect::<Vec<_>>(),
            "dailyTotals": inner.daily_totals,
        });
        let tmp = PathBuf::from(format!("{}.tmp", self.stats_file.display()));
        if std::fs::write(&tmp, payload.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.stats_file);
        }
    }

    pub fn flush(&self) {
        let inner = self.inner.lock().unwrap();
        self.persist(&inner);
    }

    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.records.clear();
        inner.alerts.clear();
        inner.daily_totals.clear();
        inner.loaded_at = None;
        self.persist(&inner);
    }

    pub fn reset_for_tests(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.records.clear();
        inner.alerts.clear();
        inner.daily_totals.clear();
    }

    fn record_for<'a>(inner: &'a mut Inner, provider: &ProviderSlot) -> &'a mut ProviderRecord {
        if !inner.records.iter().any(|r| r.id == provider.id) {
            inner.records.push(ProviderRecord {
                id: provider.id.clone(),
                name: provider.name.clone(),
                model: provider.model.clone(),
                state: "closed".to_string(),
                calls: 0,
                successes: 0,
                failures: 0,
                consecutive_failures: 0,
                latency_total_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: 0.0,
                last_error: String::new(),
                last_attempt_at: None,
                opened_at: None,
            });
        }
        let record = inner
            .records
            .iter_mut()
            .find(|r| r.id == provider.id)
            .expect("record baru saja dipastikan ada");
        record.name = provider.name.clone();
        record.model = provider.model.clone();
        record
    }

    fn push_alert(inner: &mut Inner, kind: &str, provider: &ProviderSlot, message: &str) {
        inner.alerts.insert(
            0,
            Alert {
                kind: kind.to_string(),
                provider: provider.id.clone(),
                message: message.chars().take(180).collect(),
                at: iso_of(now_ms()),
            },
        );
        inner.alerts.truncate(30);
    }

    fn bump_daily(inner: &mut Inner, calls: u64, tokens: u64, cost: f64, failures: u64) {
        let key = today_key();
        let day = inner.daily_totals.entry(key).or_default();
        day.calls += calls;
        day.tokens += tokens;
        day.cost += cost;
        day.failures += failures;
    }

    /// Port `routeProviderRequest`. `invoke` dipanggil per provider yang
    /// dicoba dan mengembalikan (hasil, usage). Maksimal 2 percobaan.
    pub async fn route_request<T, F, Fut>(
        &self,
        providers: &[ProviderSlot],
        budget: &mut RunBudget,
        mut invoke: F,
    ) -> anyhow::Result<(T, ProviderSlot, u64, bool)>
    where
        F: FnMut(ProviderSlot) -> Fut,
        Fut: Future<Output = anyhow::Result<(T, Usage)>>,
    {
        if providers.is_empty() {
            anyhow::bail!("Tidak ada provider LLM yang dikonfigurasi.");
        }
        if budget.exhausted
            || budget.used_tokens >= budget.max_tokens
            || budget.estimated_cost_usd >= budget.max_cost_usd
        {
            budget.exhausted = true;
            anyhow::bail!("LLM_BUDGET_EXHAUSTED: batas token/biaya agent run sudah tercapai.");
        }
        let mut last_error: Option<anyhow::Error> = None;
        let mut attempts: u64 = 0;
        for provider in providers {
            if attempts >= 2 {
                break;
            }
            {
                let mut inner = self.inner.lock().unwrap();
                let record = Self::record_for(&mut inner, provider);
                let current = now_ms();
                if record.state == "open" {
                    let opened = record.opened_at.unwrap_or(0.0) as i64;
                    if current - opened < self.cooldown_ms {
                        continue;
                    }
                    record.state = "half_open".to_string();
                }
                attempts += 1;
                record.calls += 1;
                record.last_attempt_at = Some(iso_of(current));
            }
            let started = now_ms();
            match invoke(provider.clone()).await {
                Ok((result, usage)) => {
                    let latency = (now_ms() - started).max(0) as u64;
                    let cost = cost_of(provider, &usage);
                    {
                        let mut inner = self.inner.lock().unwrap();
                        let record = Self::record_for(&mut inner, provider);
                        record.successes += 1;
                        record.consecutive_failures = 0;
                        record.state = "closed".to_string();
                        record.last_error = String::new();
                        record.latency_total_ms += latency;
                        record.input_tokens += usage.input;
                        record.output_tokens += usage.output;
                        record.total_tokens += usage.total;
                        record.estimated_cost_usd += cost;
                        record.opened_at = None;
                        Self::bump_daily(&mut inner, 1, usage.total, cost, 0);
                        self.prune_daily(&mut inner);
                        self.persist(&inner);
                    }
                    budget.used_tokens += usage.total;
                    budget.estimated_cost_usd += cost;
                    if budget.used_tokens > budget.max_tokens
                        || budget.estimated_cost_usd > budget.max_cost_usd
                    {
                        budget.exhausted = true;
                    }
                    return Ok((result, provider.clone(), attempts, attempts > 1));
                }
                Err(err) => {
                    let latency = (now_ms() - started).max(0) as u64;
                    {
                        let mut inner = self.inner.lock().unwrap();
                        let record = Self::record_for(&mut inner, provider);
                        record.failures += 1;
                        record.consecutive_failures += 1;
                        record.last_error = err.to_string().chars().take(180).collect();
                        record.latency_total_ms += latency;
                        if record.consecutive_failures >= self.failure_threshold {
                            record.state = "open".to_string();
                            record.opened_at = Some(now_ms() as f64);
                            let msg = record.last_error.clone();
                            Self::push_alert(&mut inner, "circuit_open", provider, &msg);
                        }
                        Self::bump_daily(&mut inner, 1, 0, 0.0, 1);
                        self.persist(&inner);
                    }
                    last_error = Some(err);
                }
            }
        }
        {
            let mut inner = self.inner.lock().unwrap();
            let msg = last_error
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "Tidak ada provider sehat.".to_string());
            Self::push_alert(&mut inner, "all_providers_failed", &providers[0], &msg);
            self.persist(&inner);
        }
        let msg = last_error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "tidak ada provider sehat".to_string());
        anyhow::bail!("Semua provider LLM gagal atau circuit sedang terbuka: {msg}")
    }

    /// Port `getProviderOperations`.
    pub fn operations(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        let now = now_ms();
        let providers: Vec<serde_json::Value> = inner
            .records
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "name": r.name, "model": r.model, "state": r.state,
                    "calls": r.calls, "successes": r.successes, "failures": r.failures,
                    "consecutiveFailures": r.consecutive_failures,
                    "latencyTotalMs": r.latency_total_ms,
                    "inputTokens": r.input_tokens, "outputTokens": r.output_tokens,
                    "totalTokens": r.total_tokens,
                    "estimatedCostUsd": (r.estimated_cost_usd * 1_000_000.0).round() / 1_000_000.0,
                    "lastError": r.last_error, "lastAttemptAt": r.last_attempt_at,
                    "openedAt": r.opened_at,
                    "averageLatencyMs": if r.calls > 0 { r.latency_total_ms / r.calls } else { 0 },
                    "successRate": if r.calls > 0 { ((r.successes as f64 / r.calls as f64) * 1000.0).round() / 10.0 } else { 0.0 },
                    "averageTokensPerCall": if r.calls > 0 { r.total_tokens / r.calls } else { 0 },
                    "circuitRemainingMs": if r.state == "open" {
                        (self.cooldown_ms - (now - r.opened_at.unwrap_or(0.0) as i64)).max(0)
                    } else { 0 },
                })
            })
            .collect();
        let totals_calls: u64 = inner.records.iter().map(|r| r.calls).sum();
        let totals = serde_json::json!({
            "calls": totals_calls,
            "successes": inner.records.iter().map(|r| r.successes).sum::<u64>(),
            "failures": inner.records.iter().map(|r| r.failures).sum::<u64>(),
            "tokens": inner.records.iter().map(|r| r.total_tokens).sum::<u64>(),
            "inputTokens": inner.records.iter().map(|r| r.input_tokens).sum::<u64>(),
            "outputTokens": inner.records.iter().map(|r| r.output_tokens).sum::<u64>(),
            "estimatedCostUsd": (inner.records.iter().map(|r| r.estimated_cost_usd).sum::<f64>() * 1_000_000.0).round() / 1_000_000.0,
            "openCircuits": inner.records.iter().filter(|r| r.state == "open").count(),
            "averageLatencyMs": if providers.is_empty() { 0 } else {
                providers.iter().map(|p| p["averageLatencyMs"].as_u64().unwrap_or(0)).sum::<u64>() / providers.len() as u64
            },
            "successRate": if totals_calls > 0 {
                ((inner.records.iter().map(|r| r.successes).sum::<u64>() as f64 / totals_calls as f64) * 1000.0).round() / 10.0
            } else { 0.0 },
        });
        let daily: Vec<serde_json::Value> = inner
            .daily_totals
            .iter()
            .map(|(date, v)| {
                serde_json::json!({
                    "date": date, "calls": v.calls, "tokens": v.tokens,
                    "failures": v.failures, "cost": (v.cost * 1_000_000.0).round() / 1_000_000.0,
                })
            })
            .collect();
        serde_json::json!({
            "providers": providers,
            "alerts": inner.alerts.iter().take(10).map(|a| serde_json::json!({
                "type": a.kind, "provider": a.provider, "message": a.message, "at": a.at,
            })).collect::<Vec<_>>(),
            "totals": totals,
            "daily": daily,
            "persisted": {
                "file": self.stats_file.to_string_lossy(),
                "savedAt": inner.loaded_at,
                "historyDays": self.max_history_days,
            },
            "config": {
                "maxAttemptsPerRequest": 2,
                "failureThreshold": self.failure_threshold,
                "cooldownMs": self.cooldown_ms,
            },
        })
    }
}

pub fn stats_path(p: &Path) -> PathBuf {
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::settings::ProviderSlot;

    fn slot(id: &str) -> ProviderSlot {
        ProviderSlot {
            id: id.into(),
            name: id.into(),
            base_url: "https://example.com/v1".into(),
            model: "m".into(),
            api_key: "k".into(),
            input_cost_per_million: 1.0,
            output_cost_per_million: 2.0,
        }
    }

    fn router() -> ProviderRouter {
        ProviderRouter::new(PathBuf::from(format!(
            "/tmp/yuki-rs-router-test-{}/stats.json",
            uuid::Uuid::new_v4()
        )))
    }

    #[tokio::test]
    async fn failover_to_backup() {
        let router = router();
        let providers = vec![slot("primary"), slot("backup")];
        let mut budget = RunBudget::default();
        let (result, used, attempts, failover) = router
            .route_request(&providers, &mut budget, |p| async move {
                if p.id == "primary" {
                    anyhow::bail!("primary down")
                }
                Ok(("ok".to_string(), Usage { input: 10, output: 5, total: 15 }))
            })
            .await
            .unwrap();
        assert_eq!(result, "ok");
        assert_eq!(used.id, "backup");
        assert_eq!(attempts, 2);
        assert!(failover);
        assert_eq!(budget.used_tokens, 15);
    }

    #[tokio::test]
    async fn circuit_opens_after_threshold() {
        let router = router();
        let providers = vec![slot("primary")];
        for _ in 0..3 {
            let mut budget = RunBudget::default();
            let res = router
                .route_request::<String, _, _>(&providers, &mut budget, |_| async {
                    anyhow::bail!("down")
                })
                .await;
            assert!(res.is_err());
        }
        let ops = router.operations();
        assert_eq!(ops["providers"][0]["state"], "open");
        assert_eq!(ops["totals"]["failures"], 3);
    }

    #[tokio::test]
    async fn budget_exhausted_blocks() {
        let router = router();
        let providers = vec![slot("primary")];
        let mut budget = RunBudget { max_tokens: 0, ..Default::default() };
        let res = router
            .route_request::<String, _, _>(&providers, &mut budget, |_| async {
                Ok(("x".to_string(), Usage::default()))
            })
            .await;
        assert!(res.unwrap_err().to_string().contains("LLM_BUDGET_EXHAUSTED"));
    }
}
