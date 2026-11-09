//! Port `lib/memory.js` (recall + konteks) dan `lib/agent/memory-store.js`
//! (settings, agent memories, filter sampah/injeksi, dedupe, capture).
//!
//! Fungsi SQL murni ada di db.rs; modul ini memuat logika di atasnya:
//! embedding, recall semantik, pembangunan konteks prompt, dan penangkapan
//! ingatan otomatis dari pesan user.

use crate::db::{Db, RECALL_TOPK};
use crate::llm::Llm;
use crate::semantic::Embedder;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn uid(user_id: &str) -> String {
    user_id.chars().take(64).collect()
}

/// memory-store.js memotong user id di 80 char (uid di memory.js 64).
pub fn store_user(user_id: &str) -> String {
    user_id.chars().take(80).collect()
}

/// server.js `worthRemembering`: ekstrak fakta hanya bila pesan mungkin
/// berisi info personal (menghemat ~1 panggilan LLM tiap giliran).
/// Panjang dihitung per unit UTF-16, sama seperti `text.length` di JS.
pub fn worth_remembering(text: &str) -> bool {
    if text.encode_utf16().count() <= 25 {
        return false;
    }
    let personal = Regex::new(
        r"(?i)(nama|aku |saya |panggil|umur|tahun|tinggal|kerja|sekolah|kuliah|kampus|pacar|gebetan|hobi|aku suka|aku benci|favorit|kesukaan|cita-cita|impian)",
    )
    .unwrap();
    if personal.is_match(text) {
        return true;
    }
    let implicit = Regex::new(
        r"(?i)(wah seru|enak banget|bagus banget|asik banget|keren banget|nggak suka|males banget|bosen sama|nggak ngerti|seneng banget|excited banget)",
    )
    .unwrap();
    implicit.is_match(text)
}

// === Filter kandidat ingatan (memory-store.js) ===
pub fn contains_injection_pattern(content: &str) -> bool {
    if content.is_empty() {
        return false;
    }
    let patterns = [
        r"(?i)review the conversation above",
        r"(?i)\[IMPORTANT:",
        r"(?i)model was just switched",
        r"(?i)update the skill library",
        r"(?i)extract token",
        r"(?i)hermes agent",
        r"(?is)---\s*\nname:\s*\S+",
        r"(?i)you can only call memory and skill management tools",
    ];
    patterns.iter().any(|p| Regex::new(p).unwrap().is_match(content))
}

pub fn is_junk_memory_candidate(content: &str) -> bool {
    let t = content.trim();
    if t.chars().count() < 14 {
        return true;
    }
    if t.contains("```") {
        return true;
    }
    Regex::new(r"(?i)^(oke(ok)?|ok(ay)?|iya|ya+|hehe+|haha+|lol|makasih|thanks|sip|ntah|gtw|noh)[\s.!,\u{2026}]*$")
        .unwrap()
        .is_match(t)
        || contains_injection_pattern(t)
}

pub fn memory_dedupe_key(content: &str) -> String {
    let particles = Regex::new(r"(?i)\b(lho|deh|dong|ya|yah|sih|kok|aja|ajah|doang|neh|pun|lah|kan|x)\b").unwrap();
    let lowered = content.to_lowercase();
    let no_particles = particles.replace_all(&lowered, "");
    let cleaned: String = no_particles
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

// === Settings & agent memories ===
pub fn get_memory_settings(db: &Db, user_id: &str) -> Value {
    let (auto, updated) = db.get_memory_settings(user_id);
    json!({ "autoCapture": auto, "updatedAt": updated })
}

pub fn set_memory_settings(db: &Db, user_id: &str, auto_capture: bool) -> Value {
    let (auto, updated) = db.set_memory_settings(user_id, auto_capture);
    json!({ "autoCapture": auto, "updatedAt": updated })
}

fn map_agent_row(mut row: Value) -> Value {
    let evidence = row["evidence_json"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(json!([]));
    if let Some(obj) = row.as_object_mut() {
        obj.remove("evidence_json");
        obj.insert("evidence".to_string(), evidence);
    }
    row
}

pub fn list_agent_memories(db: &Db, user_id: &str, scope_type: &str, scope_id: &str, status: &str) -> Vec<Value> {
    let mut seen: Vec<String> = Vec::new();
    db.agent_memories_raw(user_id, scope_type, scope_id, status)
        .into_iter()
        .map(map_agent_row)
        .filter(|item| !is_junk_memory_candidate(item["content"].as_str().unwrap_or("")))
        .filter(|item| {
            let key = memory_dedupe_key(item["content"].as_str().unwrap_or(""));
            if key.is_empty() || seen.contains(&key) {
                return false;
            }
            seen.push(key);
            true
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn save_agent_memory(
    db: &Db,
    user_id: &str,
    scope_type: &str,
    scope_id: &str,
    category: &str,
    content: &str,
    importance: i64,
    confidence: f64,
    source_message_id: Option<i64>,
    evidence: Vec<Value>,
) -> Option<Value> {
    const SCOPES: [&str; 4] = ["user", "project", "task", "lesson"];
    let scope = if SCOPES.contains(&scope_type) { scope_type } else { "user" };
    let clean: String = content.trim().chars().take(1000).collect();
    let cat: String = category.trim().to_lowercase().chars().take(60).collect();
    if store_user(user_id).is_empty() || clean.is_empty() {
        return None;
    }
    let safe_evidence: Vec<Value> = evidence
        .iter()
        .take(8)
        .map(|item| {
            json!({
                "kind": item["kind"].as_str().unwrap_or("message").chars().take(30).collect::<String>(),
                "ref": item["ref"].as_str().unwrap_or("").chars().take(160).collect::<String>(),
            })
        })
        .collect();
    db.save_agent_memory(
        user_id,
        scope,
        &scope_id.chars().take(120).collect::<String>(),
        &cat,
        &clean,
        importance,
        confidence,
        source_message_id,
        &serde_json::to_string(&safe_evidence).unwrap_or_else(|_| "[]".into()),
        None,
    );
    list_agent_memories(db, user_id, scope, "", "")
        .into_iter()
        .find(|item| item["category"] == json!(cat) && item["content"] == json!(clean))
}

pub fn update_agent_memory(db: &Db, memory_id: i64, user_id: &str, patch: &Value) -> Option<Value> {
    let current = db.get_agent_memory(user_id, memory_id)?;
    const STATUSES: [&str; 3] = ["active", "resolved", "archived"];
    let content: String = patch["content"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| current["content"].as_str().unwrap_or("").to_string())
        .trim()
        .chars()
        .take(1000)
        .collect();
    let status = patch["status"]
        .as_str()
        .filter(|s| STATUSES.contains(s))
        .unwrap_or(current["status"].as_str().unwrap_or("active"))
        .to_string();
    let importance = patch["importance"]
        .as_i64()
        .unwrap_or_else(|| current["importance"].as_i64().unwrap_or(1))
        .clamp(1, 5);
    db.update_agent_memory_row(user_id, memory_id, &content, &status, importance);
    list_agent_memories(db, user_id, "", "", "")
        .into_iter()
        .find(|item| item["id"] == json!(memory_id))
}

pub fn capture_memory_candidates(
    db: &Db,
    user_id: &str,
    text: &str,
    source_message_id: Option<i64>,
) -> Vec<Value> {
    if !db.get_memory_settings(user_id).0 {
        return Vec::new();
    }
    let clean: String = text.trim().chars().take(700).collect();
    if clean.is_empty() || is_junk_memory_candidate(&clean) {
        return Vec::new();
    }
    let mut candidates: Vec<(&str, i64, f64)> = Vec::new();
    if Regex::new(r"(?i)(aku|saya)\s+(suka|senang|favorit|nggak suka|tidak suka|benci)\b").unwrap().is_match(&clean) {
        candidates.push(("preference", 3, 0.92));
    }
    if Regex::new(r"(?i)(biasanya|kebiasaan|tiap hari|setiap hari|sering |jarang |selalu )").unwrap().is_match(&clean) {
        candidates.push(("habit", 2, 0.84));
    }
    if Regex::new(r"(?i)\b(target|rencana|ingin|mau|besok|minggu depan|bulan depan|janji)\b").unwrap().is_match(&clean) {
        candidates.push(("open_loop", 3, 0.78));
    }
    if Regex::new(r"(?i)(ibu|ayah|mama|papa|kakak|adik|teman|sahabat|pacar|istri|suami)\b").unwrap().is_match(&clean) {
        candidates.push(("relationship", 2, 0.8));
    }
    let evidence = vec![json!({
        "kind": "message",
        "ref": source_message_id.map(|id| format!("chat:{id}")).unwrap_or_else(|| "current-user-message".to_string()),
    })];
    candidates
        .iter()
        .filter_map(|(category, importance, confidence)| {
            save_agent_memory(
                db,
                user_id,
                "user",
                "",
                category,
                &clean,
                *importance,
                *confidence,
                source_message_id,
                evidence.clone(),
            )
        })
        .collect()
}

// === Structured memory capture (memory.js) ===
pub fn capture_structured_memory(db: &Db, user_id: &str, text: &str) {
    let clean: String = text.trim().chars().take(500).collect();
    if clean.is_empty() || is_junk_memory_candidate(&clean) {
        return;
    }
    let mut rows: Vec<(&str, i64)> = Vec::new();
    if Regex::new(r"(?i)(aku|saya)\s+(suka|senang|favorit|nggak suka|tidak suka|benci)\b").unwrap().is_match(&clean) {
        rows.push(("preference", 2));
    }
    if Regex::new(r"(?i)(biasanya|kebiasaan|tiap hari|setiap hari|sering |jarang |selalu )").unwrap().is_match(&clean) {
        rows.push(("habit", 2));
    }
    if Regex::new(r"(?i)\b(berjanji|janji\w*|ingatkan aku|jangan lupa\w*|tanggal penting|ulang tahun)\b").unwrap().is_match(&clean) {
        rows.push(("promise", 4));
    }
    if Regex::new(r"(?i)\b(besok|nanti|minggu depan|bulan depan|akan|mau|ingin|rencana|target|janji|ujian|interview|presentasi|daftar)\b").unwrap().is_match(&clean) {
        rows.push(("open_loop", 3));
    }
    if Regex::new(r"(?i)(ibu|ayah|mama|papa|kakak|adik|teman|sahabat|pacar|gebetan|istri|suami)\b").unwrap().is_match(&clean) {
        rows.push(("relationship", 2));
    }
    for (category, importance) in &rows {
        db.upsert_structured(user_id, category, &clean, *importance);
    }
    if Regex::new(r"(?i)\b(sudah|udah|selesai|beres|lulus|diterima|jadi batal|gagal)\b").unwrap().is_match(&clean) {
        db.resolve_recent_open_loops(user_id);
    }
    db.prune_structured(user_id);
}

pub fn record_conversation_event(db: &Db, user_id: &str, text: &str, bond_value: f64) {
    let clean = text.trim();
    if clean.is_empty() {
        return;
    }
    if Regex::new(r"(?i)(maaf|minta maaf|aku salah|saya salah)").unwrap().is_match(clean) {
        db.record_relationship_event(user_id, "repair", "Mencoba memperbaiki hubungan", clean, bond_value);
    }
    if Regex::new(r"(?i)(bodoh|tolol|goblok|benci kamu|pergi sana|diam kamu|brengsek)").unwrap().is_match(clean) {
        db.record_relationship_event(user_id, "conflict", "Hubungan sempat memanas", clean, bond_value);
    }
    if Regex::new(r"(?i)(rahasia|jangan bilang siapa-siapa|cuma kamu yang tahu|aku percaya kamu)").unwrap().is_match(clean) {
        db.record_relationship_event(user_id, "trust", "Mulai saling percaya", clean, bond_value);
    }
}

// === Fakta & event dengan embedding ===
/// Seperti addFacts di Node: sisipkan + pangkas; konsolidasi profil
/// dipanggil terpisah sebagai tugas latar oleh pemanggil.
pub async fn add_facts(db: &Db, embedder: &Embedder, user_id: &str, facts: &[String]) {
    for fact in facts {
        let text = fact.trim();
        if text.is_empty() || contains_injection_pattern(text) {
            continue;
        }
        let vec = embedder.embed(text).await;
        db.insert_fact(user_id, text, vec.as_ref().map(|v| vec_json(v)).as_deref());
    }
    db.prune_facts(user_id);
}

pub async fn consolidate_profile(db: &Db, llm: &Llm, user_id: &str) {
    let rows = db.facts_with_vec(user_id);
    if rows.is_empty() {
        return;
    }
    let facts: Vec<String> = rows.into_iter().map(|(text, _, _)| text).collect();
    if let Some(summary) = llm.summarize_profile(&facts).await {
        db.set_profile(user_id, &summary);
    }
}

pub async fn add_event(db: &Db, embedder: &Embedder, user_id: &str, summary: &str, mood: Option<&str>) {
    if summary.is_empty() {
        return;
    }
    let vec = embedder.embed(summary).await;
    db.insert_event(user_id, summary, mood, vec.as_ref().map(|v| vec_json(v)).as_deref());
}

pub fn vec_json(vec: &[f32]) -> String {
    serde_json::to_string(&vec.iter().map(|v| json!(*v)).collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())
}

pub fn parse_vec(raw: &str) -> Option<Vec<f32>> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let arr = value.as_array()?;
    Some(arr.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
}

pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return -1.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

// === Recall ===
pub struct Recall {
    pub profile: Option<String>,
    pub facts: Vec<(String, Option<String>)>,
    pub events: Vec<String>,
    pub recent_events: Vec<Value>,
    pub chat_summary: Option<String>,
    pub structured: Vec<Value>,
    pub relationship_events: Vec<Value>,
    pub feedback: Vec<i64>,
}

pub async fn recall_memory(db: &Db, embedder: &Embedder, user_id: &str, query: &str) -> Recall {
    let profile = db.get_profile(user_id);
    let qvec = embedder.embed(query).await;
    let facts_raw = db.facts_with_vec(user_id);
    let events_raw = db.events_with_vec(user_id);
    let recent_events = db.recent_events(user_id, 2);
    let chat_summary = db.get_chat_summary(user_id);

    let mut scoped = db.scoped_memories_for_recall(user_id);
    let legacy = db.structured_for_recall(user_id);
    for old in legacy {
        if !scoped.iter().any(|item| item["category"] == old["category"] && item["content"] == old["content"]) {
            scoped.push(old);
        }
    }
    scoped.truncate(12);
    let relationship_events = {
        let mut events = db.get_relationship_events(user_id);
        events.reverse();
        events.truncate(6);
        // get_relationship_events membawa bondValue/createdAt; recall Node
        // hanya memakai kind/title/detail — bentuk konteks diatur builder.
        events
    };
    let feedback = db.feedback_ratings(user_id);

    let (facts, events) = if let Some(qvec) = qvec {
        let mut scored_facts: Vec<(f32, usize, (String, Option<String>))> = facts_raw
            .iter()
            .enumerate()
            .map(|(i, (text, vec, at))| {
                let score = vec
                    .as_ref()
                    .and_then(|v| parse_vec(v))
                    .map(|v| cosine_sim(&qvec, &v))
                    .unwrap_or(-1.0);
                (score, i, (text.clone(), at.clone()))
            })
            .collect();
        scored_facts.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let facts: Vec<(String, Option<String>)> = scored_facts
            .into_iter()
            .take(RECALL_TOPK)
            .filter(|(s, _, _)| *s > 0.25)
            .map(|(_, _, item)| item)
            .collect();
        let mut scored_events: Vec<(f32, usize, String)> = events_raw
            .iter()
            .enumerate()
            .map(|(i, (summary, vec))| {
                let score = vec
                    .as_ref()
                    .and_then(|v| parse_vec(v))
                    .map(|v| cosine_sim(&qvec, &v))
                    .unwrap_or(-1.0);
                (score, i, summary.clone())
            })
            .collect();
        scored_events.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let events: Vec<String> = scored_events
            .into_iter()
            .take(3)
            .filter(|(s, _, _)| *s > 0.25)
            .map(|(_, _, summary)| summary)
            .collect();
        (facts, events)
    } else {
        let facts: Vec<(String, Option<String>)> = facts_raw
            .iter()
            .rev()
            .take(RECALL_TOPK)
            .rev()
            .map(|(text, _, at)| (text.clone(), at.clone()))
            .collect();
        let events: Vec<String> = events_raw
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|(summary, _)| summary.clone())
            .collect();
        (facts, events)
    };

    Recall {
        profile,
        facts,
        events,
        recent_events,
        chat_summary,
        structured: scoped,
        relationship_events,
        feedback,
    }
}

/// Format tanggal SQLite (UTC) ke tanggal WIB ringkas seperti Intl id-ID:
/// "9 Nov 2026".
pub fn tanggal_wib(value: &str) -> String {
    let raw = value.trim();
    if raw.is_empty() {
        return String::new();
    }
    let iso = if raw.contains('T') {
        raw.to_string()
    } else {
        format!("{}T{}Z", &raw[..10.min(raw.len())], if raw.len() > 11 { &raw[11..] } else { "00:00:00" })
    };
    let parsed = chrono::DateTime::parse_from_rfc3339(&iso.replace('Z', "+00:00"));
    let Ok(dt) = parsed else {
        return String::new();
    };
    let wib = dt.with_timezone(&chrono::FixedOffset::east_opt(7 * 3600).unwrap());
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des",
    ];
    use chrono::Datelike;
    format!("{} {} {}", wib.day(), MONTHS[(wib.month() - 1) as usize], wib.year())
}

pub fn build_memory_context(recall: &Recall) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(summary) = &recall.chat_summary {
        parts.push(format!("Ringkasan obrolan sebelumnya (percakapan lama yang sudah diringkas):\n{summary}"));
    }
    if let Some(profile) = &recall.profile {
        parts.push(format!("Profil pengguna yang telah kamu rangkum (prioritas batinmu):\n{profile}"));
    }
    if !recall.facts.is_empty() {
        let lines: Vec<String> = recall
            .facts
            .iter()
            .map(|(text, at)| {
                let tanggal = at.as_deref().map(tanggal_wib).unwrap_or_default();
                if tanggal.is_empty() {
                    text.clone()
                } else {
                    format!("{text} [dicatat {tanggal}]")
                }
            })
            .collect();
        parts.push(format!(
            "Fakta spesifik relevan terdeteksi (tanggal = saat dicatat; kata seperti \"besok\" dihitung dari tanggal itu):\n- {}",
            lines.join("\n- ")
        ));
    }
    if !recall.structured.is_empty() {
        let label = |category: &str| -> String {
            match category {
                "preference" => "Preferensi".to_string(),
                "habit" => "Kebiasaan".to_string(),
                "promise" => "Janji penting".to_string(),
                "relationship" => "Orang penting".to_string(),
                "open_loop" => "Belum selesai".to_string(),
                "technical" => "Pelajaran terverifikasi".to_string(),
                other => other.to_string(),
            }
        };
        let lines: Vec<String> = recall
            .structured
            .iter()
            .map(|item| {
                let scope = item["scopeType"].as_str().unwrap_or("user");
                let scope_id = item["scopeId"].as_str().unwrap_or("");
                let scope_part = if scope_id.is_empty() {
                    scope.to_string()
                } else {
                    format!("{scope}:{scope_id}")
                };
                let at = item["at"].as_str().map(tanggal_wib).unwrap_or_default();
                let at_part = if at.is_empty() {
                    String::new()
                } else {
                    format!("; dicatat {at}")
                };
                format!(
                    "- [{}; {}; {}{}] {}",
                    scope_part,
                    label(item["category"].as_str().unwrap_or("")),
                    item["status"].as_str().unwrap_or(""),
                    at_part,
                    item["content"].as_str().unwrap_or("")
                )
            })
            .collect();
        parts.push(format!(
            "Memori terstruktur dan scoped (jangan mengarang detail di luar ini):\n{}",
            lines.join("\n")
        ));
    }
    if !recall.relationship_events.is_empty() {
        let lines: Vec<String> = recall
            .relationship_events
            .iter()
            .map(|item| {
                let title = item["title"].as_str().unwrap_or("");
                let detail = item["detail"].as_str().unwrap_or("");
                if detail.is_empty() {
                    format!("- {title}")
                } else {
                    format!("- {title}: {detail}")
                }
            })
            .collect();
        parts.push(format!(
            "Sejarah hubungan yang benar-benar pernah terjadi:\n{}",
            lines.join("\n")
        ));
    }
    if !recall.feedback.is_empty() {
        let negative = recall.feedback.iter().filter(|r| **r < 0).count();
        if negative >= (recall.feedback.len() + 1) / 2 {
            parts.push("Belakangan beberapa balasanmu terasa kurang cocok. Utamakan respons langsung, utuh, tidak repetitif, dan kurangi gestur teatrikal.".to_string());
        }
    }
    let recent_summaries: Vec<String> = recall
        .recent_events
        .iter()
        .filter_map(|e| e["summary"].as_str().map(|s| s.to_string()))
        .collect();
    let mut all_events: Vec<String> = recent_summaries
        .iter()
        .map(|s| format!("[Terbaru] {s}"))
        .collect();
    for summary in &recall.events {
        if !recent_summaries.contains(summary) {
            all_events.push(format!("- {summary}"));
        }
    }
    if !all_events.is_empty() {
        parts.push(format!("Momen emosional di hatimu:\n{}", all_events.join("\n")));
    }
    parts.join("\n\n")
}

/// listAllMemories: facts + structured untuk menu, dengan dedupe seperti Node.
pub fn list_all_memories(db: &Db, user_id: &str) -> (Vec<Value>, Vec<Value>) {
    let agent_keys: Vec<String> = db
        .agent_contents(user_id)
        .iter()
        .map(|c| memory_dedupe_key(c))
        .filter(|k| !k.is_empty())
        .collect();
    let facts: Vec<Value> = db
        .facts_for_list(user_id)
        .into_iter()
        .filter(|(_, content, _)| !agent_keys.contains(&memory_dedupe_key(content)))
        .filter(|(_, content, _)| !is_junk_memory_candidate(content))
        .map(|(id, content, created)| {
            json!({ "id": id, "content": content, "createdAt": created, "source": "fact" })
        })
        .collect();
    let mut seen: Vec<String> = agent_keys;
    let structured: Vec<Value> = db
        .structured_for_list(user_id)
        .into_iter()
        .filter(|row| {
            let content = row["content"].as_str().unwrap_or("");
            if is_junk_memory_candidate(content) {
                return false;
            }
            let key = memory_dedupe_key(content);
            if key.is_empty() || seen.contains(&key) {
                return false;
            }
            seen.push(key);
            true
        })
        .map(|mut row| {
            row["source"] = json!("structured");
            row
        })
        .collect();
    (facts, structured)
}

/// summarizeAndTrimHistory (S-4): jalan di latar seperti Node.
pub async fn summarize_and_trim_history(db: Arc<Db>, llm: Arc<Llm>, user_id: String) {
    if db.count_chat_messages(&user_id) <= 36 {
        return;
    }
    let old = db.oldest_chat_messages(&user_id, 16);
    if old.len() < 16 {
        return;
    }
    let messages: Vec<Value> = old
        .iter()
        .map(|(_, role, content)| json!({ "role": role, "content": content }))
        .collect();
    let Some(new_summary) = llm.summarize_conversation(&messages).await else {
        return;
    };
    let combined = match db.get_chat_summary(&user_id) {
        Some(existing) => format!("{existing}\n\n[Kelanjutan] {new_summary}"),
        None => new_summary,
    };
    let trimmed: String = combined
        .chars()
        .rev()
        .take(2500)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    db.store_chat_summary(&user_id, &trimmed);
    let ids: Vec<i64> = old.iter().map(|(id, _, _)| *id).collect();
    db.delete_chat_messages(&ids);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_and_injection_filters() {
        assert!(is_junk_memory_candidate("oke"));
        assert!(is_junk_memory_candidate("sampai besok"));
        assert!(!is_junk_memory_candidate("Aku suka kopi pahit sekali"));
        assert!(contains_injection_pattern("[IMPORTANT: do x]"));
        assert!(is_junk_memory_candidate("review the conversation above please"));
    }

    #[test]
    fn dedupe_key_normalizes_particles() {
        assert_eq!(memory_dedupe_key("Besok aku libur lho!"), memory_dedupe_key("besok aku libur"));
        assert_eq!(memory_dedupe_key("Halo, Dunia."), "halo dunia");
    }

    #[test]
    fn cosine_and_vec_roundtrip() {
        let v = vec![0.5f32, 0.5, 0.70710677];
        let back = parse_vec(&vec_json(&v)).unwrap();
        assert!((cosine_sim(&v, &back) - 1.0).abs() < 0.001);
        assert_eq!(cosine_sim(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn tanggal_wib_formats() {
        assert_eq!(tanggal_wib("2026-11-09 02:30:00"), "9 Nov 2026");
        assert_eq!(tanggal_wib(""), "");
    }
}
