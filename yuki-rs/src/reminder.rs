//! Port `lib/companion-reminder.js`: deteksi permintaan pengingat di chat
//! companion, buat scheduled task sungguhan, dan hasilkan konteks jujur
//! untuk model (tidak pernah berjanji kosong).

use crate::db::Db;
use crate::scheduler::{create_scheduled_task, parse_to_cron_expr, CreateTask, ParsedSchedule};
use chrono::Utc;
use regex::Regex;
use sha2::{Digest, Sha256};

const TIMEZONE: &str = "Asia/Jakarta";

fn intent_re() -> Regex {
    Regex::new(r"(?i)\b(ingatkan|ingetin|ingat in|ingatkan aku|jangan lupa|tolong ingat|remind me|reminder|kasih tau nanti|ingetin nanti|alarm-in|bangunin)\b").unwrap()
}

fn schedule_hints() -> Vec<Regex> {
    let h = r"\d{1,2}(?:(?::|\.)\d{2})?";
    vec![
        Regex::new(&format!(r"(?i)\b(?:setiap|tiap)\s+(?:hari|senin|selasa|rabu|kamis|jumat|sabtu|minggu|weekend|hari kerja)(?:\s+jam\s+{h})?(?:\s*(?:pagi|siang|sore|malam))?")).unwrap(),
        Regex::new(&format!(r"(?i)\b(?:setiap|tiap)\s+\d+\s*(?:menit|jam|hari)(?:\s+(?:lagi|dari sekarang))?(?:\s+jam\s+{h}(?:\s*(?:pagi|siang|sore|malam))?)?")).unwrap(),
        Regex::new(&format!(r"(?i)\b(?:besok|lusa|hari ini|nanti)\s+(?:jam\s+)?{h}(?:\s*(?:pagi|siang|sore|malam))?")).unwrap(),
        Regex::new(r"(?i)\b\d+\s*(?:menit|jam)\s+(?:lagi|dari sekarang)").unwrap(),
        Regex::new(&format!(r"(?i)\bjam\s+{h}(?:\s*(?:pagi|siang|sore|malam))?\s*(?:besok|nanti)?")).unwrap(),
    ]
}

fn derive_title(raw: &str, schedule_text: &str) -> String {
    let mut t = raw.to_string();
    if !schedule_text.is_empty() {
        let escaped = regex::escape(schedule_text);
        if let Ok(re) = Regex::new(&format!("(?i){escaped}")) {
            t = re.replace(&t, " ").to_string();
        }
    }
    t = intent_re().replace(&t, " ").trim().to_string();
    let lead = Regex::new(r"(?i)^(?:yuki|he|hei|woi|tolong|please|mohon|aku|saya|ku|dong|ya|yak|yaa|deh|nanti|untuk|buat|soal|kalau|ingatkan|ingetin|ingat)\b[\s,]*").unwrap();
    loop {
        let next = lead.replace(&t, "").to_string();
        if next == t {
            break;
        }
        t = next;
    }
    t = Regex::new(r"(?i)\b(?:ya|yaa|yak|dong|deh|nih|sih|tapi|ya udah|oke|ok|yuk)\s*$")
        .unwrap()
        .replace(&t, "")
        .to_string();
    t = Regex::new(r"[?!.]+$").unwrap().replace(&t, "").to_string();
    t = Regex::new(r"\s{2,}").unwrap().replace_all(&t, " ").trim().to_string();
    if t.is_empty() {
        return "Pengingat dari Yuki".to_string();
    }
    t.chars().take(90).collect()
}

fn extract_schedule(text: &str) -> Option<(ParsedSchedule, String)> {
    let lower = text.to_lowercase();
    for re in schedule_hints() {
        if let Some(m) = re.find(&lower) {
            if let Some(parsed) = parse_to_cron_expr(m.as_str(), TIMEZONE, Utc::now()) {
                return Some((parsed, m.as_str().trim().to_string()));
            }
        }
    }
    let whole = parse_to_cron_expr(&lower, TIMEZONE, Utc::now())?;
    Some((whole, text.trim().to_string()))
}

pub struct ReminderOutcome {
    pub intent_detected: bool,
    pub context: String,
}

pub fn handle_companion_reminder(db: &Db, user_id: &str, user_text: &str) -> ReminderOutcome {
    let none = ReminderOutcome { intent_detected: false, context: String::new() };
    let text = user_text.trim();
    if text.is_empty() || text.chars().count() > 400 {
        return none;
    }
    if !intent_re().is_match(text) {
        return none;
    }
    let Some((parsed, matched)) = extract_schedule(text) else {
        return ReminderOutcome {
            intent_detected: true,
            context: "\n\n=== FAKTA PENGENAL (dari sistem, bukan keinginan user) ===\nUser baru saja meminta pengingat, tetapi WAKTUNYA BELUM JELAS sehingga pengingat BELUM DIBUAT sama sekali. Jangan pernah mengaku sudah menjadwalkan atau berkata \"nggak bakal lupa\". Tugasmu: tanyakan waktunya dengan singkat (misal \"jam berapa?\"). Jawab singkat, 1-2 kalimat.".to_string(),
        };
    };
    let title = derive_title(text, &matched);
    let mut hasher = Sha256::new();
    hasher.update(format!("{user_id}|{title}|{}|{}", parsed.cron, parsed.human).as_bytes());
    let idempotency_key: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .take(32)
        .collect();
    let task = create_scheduled_task(
        db,
        CreateTask {
            user_id: user_id.to_string(),
            title: title.clone(),
            description: format!("Pengingat: {title}"),
            cron_expr: parsed.cron.clone(),
            human_schedule: parsed.human.clone(),
            run_once: parsed.run_once,
            run_at: parsed.run_at.clone(),
            timezone: parsed.timezone.clone().unwrap_or_else(|| TIMEZONE.to_string()),
            idempotency_key,
        },
    );
    if task.is_none() {
        return ReminderOutcome {
            intent_detected: true,
            context: "\n\n=== FAKTA PENGENAL (dari sistem, bukan keinginan user) ===\nUser meminta pengingat, tapi sistem gagal membuatnya. JANGAN mengaku sudah menjadwalkan. Bilang saja dengan jujur (singkat) bahwa pengingatnya belum bisa dibuat.".to_string(),
        };
    }
    ReminderOutcome {
        intent_detected: true,
        context: format!(
            "\n\n=== FAKTA PENGENAL (dari sistem, bukan keinginan user) ===\nPengingat BERHASIL dibuat di sistem: \"{title}\" — jadwal: {}. Kamu BUKAN sedang berjanji kosong: jadwalnya sudah tersimpan dan akan benar-benar dikirimkan. Konfirmasikan ini dengan gayamu sendiri (singkat, tanpa menyebut istilah teknis seperti \"scheduled task\", \"sistem\", \"server\"). Jangan sebut jam jika waktunya sudah disebut user.",
            parsed.human
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_title_strips_filler() {
        assert_eq!(
            derive_title("Yuki tolong ingatkan aku minum vitamin besok jam 7", "besok jam 7"),
            "minum vitamin"
        );
        assert_eq!(derive_title("ingatkan aku", ""), "Pengingat dari Yuki");
    }
}
