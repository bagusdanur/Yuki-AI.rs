//! Port `lib/emotion.js` (Fase 5): mesin emosi Yuki — react, decay,
//! updateFromLLMMood, episode, label/feeling/directive, serialize.
//!
//! State disimpan sebagai JSON `serialize()` di tabel emotion, format
//! sama persis dengan Node sehingga barisnya bisa saling dibaca.
//! label/feeling/directive/allowed memakai helper llm.rs yang sudah
//! diporting di Fase 1 agar redaksinya tidak dobel.

use crate::llm::{bond_level_name, emotion_directive, emotion_label, feeling_for};
use crate::memory::contains_injection_pattern;
use rand::Rng;
use regex::Regex;
use serde_json::{json, Map, Value};

pub const MOOD_KEYS: [&str; 10] = [
    "joy", "sadness", "affection", "anger", "fear", "surprise", "fluster", "energy", "trust",
    "cemburu",
];

fn clamp(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Math.random()*2-1)*a di Node.
fn jitter(a: f64) -> f64 {
    rand::thread_rng().gen_range(-a..a)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

fn normalize_input(text: &str) -> String {
    let lower = text.to_lowercase();
    let spaced = re(r"[^a-z0-9\s]").replace_all(&lower, " ");
    re(r"\s+").replace_all(&spaced, " ").trim().to_string()
}

fn similarity(a: &str, b: &str) -> f64 {
    let na = normalize_input(a);
    let nb = normalize_input(b);
    let left: std::collections::HashSet<&str> =
        na.split(' ').filter(|w| !w.is_empty()).collect();
    let right: std::collections::HashSet<&str> =
        nb.split(' ').filter(|w| !w.is_empty()).collect();
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let overlap = left.iter().filter(|w| right.contains(**w)).count() as f64;
    let union = left.union(&right).count() as f64;
    overlap / union
}

/// K-1: keyword dihitung positif hanya bila tidak dinegasi 1-2 kata
/// sebelumnya.
fn has_keyword_positive(text: &str, pattern: &Regex) -> bool {
    if !pattern.is_match(text) {
        return false;
    }
    let neg = Regex::new(&format!(
        r"(?i)\b(nggak|tidak|bukan|gak|ga|tak|jangan|gak mau|nggak mau)(?:\s+\w+){{0,2}}\s+(?:{})",
        pattern.as_str()
    ))
    .unwrap();
    if !neg.is_match(text) {
        return true;
    }
    let cleaned = neg.replace_all(text, "__neg__");
    pattern.is_match(&cleaned)
}

pub struct Emotion {
    pub bond: f64,
    pub interactions: i64,
    pub recent_inputs: Vec<String>,
    pub relationship: (f64, f64, f64), // positive, conflict, repair
    pub episode: Option<Value>,
    pub mood: Map<String, Value>,
    pub last_update: i64,
}

impl Default for Emotion {
    fn default() -> Self {
        Self::from_state(None)
    }
}

impl Emotion {
    pub fn from_state(state: Option<Value>) -> Self {
        let state = state.unwrap_or(Value::Null);
        let bond = state.get("bond").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mut emo = Emotion {
            bond,
            interactions: state.get("interactions").and_then(|v| v.as_i64()).unwrap_or(0),
            recent_inputs: state
                .get("recentInputs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            relationship: (
                state.pointer("/relationship/positive").and_then(|v| v.as_f64()).unwrap_or(0.0),
                state.pointer("/relationship/conflict").and_then(|v| v.as_f64()).unwrap_or(0.0),
                state.pointer("/relationship/repair").and_then(|v| v.as_f64()).unwrap_or(0.0),
            ),
            episode: state.get("episode").filter(|v| v.is_object()).cloned(),
            mood: Map::new(),
            last_update: state.get("lastUpdate").and_then(|v| v.as_i64()).unwrap_or_else(now_ms),
        };
        if emo.recent_inputs.len() > 8 {
            emo.recent_inputs = emo.recent_inputs.split_off(emo.recent_inputs.len() - 8);
        }
        let mut mood = emo.baseline_mood();
        if let Some(stored) = state.get("mood").and_then(|v| v.as_object()) {
            for (k, v) in stored {
                mood.insert(k.clone(), v.clone());
            }
        }
        emo.mood = mood;
        emo
    }

    pub fn get(&self, key: &str) -> f64 {
        self.mood.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
    }

    fn set(&mut self, key: &str, value: f64) {
        self.mood.insert(key.to_string(), json!(value));
    }

    fn bump(&mut self, key: &str, delta: f64) {
        let current = self.mood.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
        self.set(key, clamp(current + delta));
    }

    pub fn closeness(&self) -> f64 {
        clamp(self.bond / 100.0)
    }

    /// Baseline persona dengan penyesuaian bertahap per kedekatan.
    pub fn baseline_mood(&self) -> Map<String, Value> {
        let mut base = Map::new();
        base.insert("joy".into(), json!(0.30));
        base.insert("sadness".into(), json!(0.12));
        base.insert("affection".into(), json!(0.08));
        base.insert("anger".into(), json!(0.08));
        base.insert("fear".into(), json!(0.10));
        base.insert("surprise".into(), json!(0.10));
        base.insert("fluster".into(), json!(0.0));
        base.insert("energy".into(), json!(0.50));
        base.insert("trust".into(), json!(0.30));
        base.insert("cemburu".into(), json!(0.0));
        let close = self.closeness();
        let (joy, affection, trust, anger) = if close >= 0.78 {
            (0.60, 0.65, 0.85, 0.02)
        } else if close >= 0.50 {
            (0.52, 0.45, 0.70, 0.04)
        } else if close >= 0.24 {
            (0.35, 0.30, 0.55, 0.06)
        } else if close >= 0.08 {
            (0.30, 0.15, 0.40, 0.08)
        } else {
            (0.30, 0.05, 0.30, 0.08)
        };
        base.insert("joy".into(), json!(joy));
        base.insert("affection".into(), json!(affection));
        base.insert("trust".into(), json!(trust));
        base.insert("anger".into(), json!(anger));
        base
    }

    pub fn update_from_llm_mood(&mut self, llm_mood: &str) {
        if llm_mood.is_empty() {
            return;
        }
        let base = self.baseline_mood();
        let base_get = |k: &str| base.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        match llm_mood {
            "kesal" => {
                let anger = self.get("anger").max(0.65);
                self.set("anger", anger);
                let joy = self.get("joy").min(0.15);
                self.set("joy", joy);
                let sadness = self.get("sadness").min(0.2);
                self.set("sadness", sadness);
            }
            "malu" => {
                let v = self.get("fluster").max(0.6);
                self.set("fluster", v);
            }
            "cemas" => {
                let v = self.get("fear").max(0.65);
                self.set("fear", v);
            }
            "sedih" => {
                let v = self.get("sadness").max(0.65);
                self.set("sadness", v);
                let joy = self.get("joy").min(0.15);
                self.set("joy", joy);
            }
            "kecewa" => {
                let trust = self
                    .mood
                    .get("trust")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0)
                    .min(0.08);
                self.set("trust", trust);
            }
            "cemburu" => {
                let v = self.get("cemburu").max(0.65);
                self.set("cemburu", v);
                let v = self.get("anger").max(0.30);
                self.set("anger", v);
                let v = self.get("fear").max(0.20);
                self.set("fear", v);
                let v = self.get("fluster").max(0.20);
                self.set("fluster", v);
            }
            "sayang/manja" => {
                let v = self.get("affection").max(0.75);
                self.set("affection", v);
                let v = self.get("joy").max(0.5);
                self.set("joy", v);
                let trust = base_get("trust").max(self.get("trust"));
                self.set("trust", trust);
            }
            "ceria" => {
                let v = self.get("joy").max(0.75);
                self.set("joy", v);
                let v = self.get("energy").max(0.7);
                self.set("energy", v);
                let v = self.get("anger").min(0.1);
                self.set("anger", v);
                let v = self.get("sadness").min(0.1);
                self.set("sadness", v);
            }
            "senang" => {
                let v = self.get("joy").max(0.55);
                self.set("joy", v);
                let v = self.get("anger").min(0.15);
                self.set("anger", v);
                let v = self.get("sadness").min(0.15);
                self.set("sadness", v);
            }
            "lesu" => {
                let energy = self
                    .mood
                    .get("energy")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0)
                    .min(0.2);
                self.set("energy", energy);
                let joy = self
                    .mood
                    .get("joy")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0)
                    .min(0.25);
                self.set("joy", joy);
            }
            "tenang" => {
                let episode_active = self
                    .episode
                    .as_ref()
                    .and_then(|e| e.get("turnsRemaining"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
                    > 0;
                if !episode_active {
                    for k in ["anger", "fluster", "fear", "sadness", "joy", "affection", "energy"] {
                        self.set(k, base_get(k));
                    }
                }
            }
            _ => {}
        }
        if llm_mood != "tenang" {
            let cause = self
                .episode
                .as_ref()
                .and_then(|e| e.get("cause"))
                .and_then(|v| v.as_str())
                .unwrap_or("dinamika percakapan")
                .to_string();
            self.remember_episode(llm_mood, &cause);
        }
    }

    pub fn remember_episode(&mut self, label: &str, cause: &str) {
        let duration: i64 = match label {
            "kesal" => 3,
            "kecewa" => 4,
            "sedih" => 3,
            "cemburu" => 2,
            "cemas" => 2,
            "malu" => 1,
            "sayang/manja" => 2,
            "senang" => 2,
            "ceria" => 2,
            _ => 1,
        };
        let intensity = ["anger", "sadness", "fear", "fluster", "joy"]
            .iter()
            .map(|k| self.get(k))
            .fold(0.0_f64, f64::max);
        self.episode = Some(json!({
            "label": label,
            "cause": cause.chars().take(160).collect::<String>(),
            "intensity": intensity,
            "turnsRemaining": duration,
            "startedAt": now_ms(),
        }));
    }

    pub fn decay(&mut self, per_turn: bool) {
        let minutes = (now_ms() - self.last_update) as f64 / 60_000.0;
        let time_rate = (minutes * 0.06).min(1.0);
        let active_episode = self
            .episode
            .as_ref()
            .and_then(|e| e.get("turnsRemaining"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            > 0;
        let turn_rate = if per_turn {
            if active_episode { 0.12 } else { 0.28 }
        } else {
            0.0
        };
        let rate = time_rate.max(turn_rate).min(1.0);
        let baseline = self.baseline_mood();
        for k in MOOD_KEYS {
            let base = baseline.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let current = self.get(k);
            self.set(k, clamp(current + (base - current) * rate + jitter(0.015)));
        }
        // Energi mengikuti jam lokal, lalu rebound halus ke baseline.
        let hour = chrono::Local::now().hour();
        if (1..5).contains(&hour) {
            let e = self.get("energy");
            self.set("energy", clamp(0.42_f64.max(e - 0.04)));
        } else if (5..11).contains(&hour) {
            let e = self.get("energy");
            self.set("energy", clamp(e + 0.05));
        }
        let base_energy = baseline.get("energy").and_then(|v| v.as_f64()).unwrap_or(0.5);
        let energy = self.get("energy");
        if energy < base_energy {
            self.set("energy", clamp(energy + ((base_energy - energy) * 0.35).min(0.08)));
        }
        self.last_update = now_ms();
        if per_turn && active_episode {
            if let Some(ep) = self.episode.as_mut() {
                let turns = ep.get("turnsRemaining").and_then(|v| v.as_i64()).unwrap_or(0) - 1;
                let intensity =
                    clamp(ep.get("intensity").and_then(|v| v.as_f64()).unwrap_or(0.0) * 0.82);
                ep["turnsRemaining"] = json!(turns);
                ep["intensity"] = json!(intensity);
                if turns <= 0 {
                    self.episode = None;
                }
            }
        }
    }

    /// Reaksi emosi terhadap pesan user + perubahan bond.
    /// Mengembalikan (label mood, nilai bond).
    pub fn react(&mut self, text: &str) -> (String, f64) {
        let was_angry = self.get("anger") > 0.45;
        let was_sad = self.get("sadness") > 0.45;
        self.decay(true);

        let t = text.to_lowercase();
        let close = self.closeness();
        let mut bond_delta: f64 = if text.trim().encode_utf16().count() >= 12 { 0.25 } else { 0.10 };

        let is_soothe = re(r"(maaf|maafin|maap|sorry|sowi|sengaja|ampun|jangan marah|sabar|jangan ngambek|tenang ya|sabar ya|baik banget|kamu baik|damai)").is_match(&t);
        if is_soothe {
            self.bump("anger", -0.4);
            self.bump("sadness", -0.2);
            self.bump("fluster", -0.15);
            self.bump("trust", 0.18);
            self.bump("affection", 0.12);
            bond_delta += if was_angry || was_sad { 0.30 } else { 0.13 };
            self.relationship.2 += if was_angry || was_sad { 1.0 } else { 0.25 };
        }
        if was_angry && !is_soothe {
            self.bump("sadness", 0.25);
            self.bump("trust", -0.12);
            self.bump("energy", -0.08);
        }
        if was_sad
            && !is_soothe
            && !re(r"(sayang|cinta|kangen|rindu|kgn|peluk|suka kamu|hebat|keren|pintar|makasih|terima kasih|haha|wkwk)").is_match(&t)
        {
            self.bump("trust", -0.15);
            self.bump("energy", -0.1);
        }

        if has_keyword_positive(
            &t,
            &re(r"(sayang|cinta|kangen|rindu|kgn|peluk|suka sama kamu|suka kamu|naksir|taksir|gombal|demen|love you|iloveyou|mwah|hug|sayangku|pacar|pacaran)"),
        ) {
            if close < 0.08 {
                self.bump("fluster", 0.55);
                self.bump("anger", 0.12);
                self.bump("surprise", 0.15);
                self.bump("affection", -0.1);
                bond_delta -= 0.80;
            } else if close < 0.35 {
                self.bump("fluster", 0.45);
                self.bump("affection", 0.12);
                self.bump("joy", 0.08);
                bond_delta += 0.16;
            } else {
                self.bump("affection", 0.28);
                self.bump("joy", 0.16);
                self.bump("fluster", 0.25);
                self.bump("trust", 0.06);
                bond_delta += 0.30;
            }
        }

        if re(r"(imut|cantik|manis|ganteng|cantik banget|imut banget|kiyowo|gemes|lucu banget|gemoy|cakep|manis banget)").is_match(&t) {
            self.bump("fluster", 0.4);
            self.bump("joy", 0.08);
            self.bump("anger", 0.05);
            bond_delta += 0.16;
        }
        if re(r"(hebat|keren|pintar|makasih|terima kasih|bangga)").is_match(&t) {
            self.bump("joy", 0.14);
            self.bump("energy", 0.06);
            self.bump("fluster", 0.1);
            bond_delta += 0.20;
        }
        if re(r"(haha|wkwk|lucu|ngakak|xixi|hehe)").is_match(&t) {
            self.bump("joy", 0.12);
            self.bump("energy", 0.08);
        }

        if has_keyword_positive(
            &t,
            &re(r"(benci|bodoh|bego|nyebelin|menyebalkan|jelek banget|diam kamu|pergi sana|berisik|jelek|goblok|tolol|annoying|bodo|bacot)"),
        ) {
            self.bump("anger", 0.4);
            self.bump("joy", -0.18);
            self.bump("affection", -0.1);
            self.bump("trust", -0.1);
            bond_delta -= 1.10;
            self.relationship.1 += 1.0;
        }
        if re(r"(bohong|boong|selingkuh|tinggalin|putus)").is_match(&t) {
            self.bump("sadness", 0.3);
            self.bump("trust", -0.25);
            self.bump("fear", 0.12);
            self.bump("affection", -0.1);
            bond_delta -= 1.50;
            self.relationship.1 += 1.5;
        }

        if re(r"(aku sedih|lagi sedih|kecewa|pengen nangis|lagi nangis|capek banget|lelah banget|aku sendiri|kesepian|lagi down|berat banget|gak ada yang peduli|butuh teman|kesepian banget|sedih banget|broken heart|patah hati|masalah keluarga|masalah pacar|masalah temen|masalah teman|berantem)").is_match(&t) {
            self.bump("sadness", 0.32);
            self.bump("affection", 0.1);
            self.bump("energy", -0.05);
            bond_delta += 0.28;
        }
        if re(r"(takut|cemas|khawatir|gugup|deg-degan|panik)").is_match(&t) {
            self.bump("fear", 0.3);
            self.bump("energy", -0.05);
            bond_delta += 0.18;
        }
        if re(r"(cewek lain|cowok lain|gebetan|mantan|pacar lain|dekat sama orang lain|deket sama)").is_match(&t) {
            self.bump("cemburu", 0.50);
            self.bump("anger", 0.12);
            self.bump("sadness", 0.1);
            self.bump("fluster", 0.15);
        }
        if re(r"[!?]{2,}").is_match(text) {
            self.bump("surprise", 0.12);
        }
        if re(r"(selamat pagi|pagi)").is_match(&t) {
            self.bump("energy", 0.1);
        }
        if re(r"(oyasumi|good night|tidur dulu|ngantuk|met bobo)").is_match(&t) {
            self.bump("energy", -0.12);
        }
        if re(r"[A-Z]{4,}").is_match(text) {
            self.bump("surprise", 0.12);
            self.bump("energy", 0.04);
        }
        if re(r"\.{3,}").is_match(text) {
            self.bump("sadness", 0.06);
            self.bump("affection", 0.04);
            bond_delta += 0.015;
        }
        if re(r"(?i)(wkwk|haha|xixi|hehe){2,}").is_match(text) {
            self.bump("joy", 0.08);
            self.bump("energy", 0.04);
        }
        if text.encode_utf16().count() > 50 {
            bond_delta += 0.15;
        }

        let normalized = normalize_input(text);
        let highest = self
            .recent_inputs
            .iter()
            .map(|prev| similarity(&normalized, prev))
            .fold(0.0_f64, f64::max);
        if bond_delta > 0.0 && highest >= 0.8 {
            bond_delta *= if highest >= 0.98 { 0.05 } else { 0.2 };
        }
        if bond_delta > 0.0 {
            bond_delta *= 1.0 - close * 0.22;
        }
        bond_delta = bond_delta.clamp(-1.0, 1.5);
        self.bond = (self.bond + bond_delta).clamp(0.0, 100.0);
        if bond_delta > 0.15 {
            self.relationship.0 += bond_delta.min(1.0);
        }
        self.interactions += 1;
        if !normalized.is_empty() && !contains_injection_pattern(text) {
            self.recent_inputs.push(normalized);
            if self.recent_inputs.len() > 8 {
                self.recent_inputs = self.recent_inputs.split_off(self.recent_inputs.len() - 8);
            }
        }
        let label = self.label().to_string();
        if ["kesal", "kecewa", "sedih", "cemburu", "cemas", "malu"].contains(&label.as_str()) {
            self.remember_episode(&label.clone(), text);
        }
        (label, self.bond)
    }

    pub fn label(&self) -> &'static str {
        emotion_label(Some(&self.serialize()), self.bond)
    }

    pub fn feeling(&self) -> &'static str {
        feeling_for(self.label())
    }

    pub fn directive(&self) -> String {
        emotion_directive(Some(&self.serialize()), self.bond)
    }

    pub fn allowed_emotions(&self) -> Vec<&'static str> {
        crate::llm::allowed_emotions(bond_level_name(self.bond))
    }

    pub fn serialize(&self) -> Value {
        json!({
            "mood": Value::Object(self.mood.clone()),
            "bond": self.bond,
            "interactions": self.interactions,
            "recentInputs": self.recent_inputs,
            "relationship": {
                "positive": self.relationship.0,
                "conflict": self.relationship.1,
                "repair": self.relationship.2,
            },
            "episode": self.episode.clone().unwrap_or(Value::Null),
            "lastUpdate": self.last_update,
        })
    }
}

use chrono::Timelike;
