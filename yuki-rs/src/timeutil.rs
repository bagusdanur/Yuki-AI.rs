//! Port `lib/time.js`: normalisasi zona waktu, bagian waktu zonasi, dan
//! konversi waktu lokal zonasi -> UTC. Node memakai Intl; Rust memakai
//! chrono-tz (database IANA yang sama).

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;

pub fn tz_of(name: &str) -> Option<Tz> {
    name.parse::<Tz>().ok()
}

pub fn normalize_timezone(value: &str) -> String {
    let upper = value.to_uppercase();
    let zone = match upper.as_str() {
        "WIB" => "Asia/Jakarta",
        "WITA" => "Asia/Makassar",
        "WIT" => "Asia/Jayapura",
        "UTC" => "UTC",
        _ => {
            if value.is_empty() {
                "Asia/Jakarta"
            } else {
                value
            }
        }
    };
    if tz_of(zone).is_some() {
        zone.to_string()
    } else {
        "Asia/Jakarta".to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZonedParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Minggu .. 6 = Sabtu (seperti pemakaian scheduler.js).
    pub weekday: u32,
}

pub fn zoned_parts(instant: DateTime<Utc>, timezone: &str) -> ZonedParts {
    let tz = tz_of(&normalize_timezone(timezone)).unwrap_or(chrono_tz::Asia::Jakarta);
    let local = instant.with_timezone(&tz);
    ZonedParts {
        year: local.year(),
        month: local.month(),
        day: local.day(),
        hour: local.hour(),
        minute: local.minute(),
        second: local.second(),
        weekday: local.weekday().num_days_from_sunday(),
    }
}

/// Sama seperti zonedDateTimeToUtc di time.js: tebak, render ulang di zona
/// tujuan, koreksi selisihnya (maks 3 putaran).
pub fn zoned_datetime_to_utc(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    timezone: &str,
) -> DateTime<Utc> {
    let tz = tz_of(&normalize_timezone(timezone)).unwrap_or(chrono_tz::Asia::Jakarta);
    let wanted = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, second))
        .unwrap_or_default();
    let mut guess = wanted.and_utc();
    for _ in 0..3 {
        let rendered_naive = guess.with_timezone(&tz).naive_local();
        let diff = wanted - rendered_naive;
        guess += diff;
    }
    let _ = tz;
    guess
}

pub fn ensure_utc_iso(value: &str) -> Option<String> {
    let text = value.trim().replace(' ', "T");
    if text.is_empty() {
        return None;
    }
    let with_zone = if text.ends_with('Z')
        || (text.len() >= 6
            && (text.as_bytes()[text.len() - 6] == b'+' || text.as_bytes()[text.len() - 6] == b'-')
            && text.as_bytes()[text.len() - 3] == b':')
    {
        text.clone()
    } else {
        format!("{text}Z")
    };
    let parsed = DateTime::parse_from_rfc3339(&with_zone).ok()?;
    Some(to_iso_z(&parsed.with_timezone(&Utc)))
}

/// Format ISO seperti Date.toISOString() di JS: milidetik 3 digit + Z.
pub fn to_iso_z(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

const MONTH_SHORT_ID: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "Mei", "Jun", "Jul", "Agu", "Sep", "Okt", "Nov", "Des",
];

/// Port formatInTimezone (label Intl id-ID dateStyle medium + timeStyle
/// long: "10 Nov 2026, 07.00.00 WIB"). Mengembalikan {isoUtc, timezone, label}.
pub fn format_in_timezone(value: &str, timezone: &str) -> Option<serde_json::Value> {
    let iso = ensure_utc_iso(value);
    let iso = iso?;
    let zone = normalize_timezone(timezone);
    let instant = parse_iso_loose(&iso)?;
    let parts = zoned_parts(instant, &zone);
    let tz = tz_of(&zone)?;
    let local = instant.with_timezone(&tz);
    let abbr = local.format("%Z").to_string();
    let label = format!(
        "{} {} {}, {:02}.{:02}.{:02} {}",
        parts.day,
        MONTH_SHORT_ID[(parts.month.max(1) - 1) as usize],
        parts.year,
        parts.hour,
        parts.minute,
        parts.second,
        abbr
    );
    Some(serde_json::json!({ "isoUtc": iso, "timezone": zone, "label": label }))
}

pub fn parse_iso_loose(value: &str) -> Option<DateTime<Utc>> {
    let iso = ensure_utc_iso(value)?;
    DateTime::parse_from_rfc3339(&iso)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_fallback() {
        assert_eq!(normalize_timezone("WIB"), "Asia/Jakarta");
        assert_eq!(normalize_timezone("WITA"), "Asia/Makassar");
        assert_eq!(normalize_timezone("zona-ngawur"), "Asia/Jakarta");
        assert_eq!(normalize_timezone("UTC"), "UTC");
    }

    #[test]
    fn zoned_parts_jakarta() {
        let dt = DateTime::parse_from_rfc3339("2026-11-09T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let p = zoned_parts(dt, "Asia/Jakarta");
        assert_eq!((p.hour, p.minute), (9, 0));
        assert_eq!(p.weekday, 1); // Senin
    }

    #[test]
    fn local_to_utc_roundtrip() {
        let utc = zoned_datetime_to_utc(2026, 11, 10, 7, 0, 0, "Asia/Jakarta");
        assert_eq!(to_iso_z(&utc), "2026-11-10T00:00:00.000Z");
    }
}
