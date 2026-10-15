// Speech to text with NVIDIA's Parakeet TDT 0.6B v3 (25 languages) through the
// sherpa-onnx ARIA already links (sherpa-rs). Far quicker than whisper-cli on a
// laptop CPU and better with the name ARIA, so dictation, meetings and
// "Hey ARIA" use it whenever it is on disk; whisper-cli stays as the fallback.
//
// Downloaded only from Settings → Voice (about 670 MB, pinned by size and
// SHA-256) into %LOCALAPPDATA%\ARIA\voice\parakeet\. Ported from Coucou's
// speech/mod.rs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};
use tauri::AppHandle;

use super::{download, emit_engine, emit_ready, install_lock, voice_dir, EngineEvent};
use crate::log;

pub const ENGINE: &str = "parakeet";
const RATE: usize = 16_000;
/// Longest stretch given to the model at once; longer audio is cut where it is quietest.
const MAX_CHUNK: usize = RATE * 25;
/// Unused this long (and not kept by "Hey ARIA"), the model leaves memory.
const IDLE_UNLOAD: Duration = Duration::from_secs(10 * 60);

struct ModelFile {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

const REPO: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/resolve/2bda32ec70b097a55adaa07d9a7173915b43cc78";

const FILES: &[ModelFile] = &[
    ModelFile {
        name: "tokens.txt",
        size: 93_939,
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    },
    ModelFile {
        name: "decoder.int8.onnx",
        size: 11_845_275,
        sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
    },
    ModelFile {
        name: "joiner.int8.onnx",
        size: 6_355_277,
        sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
    },
    ModelFile {
        name: "encoder.int8.onnx",
        size: 652_184_281,
        sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
    },
];

pub fn dir() -> PathBuf {
    voice_dir().join("parakeet")
}

fn path(name: &str) -> String {
    dir().join(name).to_string_lossy().into_owned()
}

pub fn total_mb() -> u64 {
    FILES.iter().map(|f| f.size).sum::<u64>() / 1_000_000
}

/// Every file is there with its size (the hash was checked when it came in).
pub fn is_ready() -> bool {
    FILES.iter().all(|f| std::fs::metadata(dir().join(f.name)).map(|m| m.len() == f.size).unwrap_or(false))
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Downloads and checks whatever is missing, with progress on `voice-engine`.
pub async fn ensure(app: &AppHandle) -> Result<(), String> {
    if is_ready() {
        emit_ready(app, ENGINE, true);
        return Ok(());
    }
    let _guard = install_lock().lock().await;
    for file in FILES {
        let dest = dir().join(file.name);
        if std::fs::metadata(&dest).map(|m| m.len() == file.size).unwrap_or(false) {
            continue;
        }
        download(app, ENGINE, file.name, &format!("{REPO}/{}", file.name), &dest).await?;
        let (check, size, sha) = (dest.clone(), file.size, file.sha256);
        let ok = tauri::async_runtime::spawn_blocking(move || {
            std::fs::metadata(&check).map(|m| m.len() == size).unwrap_or(false)
                && sha256_file(&check).map(|h| h == sha).unwrap_or(false)
        })
        .await
        .map_err(|e| e.to_string())?;
        if !ok {
            let _ = std::fs::remove_file(&dest);
            log::line(format!("voice: {} failed its SHA-256 check, deleted", file.name));
            let msg = format!("{} no pasó la comprobación SHA-256", file.name);
            emit_engine(app, EngineEvent { engine: ENGINE.into(), error: Some(msg.clone()), ..Default::default() });
            return Err(msg);
        }
    }
    log::line("voice: Parakeet ready");
    emit_ready(app, ENGINE, true);
    preload();
    Ok(())
}

/// Settings → Voice → Remove: frees the memory and the disk.
pub fn remove(app: &AppHandle) {
    unload();
    let _ = std::fs::remove_dir_all(dir());
    log::line("voice: Parakeet removed");
    emit_ready(app, ENGINE, false);
}

// ── Recognition ───────────────────────────────────────────────────────────────

struct Loaded {
    rec: Arc<Mutex<TransducerRecognizer>>,
    last_used: Instant,
}

fn slot() -> &'static Mutex<Option<Loaded>> {
    static S: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

/// Two callers never load the 650 MB model twice.
fn load_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Set by "Hey ARIA" while it listens: the model then stays in memory.
static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn keep_loaded(on: bool) {
    KEEP.store(on, std::sync::atomic::Ordering::SeqCst);
}

fn threads() -> i32 {
    std::thread::available_parallelism().map(|n| n.get().clamp(1, 4) as i32).unwrap_or(2)
}

fn recognizer() -> Option<Arc<Mutex<TransducerRecognizer>>> {
    if let Some(l) = slot().lock().unwrap().as_mut() {
        l.last_used = Instant::now();
        return Some(l.rec.clone());
    }
    if !is_ready() {
        return None;
    }
    let _guard = load_lock().lock().unwrap();
    if let Some(l) = slot().lock().unwrap().as_ref() {
        return Some(l.rec.clone());
    }
    let started = Instant::now();
    let config = TransducerConfig {
        encoder: path("encoder.int8.onnx"),
        decoder: path("decoder.int8.onnx"),
        joiner: path("joiner.int8.onnx"),
        tokens: path("tokens.txt"),
        model_type: "nemo_transducer".into(),
        num_threads: threads(),
        sample_rate: RATE as i32,
        feature_dim: 80,
        decoding_method: "greedy_search".into(),
        ..Default::default()
    };
    match TransducerRecognizer::new(config) {
        Ok(rec) => {
            log::line(format!("voice: Parakeet loaded in {:.1} s", started.elapsed().as_secs_f32()));
            let rec = Arc::new(Mutex::new(rec));
            *slot().lock().unwrap() = Some(Loaded { rec: rec.clone(), last_used: Instant::now() });
            watch_idle();
            Some(rec)
        }
        Err(e) => {
            log::line(format!("voice: Parakeet could not be loaded: {e}"));
            None
        }
    }
}

/// While the model is loaded, once a minute: unload it after IDLE_UNLOAD unused.
fn watch_idle() {
    let _ = std::thread::Builder::new().name("aria-parakeet-idle".into()).spawn(|| loop {
        std::thread::sleep(Duration::from_secs(60));
        let mut s = slot().lock().unwrap();
        let Some(l) = s.as_ref() else { return };
        if !KEEP.load(std::sync::atomic::Ordering::SeqCst) && l.last_used.elapsed() >= IDLE_UNLOAD {
            *s = None;
            log::line("voice: Parakeet unloaded (unused)");
            return;
        }
    });
}

/// Loads the model in the background, so the first phrase is not slowed down.
pub fn preload() {
    if is_ready() && slot().lock().unwrap().is_none() {
        std::thread::spawn(|| {
            let _ = recognizer();
        });
    }
}

pub fn unload() {
    *slot().lock().unwrap() = None;
}

/// What was said in 16 kHz mono `samples`, or `None` when Parakeet is not there.
pub fn transcribe(samples: &[f32]) -> Option<String> {
    let rec = recognizer()?;
    let mut out = String::new();
    let mut rest = samples;
    while !rest.is_empty() {
        let cut = if rest.len() > MAX_CHUNK { crate::meeting::split_point(rest, MAX_CHUNK) } else { rest.len() };
        let (chunk, tail) = rest.split_at(cut);
        rest = tail;
        if chunk.len() < RATE / 4 {
            continue;
        }
        let text = rec.lock().unwrap_or_else(|p| p.into_inner()).transcribe(RATE as u32, chunk);
        let text = text.trim();
        if !text.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_pinned() {
        assert_eq!(total_mb(), 670);
        for f in FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
        }
        assert!(REPO.starts_with("https://huggingface.co/"));
    }

    #[test]
    fn hashes_are_streamed() {
        let file = std::env::temp_dir().join(format!("aria-stt-{}", std::process::id()));
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(&file);
    }

    /// With the model on disk: `cargo test -- --ignored parakeet_hears --nocapture`.
    #[test]
    #[ignore]
    fn parakeet_hears_a_wav() {
        let wav = std::env::var("ARIA_TEST_WAV").expect("ARIA_TEST_WAV");
        let bytes = std::fs::read(wav).unwrap();
        let at = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
        let pcm: Vec<f32> = bytes[at..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
        let t = Instant::now();
        let text = transcribe(&pcm).expect("model");
        println!("{text} ({:?})", t.elapsed());
        assert!(!text.is_empty());
    }
}
