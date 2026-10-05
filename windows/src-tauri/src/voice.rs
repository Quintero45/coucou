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
pub const DEFAULT_VOICE: &str = "piper:es_MX-claude-high";
/// Pseudo-bot id for the Cursor agent: not a Grok Bot, but it has a voice too.
pub const CURSOR_ID: &str = "cursor";
/// Default voice per Bot *name* (case-insensitive) when settings.voices has no entry.
const NAMED_DEFAULTS: &[(&str, &str)] = &[
    ("aegon", "piper:es_MX-ald-medium"),
    ("aerys", "piper:es_MX-claude-high"),
    ("daemond", "piper:es_ES-davefx-medium"),
];
const CURSOR_DEFAULT_VOICE: &str = "piper:es_AR-daniela-high";
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
        .or_else(|| installed.iter().find(|v| DEFAULT_VOICE.strip_prefix("piper:") == Some(v.key)))
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

pub async fn voices(app: &AppHandle) -> Vec<VoiceInfo> {
    let piper_ready = piper_exe().is_some();
    let mut out: Vec<VoiceInfo> = PIPER_VOICES
        .iter()
        .map(|v| VoiceInfo {
            id: format!("piper:{}", v.key),
            engine: "piper".into(),
            name: v.name.into(),
            lang: v.lang.into(),
            installed: piper_ready && piper_voice_installed(v.key),
            size_mb: Some(v.size_mb + if piper_ready { 0 } else { 22 }),
        })
        .collect();
    emit_ready(app, "piper", piper_ready && PIPER_VOICES.iter().any(|v| piper_voice_installed(v.key)));
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
        Voice::Piper(key) => return synth_piper(app, &key, text, stand_in).await,
        Voice::ElevenLabs(v) => synth_elevenlabs(&v, text).await,
        Voice::Azure(v) => synth_azure(&v, text).await,
    };
    match cloud {
        Ok(wav) => Ok(wav),
        Err(e) => {
            log::line(format!("voice: {voice_id} unavailable ({e}), using Piper"));
            let Voice::Piper(key) = parse_voice(DEFAULT_VOICE)? else { unreachable!() };
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
fn play_wav(bytes: Vec<u8>) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
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
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "audio_timeout".to_string())?
}

#[cfg(not(windows))]
fn play_wav(_bytes: Vec<u8>) -> Result<(), String> {
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
    tauri::async_runtime::spawn_blocking(move || play_wav(wav)).await.map_err(|e| e.to_string())?
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
    let voice = bot.as_deref().map(|b| voice_for_bot(&app, b)).unwrap_or_else(|| DEFAULT_VOICE.to_string());
    log::line(format!("voice: speak for {} with {voice}", bot.as_deref().unwrap_or("(no bot)")));
    logged("speak", speak_with(&app, &voice, &text, true).await)
}

#[tauri::command]
pub fn stop_speaking() {
    stop_playback();
}

#[tauri::command]
pub async fn list_voices(app: AppHandle) -> Vec<VoiceInfo> {
    voices(&app).await
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
