//! Port protokol Edge TTS dari paket `msedge-tts` yang ter-vendor di
//! repo Node (Fase 6) — WebSocket ke speech.platform.bing.com dengan
//! token Sec-MS-GEC (SHA-256 atas ticks Windows yang dibulatkan 300
//! detik + token klien tepercaya). Tanpa sidecar Node.
//!
//! Hook uji: env YUKI_EDGE_WSS_URL mengganti basis URL wss (dipakai
//! harness paritas untuk menunjuk server mock lokal); default tetap
//! URL produksi persis seperti msedge-tts.

use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};

pub const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
pub const WSS_URL: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
pub const GEC_VERSION: &str = "1-143.0.3650.96";
pub const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";
const ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// generateSecMsGec (msedge-tts): ticks = floor(now/1000)+11644473600,
/// dibulatkan ke bawah per 300 detik, x10^7, SHA-256 hex huruf besar.
pub fn sec_ms_gec(now_secs: i64) -> String {
    let ticks = now_secs + 11_644_473_600;
    let rounded = ticks - (ticks % 300);
    let windows_ticks = rounded * 10_000_000;
    let mut hasher = Sha256::new();
    hasher.update(format!("{windows_ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn synth_url(now_secs: i64, connection_id: &str) -> String {
    let base = std::env::var("YUKI_EDGE_WSS_URL").unwrap_or_else(|_| WSS_URL.to_string());
    format!(
        "{base}?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version={GEC_VERSION}&ConnectionId={connection_id}",
        sec_ms_gec(now_secs)
    )
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn connection_uuid() -> String {
    // generateUUID msedge-tts: pola xxxxxxxx-xxxx-xxxx-yxxx-xxxxxxxxxxxx
    // (bukan v4 ketat: digit pertama segmen ke-4 bebas dari pola y).
    let raw = random_hex(16);
    let mut chars: Vec<char> = raw.chars().collect();
    // Susun ulang mengikuti pola: 8-4-4-4-12 dengan posisi 17 = varian.
    let get = |i: usize| chars.get(i).copied().unwrap_or('0');
    let variant = {
        let v = get(16).to_digit(16).unwrap_or(0);
        ((v & 0x3) | 0x8).to_string().chars().next().unwrap_or('8')
    };
    chars[16] = variant;
    let s: String = chars.into_iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

fn voice_locale(voice: &str) -> Option<String> {
    let re = regex::Regex::new(r"\w{2}-\w{2}").unwrap();
    re.find(voice).map(|m| m.as_str().to_string())
}

fn ssml_template(locale: &str, voice: &str, pitch: &str, rate: &str, volume: &str, input: &str) -> String {
    // Meniru _SSMLTemplate msedge-tts (termasuk trim di pemanggil).
    format!(
        "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xmlns:mstts=\"https://www.w3.org/2001/mstts\" xml:lang=\"{locale}\">\n                <voice name=\"{voice}\">\n                    <prosody pitch=\"{pitch}\" rate=\"{rate}\" volume=\"{volume}\">\n                        {input}\n                    </prosody> \n                </voice>\n            </speak>"
    )
}

fn speech_config_message() -> String {
    format!(
        "Content-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\n                    {{\n                        \"context\": {{\n                            \"synthesis\": {{\n                                \"audio\": {{\n                                    \"metadataoptions\": {{\n                                        \"sentenceBoundaryEnabled\": \"false\",\n                                        \"wordBoundaryEnabled\": \"false\"\n                                    }},\n                                    \"outputFormat\": \"{OUTPUT_FORMAT}\" \n                                }}\n                            }}\n                        }}\n                    }}\n                "
    )
}

fn request_id_of(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"X-RequestId:(.*?)\r\n").unwrap();
    re.captures(text).map(|c| c[1].to_string())
}

/// Sintesis satu ucapan lewat Edge TTS; mengembalikan byte audio MP3.
pub async fn synthesize_edge(input: &str, voice: &str, pitch: &str, rate: &str) -> Result<Vec<u8>, String> {
    let locale = voice_locale(voice)
        .ok_or_else(|| "Could not infer voiceLocale from voiceName, and no voiceLocale was specified!".to_string())?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let url = synth_url(now_secs, &connection_uuid());
    let ssml = ssml_template(&locale, voice, pitch, rate, "100", input);
    let request_id = random_hex(16);
    let ssml_message = format!(
        "X-RequestId:{request_id}\r\nContent-Type:application/ssml+xml\r\nPath:ssml\r\n\r\n{}",
        ssml.trim()
    );

    // _send msedge-tts mencoba init klien sampai 3x selama belum OPEN.
    let mut last_error = String::new();
    for _attempt in 1..=3 {
        match run_once(&url, &ssml_message, &request_id).await {
            Ok(audio) => return Ok(audio),
            Err(e) => last_error = e,
        }
    }
    Err(format!("Edge TTS WebSocket error: {last_error}"))
}

async fn run_once(url: &str, ssml_message: &str, request_id: &str) -> Result<Vec<u8>, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url
        .into_client_request()
        .map_err(|e| e.to_string())?;
    request.headers_mut().insert(
        "User-Agent",
        USER_AGENT.parse().map_err(|_| "header UA tidak valid".to_string())?,
    );
    request.headers_mut().insert(
        "Origin",
        ORIGIN.parse().map_err(|_| "header Origin tidak valid".to_string())?,
    );
    let (mut socket, _response) = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "timeout koneksi".to_string())?
    .map_err(|e| e.to_string())?;

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(speech_config_message().into()))
        .await
        .map_err(|e| e.to_string())?;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(ssml_message.to_string().into()))
        .await
        .map_err(|e| e.to_string())?;

    let mut audio: Vec<u8> = Vec::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let next = tokio::time::timeout_at(deadline, socket.next()).await;
        let Some(item) = (match next {
            Ok(v) => v,
            Err(_) => break, // batas waktu keseluruhan: kembalikan yang terkumpul
        }) else {
            break; // stream selesai / koneksi ditutup
        };
        let message = item.map_err(|e| e.to_string())?;
        match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                let rid = request_id_of(&text);
                if text.contains("Path:turn.end") && rid.as_deref() == Some(request_id) {
                    break;
                }
            }
            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                let text_form = String::from_utf8_lossy(&data).to_string();
                if !text_form.contains("Path:audio") {
                    continue;
                }
                let rid = request_id_of(&text_form);
                if rid.as_deref() != Some(request_id) {
                    continue;
                }
                const DELIM: &[u8] = b"Path:audio\r\n";
                if let Some(pos) = find_subslice(&data, DELIM) {
                    audio.extend_from_slice(&data[pos + DELIM.len()..]);
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    let _ = socket.close(None).await;
    Ok(audio)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gec_matches_reference_vector() {
        // Vektor referensi dihitung INDEPENDEN dengan Python hashlib
        // dari algoritma msedge-tts: now=1762387200 ->
        // ticks=13406860800 (kelipatan 300) -> windows=134068608000000000
        // -> SHA-256("{windows}6A5AA1D4...") huruf besar.
        assert_eq!(
            sec_ms_gec(1_762_387_200),
            "CB4577B87AF13283FB456C86BD47CCF0E616B5A2DAD9874885CFAD32F72537D5"
        );
        // now lain dalam jendela 300 detik yang sama -> token identik
        // (pembulatan ke bawah), membuktikan langkah rounding bekerja.
        assert_eq!(
            sec_ms_gec(1_762_387_355),
            "CB4577B87AF13283FB456C86BD47CCF0E616B5A2DAD9874885CFAD32F72537D5"
        );
    }

    #[test]
    fn ssml_shape() {
        let s = ssml_template("id-ID", "id-ID-GadisNeural", "+3Hz", "+0%", "100", "Halo");
        assert!(s.contains("xml:lang=\"id-ID\""));
        assert!(s.contains("<voice name=\"id-ID-GadisNeural\">"));
        assert!(s.contains("pitch=\"+3Hz\" rate=\"+0%\" volume=\"100\""));
    }
}
