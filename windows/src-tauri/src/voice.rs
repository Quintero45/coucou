// Voice: the Grok Bots speak. Piper (local, offline) by default with one voice
// per Bot; ElevenLabs or Azure when the owner stored a key for them.
//
// No native build deps: Piper is the prebuilt piper.exe, downloaded at first use
// (with progress on the `voice-engine` event) into %LOCALAPPDATA%\Coucou\voice\,
// voices from huggingface rhasspy/piper-voices. Cloud keys live in the
// Credential Manager (`tts-key:elevenlabs`, `tts-key:azure`) and are never logged
// nor handed to the front end — it may only ask whether they are set.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::{grokbot, log, platform, secrets, settings};

const PIPER_URL: &str = "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip";
const PIPER_ZIP: &str = "piper_windows_amd64.zip";
const PIPER_VOICES_BASE: &str = "https://huggingface.co/rhasspy/piper-voices/resolve/main";
/// Microsoft Edge neural voices (free "Read aloud" service) are the default;
/// Piper stays as the offline fallback.
pub const DEFAULT_VOICE: &str = "edge:es-CO-SalomeNeural";
/// Spoken when Edge cannot be reached (installed with the app's first voice).
const PIPER_FALLBACK_VOICE: &str = "piper:es_MX-ald-medium";
/// Pseudo-bot id for the Cursor agent: not a Grok Bot, but it has a voice too.
pub const CURSOR_ID: &str = "cursor";
/// Default voice per Bot *name* (case-insensitive) when settings.voices has no entry.
const NAMED_DEFAULTS: &[(&str, &str)] = &[
    ("aegon", "edge:es-CO-GonzaloNeural"),
    ("aerys", "edge:es-CO-SalomeNeural"),
    ("daemond", "edge:es-MX-JorgeNeural"),
];
const CURSOR_DEFAULT_VOICE: &str = "edge:es-MX-DaliaNeural";
const PREVIEW_TEXT: &str = "Hola, soy tu asistente. Así sueno cuando te leo las respuestas.";
/// Longest text read aloud in one go.
const MAX_SPEAK_CHARS: usize = 3000;

pub const ELEVENLABS_KEY: &str = "tts-key:elevenlabs";
pub const AZURE_KEY: &str = "tts-key:azure";

struct PiperVoice {
    key: &'static str,
    /// Folder under rhasspy/piper-voices.
    dir: &'static str,
    name: &'static str,
    lang: &'static str,
    size_mb: u32,
}

const PIPER_VOICES: &[PiperVoice] = &[
    PiperVoice { key: "es_MX-claude-high", dir: "es/es_MX/claude/high", name: "Claude (México)", lang: "es-MX", size_mb: 63 },
    PiperVoice { key: "es_MX-ald-medium", dir: "es/es_MX/ald/medium", name: "Ald (México)", lang: "es-MX", size_mb: 63 },
    PiperVoice { key: "es_ES-davefx-medium", dir: "es/es_ES/davefx/medium", name: "DaveFX (España)", lang: "es-ES", size_mb: 63 },
    PiperVoice { key: "es_ES-sharvard-medium", dir: "es/es_ES/sharvard/medium", name: "Sharvard (España)", lang: "es-ES", size_mb: 77 },
    PiperVoice { key: "es_AR-daniela-high", dir: "es/es_AR/daniela/high", name: "Daniela (Argentina)", lang: "es-AR", size_mb: 114 },
    PiperVoice { key: "es_MX-ald-x_low", dir: "es/es_MX/ald/x_low", name: "Ald ligera (México)", lang: "es-MX", size_mb: 21 },
];

/// Azure neural voices offered when an Azure key is set.
const AZURE_VOICES: &[(&str, &str, &str)] = &[
    ("es-MX-DaliaNeural", "Dalia (México)", "es-MX"),
    ("es-MX-JorgeNeural", "Jorge (México)", "es-MX"),
    ("es-CO-SalomeNeural", "Salomé (Colombia)", "es-CO"),
    ("es-CO-GonzaloNeural", "Gonzalo (Colombia)", "es-CO"),
    ("es-ES-ElviraNeural", "Elvira (España)", "es-ES"),
    ("es-ES-AlvaroNeural", "Álvaro (España)", "es-ES"),
    ("es-AR-ElenaNeural", "Elena (Argentina)", "es-AR"),
];

/// ElevenLabs premade voices, used when their voice list cannot be fetched.
const ELEVENLABS_FALLBACK: &[(&str, &str)] = &[
    ("21m00Tcm4TlvDq8ikWAM", "Rachel"),
    ("EXAVITQu4vr4xnSDxMaL", "Sarah"),
    ("pNInz6obpgDQGcFmaJgB", "Adam"),
    ("ErXwobaYiN019PkySvjV", "Antoni"),
];

// ── Microsoft Edge neural voices ──────────────────────────────────────────────

mod edge {
    //! The free Microsoft Edge "Read aloud" service, spoken the way the edge-tts
    //! project (rany2/edge-tts: constants.py, drm.py, communicate.py) does: a
    //! WebSocket to speech.platform.bing.com with the TrustedClientToken, the
    //! Sec-MS-GEC token (SHA-256 of the 5-minute-rounded Windows file time plus
    //! the token) and Sec-MS-GEC-Version, then `speech.config` + SSML, collecting
    //! the `Path:audio` binary frames until `turn.end`. Nothing here is secret.

    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use futures_util::{SinkExt, StreamExt};
    use sha2::{Digest, Sha256};
    use tokio::net::TcpStream;
    use tokio_rustls::rustls;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::HeaderValue;
    use tokio_tungstenite::tungstenite::{Error as WsError, Message};

    pub const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
    const HOST: &str = "speech.platform.bing.com";
    const BASE: &str = "speech.platform.bing.com/consumer/speech/synthesize/readaloud";
    /// edge-tts constants.py (CHROMIUM_FULL_VERSION), October 2026.
    const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";
    const ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";
    const WIN_EPOCH_S: u64 = 11_644_473_600;
    /// The service caps one SSML request; longer text goes in several turns.
    const MAX_CHUNK_BYTES: usize = 4096;
    /// The free endpoint refuses riff/raw PCM ("Unsupported Edge output format",
    /// checked October 2026); edge-tts' own MP3 format works.
    pub const OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

    /// Seconds to add to the PC clock (learnt from the server's Date on a 403).
    static CLOCK_SKEW_S: AtomicI64 = AtomicI64::new(0);

    fn chromium_major() -> &'static str {
        CHROMIUM_FULL_VERSION.split('.').next().unwrap_or("143")
    }

    pub fn sec_ms_gec_version() -> String {
        format!("1-{CHROMIUM_FULL_VERSION}")
    }

    fn user_agent() -> String {
        let m = chromium_major();
        format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{m}.0.0.0 Safari/537.36 Edg/{m}.0.0.0"
        )
    }

    fn unix_now() -> i64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        now + CLOCK_SKEW_S.load(Ordering::Relaxed)
    }

    /// drm.py generate_sec_ms_gec: Windows file time (100 ns ticks since 1601)
    /// rounded down to 5 minutes, then SHA-256(ticks + token), uppercase hex.
    pub fn sec_ms_gec_at(unix_s: i64) -> String {
        let mut secs = (unix_s.max(0) as u64) + WIN_EPOCH_S;
        secs -= secs % 300;
        let ticks = secs as u128 * 10_000_000;
        let digest = Sha256::digest(format!("{ticks}{TRUSTED_CLIENT_TOKEN}").as_bytes());
        digest.iter().map(|b| format!("{b:02X}")).collect()
    }

    pub fn sec_ms_gec() -> String {
        sec_ms_gec_at(unix_now())
    }

    /// 32 uppercase hex chars, fresh per connection (drm.py generate_muid).
    fn random_hex() -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        let seed = format!(
            "{:?}{}{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        Sha256::digest(seed.as_bytes())[..16].iter().map(|b| format!("{b:02X}")).collect()
    }

    fn connect_id() -> String {
        random_hex().to_lowercase()
    }

    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
    }

    fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y.rem_euclid(400);
        let m = m as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// communicate.py date_to_string: JavaScript-style, in UTC.
    pub fn date_string_at(unix_s: i64) -> String {
        let days = unix_s.div_euclid(86_400);
        let rem = unix_s.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{} {} {:02} {} {:02}:{:02}:{:02} GMT+0000 (Coordinated Universal Time)",
            DAYS[days.rem_euclid(7) as usize],
            MONTHS[(m - 1) as usize],
            d,
            y,
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    /// RFC 2616 date (`Wed, 07 Oct 2026 16:48:00 GMT`) to unix seconds.
    pub fn parse_http_date(s: &str) -> Option<i64> {
        let mut parts = s.split_whitespace().skip(1);
        let day: u32 = parts.next()?.parse().ok()?;
        let month = MONTHS.iter().position(|m| Some(*m) == parts.clone().next())? as u32 + 1;
        parts.next();
        let year: i64 = parts.next()?.parse().ok()?;
        let mut hms = parts.next()?.split(':').map(|x| x.parse::<i64>().ok());
        let (h, mi, se) = (hms.next()??, hms.next()??, hms.next()??);
        Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + se)
    }

    /// `es-CO-SalomeNeural` → `Microsoft Server Speech Text to Speech Voice (es-CO, SalomeNeural)`.
    pub fn full_voice_name(short: &str) -> String {
        let mut it = short.splitn(3, '-');
        let (lang, mut region, mut name) = match (it.next(), it.next(), it.next()) {
            (Some(l), Some(r), Some(n)) => (l.to_string(), r.to_string(), n.to_string()),
            _ => return short.to_string(),
        };
        if let Some(i) = name.find('-') {
            region = format!("{region}-{}", &name[..i]);
            name = name[i + 1..].to_string();
        }
        format!("Microsoft Server Speech Text to Speech Voice ({lang}-{region}, {name})")
    }

    pub fn valid_short_name(s: &str) -> bool {
        let mut it = s.splitn(3, '-');
        let (Some(l), Some(r), Some(n)) = (it.next(), it.next(), it.next()) else { return false };
        s.len() <= 80
            && l.len() >= 2
            && l.chars().all(|c| c.is_ascii_lowercase())
            && r.len() >= 2
            && r.chars().all(|c| c.is_ascii_uppercase())
            && n.ends_with("Neural")
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }

    /// remove_incompatible_characters + xml escape (&, <, >), as edge-tts does.
    fn escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                c if matches!(c as u32, 0..=8 | 11..=12 | 14..=31) => out.push(' '),
                c => out.push(c),
            }
        }
        out
    }

    /// Splits escaped text in pieces of at most MAX_CHUNK_BYTES, at a space when
    /// possible, never inside a character or an XML entity.
    fn split(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = text.trim();
        while rest.len() > MAX_CHUNK_BYTES {
            let mut cut = MAX_CHUNK_BYTES;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if let Some(space) = rest[..cut].rfind(['\n', ' ']) {
                if space > 0 {
                    cut = space;
                }
            }
            if let Some(amp) = rest[..cut].rfind('&') {
                if !rest[amp..cut].contains(';') {
                    cut = amp.max(1);
                }
            }
            let piece = rest[..cut].trim();
            if !piece.is_empty() {
                out.push(piece.to_string());
            }
            rest = rest[cut..].trim_start();
        }
        if !rest.is_empty() {
            out.push(rest.to_string());
        }
        out
    }

    fn tls_config() -> Arc<rustls::ClientConfig> {
        static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
        CONFIG
            .get_or_init(|| {
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                let config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .expect("rustls protocol versions")
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                Arc::new(config)
            })
            .clone()
    }

    type Socket = tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

    enum ConnectError {
        /// 403 with the server's clock: retry once with the skew corrected.
        Skew(i64),
        Other(String),
    }

    async fn connect_once() -> Result<Socket, ConnectError> {
        use ConnectError::Other;
        let url = format!(
            "wss://{BASE}/edge/v1?TrustedClientToken={TRUSTED_CLIENT_TOKEN}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version={}",
            connect_id(),
            sec_ms_gec(),
            sec_ms_gec_version()
        );
        let mut request = url.into_client_request().map_err(|e| Other(e.to_string()))?;
        let headers = request.headers_mut();
        let ua = user_agent();
        let muid = format!("muid={};", random_hex());
        for (k, v) in [
            ("Pragma", "no-cache"),
            ("Cache-Control", "no-cache"),
            ("Origin", ORIGIN),
            ("User-Agent", ua.as_str()),
            ("Accept-Encoding", "gzip, deflate, br, zstd"),
            ("Accept-Language", "en-US,en;q=0.9"),
            ("Cookie", muid.as_str()),
        ] {
            headers.insert(k, HeaderValue::from_str(v).map_err(|e| Other(e.to_string()))?);
        }
        let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((HOST, 443)))
            .await
            .map_err(|_| Other("tiempo de conexión agotado".into()))?
            .map_err(|e| Other(format!("sin conexión: {e}")))?;
        let _ = tcp.set_nodelay(true);
        let domain = rustls::pki_types::ServerName::try_from(HOST).map_err(|e| Other(e.to_string()))?.to_owned();
        let tls = tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(tls_config()).connect(domain, tcp))
            .await
            .map_err(|_| Other("TLS: tiempo agotado".into()))?
            .map_err(|e| Other(format!("TLS: {e}")))?;
        match tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::client_async(request, tls)).await {
            Err(_) => Err(Other("WebSocket: tiempo agotado".into())),
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(WsError::Http(response))) => {
                let status = response.status().as_u16();
                let server = response.headers().get("date").and_then(|d| d.to_str().ok()).and_then(parse_http_date);
                match (status, server) {
                    (403, Some(server)) => Err(ConnectError::Skew(server - unix_now())),
                    _ => Err(Other(format!("el servicio respondió HTTP {status}"))),
                }
            }
            Ok(Err(e)) => Err(Other(format!("WebSocket: {e}"))),
        }
    }

    async fn connect() -> Result<Socket, String> {
        match connect_once().await {
            Ok(s) => Ok(s),
            Err(ConnectError::Other(e)) => Err(e),
            Err(ConnectError::Skew(skew)) => {
                // drm.py handle_client_response_error: trust the server's clock.
                CLOCK_SKEW_S.fetch_add(skew, Ordering::Relaxed);
                connect_once().await.map_err(|e| match e {
                    ConnectError::Other(e) => e,
                    ConnectError::Skew(_) => "el servicio rechazó el token (HTTP 403)".into(),
                })
            }
        }
    }

    fn header_path(headers: &[u8]) -> Option<String> {
        String::from_utf8_lossy(headers)
            .split("\r\n")
            .find_map(|line| line.strip_prefix("Path:").map(|v| v.trim().to_string()))
    }

    /// One SSML turn: the audio bytes the service streams back.
    async fn turn(voice: &str, escaped: &str) -> Result<Vec<u8>, String> {
        let mut ws = connect().await?;
        let config = format!(
            "X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
             {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
             \"sentenceBoundaryEnabled\":\"true\",\"wordBoundaryEnabled\":\"false\"}},\
             \"outputFormat\":\"{OUTPUT_FORMAT}\"}}}}}}}}\r\n",
            date_string_at(unix_now())
        );
        let ssml = format!(
            "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
             <voice name='{}'><prosody pitch='+0Hz' rate='+0%' volume='+0%'>{escaped}</prosody></voice></speak>",
            full_voice_name(voice)
        );
        let request = format!(
            "X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}Z\r\nPath:ssml\r\n\r\n{ssml}",
            connect_id(),
            date_string_at(unix_now())
        );
        ws.send(Message::text(config)).await.map_err(|e| format!("envío: {e}"))?;
        ws.send(Message::text(request)).await.map_err(|e| format!("envío: {e}"))?;
        let mut audio = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(30), ws.next())
                .await
                .map_err(|_| "el servicio dejó de responder (30 s)".to_string())?;
            let msg = match next {
                None => return Err("conexión cerrada antes de turn.end".into()),
                Some(Err(e)) => return Err(format!("WebSocket: {e}")),
                Some(Ok(m)) => m,
            };
            match msg {
                Message::Text(t) => {
                    let t: &str = t.as_ref();
                    let head = t.split("\r\n\r\n").next().unwrap_or("");
                    if header_path(head.as_bytes()).as_deref() == Some("turn.end") {
                        break;
                    }
                }
                Message::Binary(b) => {
                    let b: &[u8] = b.as_ref();
                    if b.len() < 2 {
                        return Err("trama binaria sin cabecera".into());
                    }
                    let hl = u16::from_be_bytes([b[0], b[1]]) as usize;
                    if 2 + hl > b.len() {
                        return Err("cabecera binaria más larga que la trama".into());
                    }
                    if header_path(&b[2..2 + hl]).as_deref() != Some("audio") {
                        return Err("trama binaria que no es audio".into());
                    }
                    audio.extend_from_slice(&b[2 + hl..]);
                }
                Message::Close(frame) => {
                    return Err(format!("el servicio cerró: {}", frame.map(|f| f.reason.to_string()).unwrap_or_default()))
                }
                _ => {}
            }
        }
        let _ = ws.close(None).await;
        if audio.is_empty() {
            return Err("no llegó audio".into());
        }
        Ok(audio)
    }

    /// MP3 (24 kHz mono, 48 kbit/s) for `text` in `voice` (`es-CO-SalomeNeural`).
    /// Text over 4096 bytes takes several turns; MP3 frames simply concatenate.
    pub async fn synth_mp3(voice: &str, text: &str) -> Result<Vec<u8>, String> {
        if !valid_short_name(voice) {
            return Err(format!("voz Edge inválida: {voice}"));
        }
        let mut mp3 = Vec::new();
        for piece in split(&escape(text)) {
            mp3.extend_from_slice(&turn(voice, &piece).await?);
        }
        if mp3.len() < 600 {
            return Err("audio vacío".into());
        }
        Ok(mp3)
    }

    #[derive(Clone, Debug)]
    pub struct EdgeVoice {
        pub short_name: String,
        pub locale: String,
        pub display: String,
    }

    /// The service's voice list, Spanish only.
    pub async fn spanish_voices() -> Result<Vec<EdgeVoice>, String> {
        let url = format!(
            "https://{BASE}/voices/list?trustedclienttoken={TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version={}",
            sec_ms_gec(),
            sec_ms_gec_version()
        );
        let m = chromium_major();
        let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
        let resp = client
            .get(url)
            .header("Authority", HOST)
            .header("Sec-CH-UA", format!("\" Not;A Brand\";v=\"99\", \"Microsoft Edge\";v=\"{m}\", \"Chromium\";v=\"{m}\""))
            .header("Sec-CH-UA-Mobile", "?0")
            .header("Accept", "*/*")
            .header("Sec-Fetch-Site", "none")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Dest", "empty")
            .header("User-Agent", user_agent())
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cookie", format!("muid={};", random_hex()))
            .send()
            .await
            .map_err(|e| format!("lista de voces: {}", e.without_url()))?;
        if !resp.status().is_success() {
            return Err(format!("lista de voces: HTTP {}", resp.status()));
        }
        let list: serde_json::Value = resp.json().await.map_err(|e| format!("lista de voces: {e}"))?;
        let mut out: Vec<EdgeVoice> = list
            .as_array()
            .ok_or("lista de voces inesperada")?
            .iter()
            .filter_map(|v| {
                let short = v["ShortName"].as_str()?;
                let locale = v["Locale"].as_str()?;
                if !locale.starts_with("es-") || !valid_short_name(short) {
                    return None;
                }
                let friendly = v["FriendlyName"].as_str().unwrap_or("");
                Some(EdgeVoice { short_name: short.into(), locale: locale.into(), display: display_name(short, locale, friendly) })
            })
            .collect();
        out.sort_by(|a, b| a.locale.cmp(&b.locale).then(a.short_name.cmp(&b.short_name)));
        Ok(out)
    }

    /// `SalomeNeural` + `… - Spanish (Colombia)` → `Salome (Colombia)`.
    pub fn display_name(short: &str, locale: &str, friendly: &str) -> String {
        let person = short.rsplit('-').next().unwrap_or(short).trim_end_matches("Neural").to_string();
        let region = friendly
            .rsplit_once('(')
            .map(|(_, r)| r.trim_end_matches(')').trim().to_string())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| locale.to_string());
        format!("{person} ({region})")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn token_matches_edge_tts() {
            // 2024-11-08 12:00:00 UTC; same token for the whole 5-minute window.
            let a = sec_ms_gec_at(1_731_067_200);
            assert_eq!(a, sec_ms_gec_at(1_731_067_200 + 299));
            assert_ne!(a, sec_ms_gec_at(1_731_067_200 + 300));
            // Same input through edge-tts drm.py's float arithmetic.
            assert_eq!(a, "EA09A9EEE65ADF1C44241564DD783FB0496E6FC56A5EA9DA4077423D5E6281E2");
            assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        }

        #[test]
        fn names_and_dates() {
            assert_eq!(full_voice_name("es-CO-SalomeNeural"), "Microsoft Server Speech Text to Speech Voice (es-CO, SalomeNeural)");
            assert!(valid_short_name("es-MX-JorgeNeural"));
            assert!(!valid_short_name("es-MX-Jorge"));
            assert_eq!(date_string_at(0), "Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)");
            assert_eq!(parse_http_date("Wed, 07 Oct 2026 16:48:00 GMT"), Some(1_791_391_680));
            assert_eq!(date_string_at(1_791_391_680), "Wed Oct 07 2026 16:48:00 GMT+0000 (Coordinated Universal Time)");
        }

        #[test]
        fn text_is_escaped_and_split() {
            assert_eq!(escape("a & b <c>\u{b}"), "a &amp; b &lt;c&gt; ");
            let long = "palabra ".repeat(1200);
            let parts = split(&long);
            assert!(parts.len() >= 2 && parts.iter().all(|p| p.len() <= MAX_CHUNK_BYTES));
        }
    }
}

/// Edge voices offered when the service's list cannot be fetched.
const EDGE_FALLBACK: &[(&str, &str, &str)] = &[
    ("es-CO-SalomeNeural", "es-CO", "Salome (Colombia)"),
    ("es-CO-GonzaloNeural", "es-CO", "Gonzalo (Colombia)"),
    ("es-MX-DaliaNeural", "es-MX", "Dalia (Mexico)"),
    ("es-MX-JorgeNeural", "es-MX", "Jorge (Mexico)"),
    ("es-ES-ElviraNeural", "es-ES", "Elvira (Spain)"),
    ("es-ES-AlvaroNeural", "es-ES", "Alvaro (Spain)"),
    ("es-AR-ElenaNeural", "es-AR", "Elena (Argentina)"),
    ("es-AR-TomasNeural", "es-AR", "Tomas (Argentina)"),
];
/// The service's list is fetched at most this often.
const EDGE_LIST_TTL: Duration = Duration::from_secs(12 * 3600);

fn edge_list_cache() -> &'static Mutex<Option<(Instant, Vec<edge::EdgeVoice>)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<edge::EdgeVoice>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Spanish Edge voices: cached service list, else the built-in few. The
/// fallback voices (the Bots' defaults among them) are always included.
async fn edge_voices() -> Vec<edge::EdgeVoice> {
    let cached = edge_list_cache().lock().unwrap().clone();
    let mut list = match cached {
        Some((at, list)) if at.elapsed() < EDGE_LIST_TTL => list,
        stale => match edge::spanish_voices().await {
            Ok(list) if !list.is_empty() => {
                log::line(format!("voice: Edge voice list fetched ({} Spanish voices)", list.len()));
                *edge_list_cache().lock().unwrap() = Some((Instant::now(), list.clone()));
                list
            }
            other => {
                if let Err(e) = other {
                    log::line(format!("voice: Edge voice list unavailable: {e}"));
                }
                stale.map(|(_, l)| l).unwrap_or_default()
            }
        },
    };
    for (short, locale, display) in EDGE_FALLBACK {
        if !list.iter().any(|v| v.short_name == *short) {
            list.push(edge::EdgeVoice { short_name: (*short).into(), locale: (*locale).into(), display: (*display).into() });
        }
    }
    list
}

/// Last Edge outcome reported on `voice-engine` (0 none, 1 ready, 2 failing):
/// the event goes out on a change, so the settings page isn't reloaded per sentence.
static EDGE_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn note_edge(app: &AppHandle, error: Option<String>) {
    let state = if error.is_some() { 2 } else { 1 };
    if EDGE_STATE.swap(state, Ordering::SeqCst) != state || error.is_some() {
        emit_engine(app, EngineEvent { ready: error.is_none(), engine: "edge".into(), error, ..Default::default() });
    }
}

/// WAV for `text` in an Edge voice; on any network/protocol failure the
/// fallback Piper voice speaks instead, so the Bot is still heard.
async fn synth_edge(app: &AppHandle, short: &str, text: &str) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    match edge::synth_mp3(short, text).await.and_then(|mp3| {
        let kb = mp3.len() / 1024;
        mp3_to_wav(mp3).map(|wav| (kb, wav))
    }) {
        Ok((kb, wav)) => {
            log::line(format!("voice: Edge {short} synthesised {kb} KB MP3 in {} ms", started.elapsed().as_millis()));
            note_edge(app, None);
            Ok(wav)
        }
        Err(e) => {
            log::line(format!("voice: Edge {short} failed after {} ms ({e}); falling back to Piper", started.elapsed().as_millis()));
            note_edge(app, Some(e));
            let Voice::Piper(key) = parse_voice(PIPER_FALLBACK_VOICE)? else { unreachable!() };
            synth_piper(app, &key, text, true).await
        }
    }
}

/// Decodes Edge's MP3 into the 16-bit mono WAV every other path (playback,
/// call.rs) expects.
#[cfg(windows)]
fn mp3_to_wav(mp3: Vec<u8>) -> Result<Vec<u8>, String> {
    use rodio::Source;
    let decoder = rodio::Decoder::new_mp3(std::io::Cursor::new(mp3)).map_err(|e| format!("MP3 ilegible: {e}"))?;
    let channels = decoder.channels().max(1) as usize;
    let rate = decoder.sample_rate();
    let samples: Vec<i16> = decoder.collect();
    let mut pcm = Vec::with_capacity(samples.len() / channels * 2);
    for frame in samples.chunks(channels) {
        let mixed = frame.iter().map(|&s| s as i32).sum::<i32>() / frame.len() as i32;
        pcm.extend_from_slice(&(mixed as i16).to_le_bytes());
    }
    if pcm.len() < 480 {
        return Err("MP3 sin audio".into());
    }
    Ok(wav_from_pcm16(&pcm, rate))
}

#[cfg(not(windows))]
fn mp3_to_wav(_mp3: Vec<u8>) -> Result<Vec<u8>, String> {
    Err("La voz solo está disponible en Windows por ahora.".into())
}

// ── Paths and small helpers (shared with meeting.rs) ──────────────────────────

/// %LOCALAPPDATA%\Coucou\voice
pub(crate) fn voice_dir() -> PathBuf {
    settings::local_dir().join("voice")
}

fn piper_dir() -> PathBuf {
    voice_dir().join("piper")
}

fn piper_voices_dir() -> PathBuf {
    voice_dir().join("piper-voices")
}

pub(crate) fn tmp_dir() -> PathBuf {
    voice_dir().join("tmp")
}

/// A fresh file name under tmp\ for this process.
pub(crate) fn tmp_file(prefix: &str, ext: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    tmp_dir().join(format!("{prefix}-{}-{n}.{ext}", std::process::id()))
}

pub(crate) fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    if depth == 0 {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn piper_exe() -> Option<PathBuf> {
    find_file(&piper_dir(), if cfg!(windows) { "piper.exe" } else { "piper" }, 2)
}

fn piper_model(key: &str) -> PathBuf {
    piper_voices_dir().join(format!("{key}.onnx"))
}

fn piper_voice_installed(key: &str) -> bool {
    let model = piper_model(key);
    model.is_file() && PathBuf::from(format!("{}.json", model.display())).is_file()
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 16-bit PCM mono WAV around raw little-endian samples.
pub(crate) fn wav_from_pcm16(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

// ── The `voice-engine` event and downloads ────────────────────────────────────

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct EngineEvent {
    pub ready: bool,
    pub engine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloading: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<f64>,
    /// What is being downloaded (`piper`, `es_MX-claude-high`, `whisper`, `ggml-small.bin`…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub(crate) fn emit_engine(app: &AppHandle, event: EngineEvent) {
    let _ = app.emit("voice-engine", event);
}

pub(crate) fn emit_ready(app: &AppHandle, engine: &str, ready: bool) {
    emit_engine(app, EngineEvent { ready, engine: engine.into(), ..Default::default() });
}

/// One download or install at a time: two clicks never fetch the same 500 MB twice.
pub(crate) fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Waits between download attempts: about 7 minutes in all, enough to ride out
/// a dropped connection. Whatever arrived stays in the .part file.
const RETRY_WAITS_S: &[u64] = &[5, 10, 20, 40, 60, 60, 60, 60, 60, 60];

enum DownloadError {
    /// Network trouble: try again from where the .part file ends.
    Retry(String),
    /// Disk errors, 404…: trying again won't help.
    Fatal(String),
}

pub(crate) fn part_path(dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", dest.display()))
}

/// Total size from `Content-Range: bytes a-b/total`.
fn content_range_total(response: &reqwest::Response) -> Option<u64> {
    let value = response.headers().get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    value.rsplit_once('/')?.1.trim().parse().ok()
}

/// Streams `url` into `dest` through a .part file, reporting progress. A cut
/// connection is retried and resumed (HTTP Range) from what is already on disk;
/// the .part survives a restart too (see `resume_pending`).
pub(crate) async fn download(app: &AppHandle, engine: &str, item: &str, url: &str, dest: &Path) -> Result<(), String> {
    let mut attempt = 0;
    loop {
        let msg = match download_once(app, engine, item, url, dest).await {
            Ok(()) => return Ok(()),
            Err(DownloadError::Fatal(msg)) => msg,
            Err(DownloadError::Retry(msg)) if attempt < RETRY_WAITS_S.len() => {
                let wait = RETRY_WAITS_S[attempt];
                attempt += 1;
                log::line(format!("voice download {item}: {msg}; retry {attempt} in {wait} s"));
                emit_engine(app, EngineEvent {
                    ready: false,
                    engine: engine.into(),
                    downloading: Some(true),
                    item: Some(item.into()),
                    error: Some(format!("{msg}. Reintento en {wait} s")),
                    ..Default::default()
                });
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            Err(DownloadError::Retry(msg)) => msg,
        };
        log::line(format!("voice download {item}: {msg}"));
        emit_engine(app, EngineEvent {
            ready: false,
            engine: engine.into(),
            downloading: Some(false),
            item: Some(item.into()),
            error: Some(msg.clone()),
            ..Default::default()
        });
        return Err(msg);
    }
}

async fn download_once(app: &AppHandle, engine: &str, item: &str, url: &str, dest: &Path) -> Result<(), DownloadError> {
    use tokio::io::AsyncWriteExt;
    use DownloadError::{Fatal, Retry};
    let disk = |e: std::io::Error| Fatal(e.to_string());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(disk)?;
    }
    let part = part_path(dest);
    // Created before the request: a download that never got a byte is still
    // pending and gets resumed at the next start.
    let have = match std::fs::metadata(&part) {
        Ok(m) => m.len(),
        Err(_) => {
            std::fs::File::create(&part).map_err(disk)?;
            0
        }
    };
    let client = reqwest::Client::builder()
        .user_agent("Coucou")
        .connect_timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| Fatal(e.to_string()))?;
    let mut request = client.get(url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await.map_err(|e| Retry(format!("Sin conexión: {}", e.without_url())))?;
    let status = response.status();
    let (mut got, total, append) = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        let total = content_range_total(&response).or_else(|| response.content_length().map(|n| n + have));
        (have, total, true)
    } else if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let _ = std::fs::remove_file(&part);
        return Err(Retry("La descarga guardada no coincide; empiezo de nuevo".into()));
    } else if status.is_success() {
        (0, response.content_length(), false)
    } else if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(Retry(format!("El servidor respondió {status}")));
    } else {
        return Err(Fatal(format!("El servidor respondió {status}")));
    };
    if append {
        log::line(format!("voice download {item}: resuming at {} MB", have / (1024 * 1024)));
    }
    let mut file = if append {
        tokio::fs::OpenOptions::new().append(true).open(&part).await.map_err(disk)?
    } else {
        tokio::fs::File::create(&part).await.map_err(disk)?
    };
    let pct_of = |got: u64| {
        let pct = total.filter(|t| *t > 0).map(|t| (got as f64 * 100.0 / t as f64).min(100.0));
        (pct.unwrap_or(0.0) * 10.0).round() / 10.0
    };
    let mut last = Instant::now();
    emit_engine(app, EngineEvent {
        ready: false,
        engine: engine.into(),
        downloading: Some(true),
        pct: Some(pct_of(got)),
        item: Some(item.into()),
        ..Default::default()
    });
    loop {
        let next = tokio::time::timeout(Duration::from_secs(60), response.chunk()).await;
        let chunk = match next {
            Err(_) => return Err(Retry("Descarga detenida (sin datos en 60 s)".into())),
            Ok(Err(e)) => return Err(Retry(format!("Descarga interrumpida: {}", e.without_url()))),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
        };
        file.write_all(&chunk).await.map_err(disk)?;
        got += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(300) {
            last = Instant::now();
            emit_engine(app, EngineEvent {
                ready: false,
                engine: engine.into(),
                downloading: Some(true),
                pct: Some(pct_of(got)),
                item: Some(item.into()),
                ..Default::default()
            });
        }
    }
    file.flush().await.map_err(disk)?;
    drop(file);
    if let Some(total) = total {
        if got < total {
            return Err(Retry(format!("Descarga incompleta ({got} de {total} bytes)")));
        }
        if got > total {
            let _ = std::fs::remove_file(&part);
            return Err(Retry(format!("Descarga más grande de lo esperado ({got} de {total} bytes)")));
        }
    }
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest).map_err(disk)?;
    emit_engine(app, EngineEvent {
        ready: false,
        engine: engine.into(),
        downloading: Some(false),
        pct: Some(100.0),
        item: Some(item.into()),
        ..Default::default()
    });
    log::line(format!("voice: downloaded {item} ({} MB)", got / (1024 * 1024)));
    Ok(())
}

/// Unpacks a .zip with System32's bsdtar (no console window).
pub(crate) async fn extract_zip(zip: PathBuf, dest: PathBuf) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let tar = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("tar.exe");
        #[cfg(not(windows))]
        let tar = PathBuf::from("bsdtar");
        let mut cmd = Command::new(tar);
        cmd.arg("-xf").arg(&zip).arg("-C").arg(&dest).stdin(Stdio::null());
        let out = platform::no_console(&mut cmd).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("No se pudo descomprimir: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// piper.exe, downloaded on first use.
async fn ensure_piper(app: &AppHandle) -> Result<PathBuf, String> {
    if let Some(exe) = piper_exe() {
        return Ok(exe);
    }
    let _guard = install_lock().lock().await;
    if let Some(exe) = piper_exe() {
        return Ok(exe);
    }
    let zip = voice_dir().join(PIPER_ZIP);
    download(app, "piper", "piper", PIPER_URL, &zip).await?;
    // The zip holds a top-level piper\ folder.
    let result = extract_zip(zip.clone(), voice_dir()).await;
    let _ = std::fs::remove_file(&zip);
    result?;
    let exe = piper_exe().ok_or("piper.exe no apareció tras descomprimir")?;
    emit_ready(app, "piper", true);
    Ok(exe)
}

async fn ensure_piper_voice(app: &AppHandle, key: &str) -> Result<PathBuf, String> {
    let voice = PIPER_VOICES.iter().find(|v| v.key == key).ok_or("unknown_voice")?;
    let model = piper_model(key);
    if piper_voice_installed(key) {
        return Ok(model);
    }
    let _guard = install_lock().lock().await;
    if piper_voice_installed(key) {
        return Ok(model);
    }
    let json_path = PathBuf::from(format!("{}.json", model.display()));
    let base = format!("{PIPER_VOICES_BASE}/{}/{}", voice.dir, voice.key);
    if !json_path.is_file() {
        download(app, "piper", &format!("{key}.onnx.json"), &format!("{base}.onnx.json"), &json_path).await?;
    }
    if !model.is_file() {
        download(app, "piper", key, &format!("{base}.onnx"), &model).await?;
    }
    emit_ready(app, "piper", piper_exe().is_some());
    Ok(model)
}

/// An installed Piper voice to speak with while `wanted` downloads: same
/// accent first, then the default voice, then any.
fn installed_stand_in(wanted: &str) -> Option<&'static str> {
    let installed: Vec<&PiperVoice> =
        PIPER_VOICES.iter().filter(|v| v.key != wanted && piper_voice_installed(v.key)).collect();
    let lang = PIPER_VOICES.iter().find(|v| v.key == wanted).map(|v| v.lang);
    installed
        .iter()
        .find(|v| Some(v.lang) == lang)
        .or_else(|| installed.iter().find(|v| PIPER_FALLBACK_VOICE.strip_prefix("piper:") == Some(v.key)))
        .or_else(|| installed.first())
        .map(|v| v.key)
}

fn pending_installs() -> &'static Mutex<HashSet<String>> {
    static PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Downloads a Piper voice without making anyone wait for it (once per voice).
fn install_in_background(app: &AppHandle, key: &str) {
    if !pending_installs().lock().unwrap().insert(key.to_string()) {
        return;
    }
    let app = app.clone();
    let key = key.to_string();
    tauri::async_runtime::spawn(async move {
        match ensure_piper_voice(&app, &key).await {
            Ok(_) => log::line(format!("voice: {key} installed")),
            Err(e) => log::line(format!("voice: could not install {key}: {e}")),
        }
        pending_installs().lock().unwrap().remove(&key);
    });
}

/// Picks up, shortly after startup, the downloads a dropped connection or a
/// restart left half done (their .part file is still there).
pub fn resume_pending(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if part_path(&voice_dir().join(PIPER_ZIP)).exists() && piper_exe().is_none() {
            log::line("voice: resuming the Piper download");
            if let Err(e) = ensure_piper(&app).await {
                log::line(format!("voice: Piper still missing: {e}"));
            }
        }
        for v in PIPER_VOICES {
            let model = piper_model(v.key);
            let json = PathBuf::from(format!("{}.json", model.display()));
            if !piper_voice_installed(v.key) && (part_path(&model).exists() || part_path(&json).exists()) {
                log::line(format!("voice: resuming the download of {}", v.key));
                install_in_background(&app, v.key);
            }
        }
        crate::meeting::resume_pending(&app).await;
    });
}

// ── Voices and keys ───────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VoiceInfo {
    pub id: String,
    pub engine: String,
    pub name: String,
    pub lang: String,
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_mb: Option<u32>,
}

enum Voice {
    /// Microsoft Edge neural voice, by ShortName (`es-CO-SalomeNeural`).
    Edge(String),
    Piper(String),
    ElevenLabs(String),
    Azure(String),
}

fn parse_voice(id: &str) -> Result<Voice, String> {
    let (engine, rest) = id.split_once(':').ok_or("unknown_voice")?;
    let safe = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    };
    match engine {
        "edge" if edge::valid_short_name(rest) => Ok(Voice::Edge(rest.into())),
        "piper" if PIPER_VOICES.iter().any(|v| v.key == rest) => Ok(Voice::Piper(rest.into())),
        "elevenlabs" if safe(rest, 64) => Ok(Voice::ElevenLabs(rest.into())),
        "azure" if safe(rest, 80) => Ok(Voice::Azure(rest.into())),
        _ => Err("unknown_voice".into()),
    }
}

fn key_name(engine: &str) -> Result<&'static str, String> {
    match engine {
        "elevenlabs" => Ok(ELEVENLABS_KEY),
        "azure" => Ok(AZURE_KEY),
        _ => Err("unknown_engine".into()),
    }
}

/// The Azure "key" is stored as `region:key` (a bare key means eastus).
fn azure_parts(stored: &str) -> (String, String) {
    match stored.split_once(|c| c == ':' || c == '|') {
        Some((region, key)) if !region.trim().is_empty() => (region.trim().to_ascii_lowercase(), key.trim().to_string()),
        _ => ("eastus".into(), stored.trim().to_string()),
    }
}

async fn elevenlabs_voices(key: &str) -> Vec<(String, String)> {
    let fetched = async {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(8)).build().ok()?;
        let resp = client.get("https://api.elevenlabs.io/v1/voices").header("xi-api-key", key).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().await.ok()?;
        let list: Vec<(String, String)> = body["voices"]
            .as_array()?
            .iter()
            .filter_map(|v| Some((v["voice_id"].as_str()?.to_string(), v["name"].as_str().unwrap_or("?").to_string())))
            .collect();
        (!list.is_empty()).then_some(list)
    }
    .await;
    fetched.unwrap_or_else(|| ELEVENLABS_FALLBACK.iter().map(|(id, n)| (id.to_string(), n.to_string())).collect())
}

pub async fn voices() -> Vec<VoiceInfo> {
    let piper_ready = piper_exe().is_some();
    let mut out: Vec<VoiceInfo> = edge_voices()
        .await
        .into_iter()
        .map(|v| VoiceInfo {
            id: format!("edge:{}", v.short_name),
            engine: "edge".into(),
            name: v.display,
            lang: v.locale,
            installed: true,
            size_mb: None,
        })
        .collect();
    out.extend(PIPER_VOICES.iter().map(|v| VoiceInfo {
            id: format!("piper:{}", v.key),
            engine: "piper".into(),
            name: v.name.into(),
            lang: v.lang.into(),
            installed: piper_ready && piper_voice_installed(v.key),
            size_mb: Some(v.size_mb + if piper_ready { 0 } else { 22 }),
        }));
    // No `voice-engine` event from here: the settings page reloads this list on
    // that event, and emitting it back made an endless loop that redrew the rows
    // (and closed any open dropdown) many times a second.
    if let Some(key) = secrets::get(ELEVENLABS_KEY) {
        for (id, name) in elevenlabs_voices(&key).await {
            out.push(VoiceInfo { id: format!("elevenlabs:{id}"), engine: "elevenlabs".into(), name, lang: "multi".into(), installed: true, size_mb: None });
        }
    }
    if secrets::present(AZURE_KEY) {
        for (id, name, lang) in AZURE_VOICES {
            out.push(VoiceInfo { id: format!("azure:{id}"), engine: "azure".into(), name: (*name).into(), lang: (*lang).into(), installed: true, size_mb: None });
        }
    }
    out
}

/// A Bot's id from whatever the island passed (id, name or slug of the name).
/// `cursor` is the Cursor agent's pseudo-id and stays as is.
pub(crate) fn resolve_bot(app: &AppHandle, who: &str) -> String {
    let who = who.trim();
    if who.eq_ignore_ascii_case(CURSOR_ID) {
        return CURSOR_ID.to_string();
    }
    let id = grokbot::slug(who);
    grokbot::list(app)
        .into_iter()
        .find(|b| b.id == who || b.id == id || b.name.eq_ignore_ascii_case(who))
        .map(|b| b.id)
        .unwrap_or(id)
}

fn current_bot() -> &'static Mutex<Option<String>> {
    static CURRENT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(None))
}

/// The Bot last talked to (dictation, meeting, speak): `speak` without a bot uses its voice.
pub(crate) fn set_current_bot(id: &str) {
    if !id.is_empty() {
        *current_bot().lock().unwrap() = Some(id.to_string());
    }
}

/// The voice a Bot gets when the owner never picked one: by the Bot's name in
/// grokBots (ids are opaque), `cursor` for the Cursor agent, else the default.
fn default_voice_for(app: &AppHandle, bot: &str) -> &'static str {
    if bot == CURSOR_ID {
        return CURSOR_DEFAULT_VOICE;
    }
    let name = grokbot::list(app)
        .into_iter()
        .find(|b| b.id == bot)
        .map(|b| b.name)
        .unwrap_or_else(|| bot.to_string());
    let name = name.trim();
    NAMED_DEFAULTS
        .iter()
        .find(|(n, _)| name.eq_ignore_ascii_case(n))
        .map(|(_, v)| *v)
        .unwrap_or(DEFAULT_VOICE)
}

fn voice_for_bot(app: &AppHandle, bot: &str) -> String {
    let chosen = {
        let shared = app.state::<crate::Shared>();
        let settings = shared.settings.lock().unwrap();
        settings.voices.get(bot).cloned()
    };
    chosen
        .filter(|v| parse_voice(v).is_ok())
        .unwrap_or_else(|| default_voice_for(app, bot).to_string())
}

fn speak_cursor_enabled(app: &AppHandle) -> bool {
    app.state::<crate::Shared>().settings.lock().unwrap().speak_cursor
}

pub fn store_bot_voice(app: &AppHandle, bot: &str, voice_id: &str) -> Result<(), String> {
    parse_voice(voice_id)?;
    let id = resolve_bot(app, bot);
    if id.is_empty() {
        return Err("not_connected".into());
    }
    let snapshot = {
        let shared = app.state::<crate::Shared>();
        let mut current = shared.settings.lock().unwrap();
        current.voices.insert(id, voice_id.to_string());
        current.clone()
    };
    settings::save(&snapshot).map_err(|e| e.to_string())?;
    let _ = app.emit("settings-changed", snapshot);
    Ok(())
}

// ── Synthesis ─────────────────────────────────────────────────────────────────

/// Markdown and links read badly aloud.
fn clean_for_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        let word = if word.starts_with("http://") || word.starts_with("https://") {
            "(enlace)".to_string()
        } else {
            word.chars().filter(|c| !matches!(c, '*' | '_' | '#' | '`' | '>' | '|' | '~')).collect()
        };
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word);
        if out.chars().count() >= MAX_SPEAK_CHARS {
            break;
        }
    }
    out.chars().take(MAX_SPEAK_CHARS).collect()
}

/// With `stand_in`, a voice still to download is replaced by an installed one
/// (and fetched in the background) instead of keeping the speaker waiting.
async fn synth_piper(app: &AppHandle, key: &str, text: &str, stand_in: bool) -> Result<Vec<u8>, String> {
    let exe = ensure_piper(app).await?;
    let mut key = key.to_string();
    if stand_in && !piper_voice_installed(&key) {
        if let Some(other) = installed_stand_in(&key) {
            log::line(format!("voice: {key} not installed yet, speaking with {other} while it downloads"));
            install_in_background(app, &key);
            key = other.to_string();
        }
    }
    let model = ensure_piper_voice(app, &key).await?;
    let text = text.replace(['\r', '\n'], " ");
    tauri::async_runtime::spawn_blocking(move || {
        std::fs::create_dir_all(tmp_dir()).map_err(|e| e.to_string())?;
        let out_path = tmp_file("speak", "wav");
        let mut cmd = Command::new(&exe);
        cmd.arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(&out_path)
            .current_dir(exe.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = platform::no_console(&mut cmd).spawn().map_err(|e| format!("piper: {e}"))?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().ok_or("piper: sin stdin")?;
            stdin.write_all(text.as_bytes()).map_err(|e| format!("piper: {e}"))?;
            stdin.write_all(b"\n").map_err(|e| format!("piper: {e}"))?;
        }
        let out = child.wait_with_output().map_err(|e| format!("piper: {e}"))?;
        let bytes = std::fs::read(&out_path);
        let _ = std::fs::remove_file(&out_path);
        match bytes {
            Ok(b) if out.status.success() && b.len() > 44 => Ok(b),
            _ => {
                let err = String::from_utf8_lossy(&out.stderr);
                let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                Err(format!("piper falló: {last}"))
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn synth_elevenlabs(voice: &str, text: &str) -> Result<Vec<u8>, String> {
    let key = secrets::get(ELEVENLABS_KEY).ok_or("no_key")?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
    let resp = client
        .post(format!("https://api.elevenlabs.io/v1/text-to-speech/{voice}?output_format=pcm_22050"))
        .header("xi-api-key", key)
        .json(&json!({ "text": text, "model_id": "eleven_multilingual_v2" }))
        .send()
        .await
        .map_err(|e| format!("ElevenLabs sin conexión: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("ElevenLabs respondió {}", resp.status()));
    }
    let pcm = resp.bytes().await.map_err(|e| e.to_string())?;
    Ok(wav_from_pcm16(&pcm, 22050))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

async fn synth_azure(voice: &str, text: &str) -> Result<Vec<u8>, String> {
    let stored = secrets::get(AZURE_KEY).ok_or("no_key")?;
    let (region, key) = azure_parts(&stored);
    if !region.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Región de Azure inválida".into());
    }
    let lang: String = voice.splitn(3, '-').take(2).collect::<Vec<_>>().join("-");
    let ssml = format!(
        "<speak version='1.0' xml:lang='{lang}'><voice name='{voice}'>{}</voice></speak>",
        xml_escape(text)
    );
    let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| e.to_string())?;
    let resp = client
        .post(format!("https://{region}.tts.speech.microsoft.com/cognitiveservices/v1"))
        .header("Ocp-Apim-Subscription-Key", key)
        .header(reqwest::header::CONTENT_TYPE, "application/ssml+xml")
        .header("X-Microsoft-OutputFormat", "riff-24khz-16bit-mono-pcm")
        .header(reqwest::header::USER_AGENT, "Coucou")
        .body(ssml)
        .send()
        .await
        .map_err(|e| format!("Azure sin conexión: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!("Azure respondió {}", resp.status()));
    }
    Ok(resp.bytes().await.map_err(|e| e.to_string())?.to_vec())
}

/// WAV bytes for `text` in `voice_id`. A cloud voice without key or failing
/// falls back to the default Piper voice so the Bot still speaks.
async fn synth(app: &AppHandle, voice_id: &str, text: &str, stand_in: bool) -> Result<Vec<u8>, String> {
    let cloud = match parse_voice(voice_id)? {
        Voice::Edge(v) => return synth_edge(app, &v, text).await,
        Voice::Piper(key) => return synth_piper(app, &key, text, stand_in).await,
        Voice::ElevenLabs(v) => synth_elevenlabs(&v, text).await,
        Voice::Azure(v) => synth_azure(&v, text).await,
    };
    match cloud {
        Ok(wav) => Ok(wav),
        Err(e) => {
            log::line(format!("voice: {voice_id} unavailable ({e}), using Piper"));
            let Voice::Piper(key) = parse_voice(PIPER_FALLBACK_VOICE)? else { unreachable!() };
            synth_piper(app, &key, text, true).await
        }
    }
}

// ── Playback ──────────────────────────────────────────────────────────────────

/// Bumped by every speak/stop: a synthesis that finishes after a newer request
/// (or a stop) is dropped instead of played.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
fn sink_slot() -> &'static Mutex<Option<Arc<rodio::Sink>>> {
    static SINK: OnceLock<Mutex<Option<Arc<rodio::Sink>>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

#[cfg(windows)]
/// Starts playing; with `until_end`, returns only when the audio is over (or stopped).
fn play_wav(bytes: Vec<u8>, until_end: bool) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("coucou-speak".into())
        .spawn(move || {
            {
                use cpal::traits::{DeviceTrait, HostTrait};
                let device = cpal::default_host().default_output_device().and_then(|d| d.name().ok());
                log::line(format!("voice: output device {}", device.as_deref().unwrap_or("(none)")));
            }
            // The output stream is not Send: it lives and dies on this thread.
            let (stream, handle) = match rodio::OutputStream::try_default() {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = tx.send(Err(format!("Sin salida de audio: {e}")));
                    return;
                }
            };
            let sink = match rodio::Sink::try_new(&handle) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };
            match rodio::Decoder::new(std::io::Cursor::new(bytes)) {
                Ok(source) => sink.append(source),
                Err(e) => {
                    let _ = tx.send(Err(format!("Audio ilegible: {e}")));
                    return;
                }
            }
            if let Some(old) = sink_slot().lock().unwrap().replace(sink.clone()) {
                old.stop();
            }
            let _ = tx.send(Ok(()));
            let started = Instant::now();
            sink.sleep_until_end();
            log::line(format!("voice: playback ended after {} ms", started.elapsed().as_millis()));
            let mut slot = sink_slot().lock().unwrap();
            if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, &sink)) {
                *slot = None;
            }
            drop(slot);
            drop(stream);
            let _ = done_tx.send(());
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "audio_timeout".to_string())??;
    if until_end {
        let _ = done_rx.recv();
    }
    Ok(())
}

#[cfg(not(windows))]
fn play_wav(_bytes: Vec<u8>, _until_end: bool) -> Result<(), String> {
    Err("La voz solo está disponible en Windows por ahora.".into())
}

pub fn stop_playback() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    #[cfg(windows)]
    if let Some(sink) = sink_slot().lock().unwrap().take() {
        sink.stop();
    }
}

/// A reply that took longer than this to synthesise (first-time downloads) is
/// not read out of the blue minutes later.
const STALE_AFTER: Duration = Duration::from_secs(180);

async fn speak_with(app: &AppHandle, voice_id: &str, text: &str, stand_in: bool) -> Result<(), String> {
    let text = clean_for_speech(text);
    if text.is_empty() {
        log::line("voice: nothing to say after cleaning the text");
        return Ok(());
    }
    stop_playback();
    let generation = GENERATION.load(Ordering::SeqCst);
    let started = Instant::now();
    log::line(format!("voice: synthesising {} chars with {voice_id}", text.chars().count()));
    let wav = synth(app, voice_id, &text, stand_in).await?;
    if GENERATION.load(Ordering::SeqCst) != generation {
        log::line("voice: dropped (stopped or superseded while synthesising)");
        return Ok(());
    }
    if started.elapsed() > STALE_AFTER {
        log::line(format!("voice: dropped (ready after {} s, too late)", started.elapsed().as_secs()));
        return Ok(());
    }
    log::line(format!("voice: playing {} KB, synthesised in {} ms", wav.len() / 1024, started.elapsed().as_millis()));
    tauri::async_runtime::spawn_blocking(move || play_wav(wav, false)).await.map_err(|e| e.to_string())?
}

/// WAV bytes for `text` in `bot`'s voice (a voice still downloading is stood in for).
pub(crate) async fn synth_for(app: &AppHandle, bot: &str, text: &str) -> Result<Vec<u8>, String> {
    let text = clean_for_speech(text);
    if text.is_empty() {
        return Err("nothing_to_say".into());
    }
    let voice = voice_for_bot(app, bot);
    synth(app, &voice, &text, true).await
}

/// Plays WAV bytes and returns when they are over or `stop_playback` cut them.
pub(crate) async fn play_until_end(wav: Vec<u8>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || play_wav(wav, true)).await.map_err(|e| e.to_string())?
}

fn logged<T>(what: &str, result: Result<T, String>) -> Result<T, String> {
    if let Err(e) = &result {
        log::line(format!("voice: {what} failed: {e}"));
    }
    result
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Reads `text` aloud in the voice of `bot` (or the Bot last used). `bot` may
/// be `cursor` (the Cursor agent): silent when settings.speakCursor is off.
#[tauri::command]
pub async fn speak(app: AppHandle, text: String, bot: Option<String>) -> Result<(), String> {
    let bot = match bot.filter(|b| !b.trim().is_empty()) {
        Some(b) => {
            let id = resolve_bot(&app, &b);
            if id == CURSOR_ID {
                if !speak_cursor_enabled(&app) {
                    log::line("voice: Cursor notice not read (speakCursor off)");
                    return Ok(());
                }
            } else {
                // The Cursor agent never becomes "the current Bot".
                set_current_bot(&id);
            }
            Some(id)
        }
        None => current_bot().lock().unwrap().clone().or_else(|| grokbot::list(&app).first().map(|b| b.id.clone())),
    };
    // In a call, notices wait for a gap in the conversation instead of cutting in.
    if let Some(id) = bot.as_deref().filter(|_| crate::call::active()) {
        log::line(format!(
            "voice: call active, notice for {id} ({} chars) queued for the next pause",
            text.chars().count()
        ));
        crate::call::announce(id, &text);
        return Ok(());
    }
    let voice = bot.as_deref().map(|b| voice_for_bot(&app, b)).unwrap_or_else(|| DEFAULT_VOICE.to_string());
    log::line(format!("voice: speak for {} with {voice}", bot.as_deref().unwrap_or("(no bot)")));
    logged("speak", speak_with(&app, &voice, &text, true).await)
}

#[tauri::command]
pub fn stop_speaking() {
    stop_playback();
}

#[tauri::command]
pub async fn list_voices() -> Vec<VoiceInfo> {
    voices().await
}

/// The voice each Bot (and `cursor`) speaks with today: the owner's pick or the default.
#[tauri::command]
pub fn bot_voices(app: AppHandle) -> std::collections::HashMap<String, String> {
    let mut ids: Vec<String> = grokbot::list(&app).into_iter().map(|b| b.id).collect();
    ids.push(CURSOR_ID.to_string());
    ids.into_iter()
        .map(|id| {
            let voice = voice_for_bot(&app, &id);
            (id, voice)
        })
        .collect()
}

#[tauri::command]
pub fn set_bot_voice(app: AppHandle, bot: String, voice_id: String) -> Result<(), String> {
    store_bot_voice(&app, &bot, &voice_id)
}

#[tauri::command]
pub async fn preview_voice(app: AppHandle, voice_id: String, text: Option<String>) -> Result<(), String> {
    let text = text.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| PREVIEW_TEXT.to_string());
    log::line(format!("voice: preview {voice_id}"));
    // A preview is the voice itself: no stand-in.
    logged("preview", speak_with(&app, &voice_id, &text, false).await)
}

/// Downloads Piper and the voice (progress on `voice-engine`). Cloud voices only need their key.
#[tauri::command]
pub async fn install_voice(app: AppHandle, voice_id: String) -> Result<(), String> {
    log::line(format!("voice: install {voice_id}"));
    let result = match parse_voice(&voice_id)? {
        Voice::Piper(key) => async {
            ensure_piper(&app).await?;
            ensure_piper_voice(&app, &key).await?;
            emit_ready(&app, "piper", true);
            Ok(())
        }
        .await,
        // Online voices: nothing to download.
        Voice::Edge(_) => Ok(()),
        Voice::ElevenLabs(_) => secrets::present(ELEVENLABS_KEY).then_some(()).ok_or_else(|| "no_key".to_string()),
        Voice::Azure(_) => secrets::present(AZURE_KEY).then_some(()).ok_or_else(|| "no_key".to_string()),
    };
    logged("install", result)
}

/// Stores (or, with an empty key, removes) a cloud TTS key. Azure takes `region:key`.
#[tauri::command]
pub fn set_tts_key(engine: String, key: String) -> Result<(), String> {
    let name = key_name(engine.trim())?;
    let result = secrets::set(name, key.trim());
    log::line(format!("voice: {engine} key {}", if result.is_ok() { "updated" } else { "not stored" }));
    result
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct KeyStatus {
    pub elevenlabs: bool,
    pub azure: bool,
}

#[tauri::command]
pub fn tts_key_status() -> KeyStatus {
    KeyStatus { elevenlabs: secrets::present(ELEVENLABS_KEY), azure: secrets::present(AZURE_KEY) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voices_parse() {
        assert!(matches!(parse_voice("piper:es_MX-claude-high"), Ok(Voice::Piper(_))));
        assert!(parse_voice("piper:nope").is_err());
        assert!(matches!(parse_voice("azure:es-MX-DaliaNeural"), Ok(Voice::Azure(_))));
        assert!(parse_voice("elevenlabs:../x").is_err());
        assert!(parse_voice("nothing").is_err());
        assert!(matches!(parse_voice("edge:es-CO-SalomeNeural"), Ok(Voice::Edge(_))));
        assert!(parse_voice("edge:es-CO-Salome").is_err());
        for (_, v) in NAMED_DEFAULTS {
            assert!(parse_voice(v).is_ok(), "{v}");
        }
        for v in [DEFAULT_VOICE, CURSOR_DEFAULT_VOICE, PIPER_FALLBACK_VOICE] {
            assert!(parse_voice(v).is_ok(), "{v}");
        }
    }

    /// Live: reaches Microsoft's service. `cargo test -p coucou --lib edge_live -- --ignored`
    #[test]
    #[ignore]
    fn edge_live() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mp3 = rt.block_on(edge::synth_mp3("es-CO-GonzaloNeural", "Hola Miller, soy Aegon")).unwrap();
        let wav = mp3_to_wav(mp3).unwrap();
        let path = std::env::temp_dir().join("coucou-edge-live.wav");
        std::fs::write(&path, &wav).unwrap();
        let pcm = &wav[44..];
        let peak = pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap_or(0);
        println!("edge_live: {} bytes PCM, peak {peak} -> {}", pcm.len(), path.display());
        assert!(pcm.len() > 24_000 && peak > 1000);
    }

    #[test]
    fn azure_key_takes_a_region() {
        assert_eq!(azure_parts("westeurope:abc"), ("westeurope".into(), "abc".into()));
        assert_eq!(azure_parts("abc"), ("eastus".into(), "abc".into()));
    }

    #[test]
    fn speech_is_cleaned() {
        assert_eq!(clean_for_speech("**Hola** mira https://x.y  `code`"), "Hola mira (enlace) code");
    }

    #[test]
    fn wav_header() {
        let w = wav_from_pcm16(&[0, 0, 1, 0], 16000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 48);
    }
}
