// The local speech engine's plumbing, shared by dictation and meetings
// (meeting.rs, Whisper): its folder (%LOCALAPPDATA%\Coucou\voice\), resumable
// downloads with progress on the `voice-engine` event, and unzipping.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{grokbot, log, platform, settings};

// ── Paths and small helpers (shared with meeting.rs) ──────────────────────────

/// %LOCALAPPDATA%\Coucou\voice
pub(crate) fn voice_dir() -> PathBuf {
    settings::local_dir().join("voice")
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

// ── Who a dictation or meeting is for ─────────────────────────────────────────

/// The Cursor agent's pseudo-id in the island's conversations.
pub const CURSOR_ID: &str = "cursor";

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

/// Picks up, shortly after startup, the Whisper download a dropped connection
/// or a restart left half done (its .part file is still there).
pub fn resume_pending(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(15)).await;
        crate::meeting::resume_pending(&app).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header() {
        let w = wav_from_pcm16(&[0, 0, 1, 0], 16000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 48);
    }

    #[test]
    fn part_files_sit_next_to_their_download() {
        assert_eq!(part_path(Path::new("a/b.zip")), PathBuf::from("a/b.zip.part"));
    }
}
