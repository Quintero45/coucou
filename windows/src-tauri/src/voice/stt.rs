// Speech to text with NVIDIA's Parakeet TDT 0.6B v3 (25 languages) through the
// sherpa-onnx ARIA already links (sherpa-rs). Far quicker than whisper-cli on a
// laptop CPU and better with the name ARIA, so dictation, meetings and
// "Hey ARIA" use it whenever it is on disk; whisper-cli stays as the fallback.
//
// Downloaded only from Settings → Voice (about 670 MB, pinned by size and
// SHA-256) into %LOCALAPPDATA%\ARIA\voice\parakeet\. Ported from Coucou's
// speech/mod.rs.

use std::ffi::{CStr, CString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sherpa_rs::sherpa_rs_sys as sys;
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

pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
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

/// Parakeet through sherpa-onnx's C API. sherpa-rs' own recognizer only gives
/// the text; the Notetaker also needs when each word was said.
struct Recognizer(*const sys::SherpaOnnxOfflineRecognizer);

unsafe impl Send for Recognizer {}

impl Drop for Recognizer {
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxDestroyOfflineRecognizer(self.0) }
    }
}

/// A word and where it is in the audio, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// What was said, and each word with its place in the audio.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    pub text: String,
    pub words: Vec<TimedWord>,
}

impl Recognizer {
    fn new() -> Result<Self, String> {
        let c = |s: &str| CString::new(s).unwrap_or_default();
        let (encoder, decoder, joiner, tokens) =
            (c(&path("encoder.int8.onnx")), c(&path("decoder.int8.onnx")), c(&path("joiner.int8.onnx")), c(&path("tokens.txt")));
        let (provider, model_type, method, empty) = (c("cpu"), c("nemo_transducer"), c("greedy_search"), c(""));
        let rec = unsafe {
            let mut config: sys::SherpaOnnxOfflineRecognizerConfig = std::mem::zeroed();
            config.model_config.transducer.encoder = encoder.as_ptr();
            config.model_config.transducer.decoder = decoder.as_ptr();
            config.model_config.transducer.joiner = joiner.as_ptr();
            config.model_config.tokens = tokens.as_ptr();
            config.model_config.num_threads = threads();
            config.model_config.provider = provider.as_ptr();
            config.model_config.model_type = model_type.as_ptr();
            config.model_config.modeling_unit = empty.as_ptr();
            config.model_config.bpe_vocab = empty.as_ptr();
            config.feat_config.sample_rate = RATE as i32;
            config.feat_config.feature_dim = 80;
            config.decoding_method = method.as_ptr();
            config.hotwords_file = empty.as_ptr();
            sys::SherpaOnnxCreateOfflineRecognizer(&config)
        };
        if rec.is_null() {
            return Err("SherpaOnnxCreateOfflineRecognizer failed".into());
        }
        Ok(Recognizer(rec))
    }

    /// The text and Parakeet's tokens with their start times, for one chunk.
    fn decode(&self, samples: &[f32]) -> (String, Vec<String>, Vec<f32>) {
        unsafe {
            let stream = sys::SherpaOnnxCreateOfflineStream(self.0);
            sys::SherpaOnnxAcceptWaveformOffline(stream, RATE as i32, samples.as_ptr(), samples.len() as i32);
            sys::SherpaOnnxDecodeOfflineStream(self.0, stream);
            let result = sys::SherpaOnnxGetOfflineStreamResult(stream);
            let mut out = (String::new(), Vec::new(), Vec::new());
            if !result.is_null() {
                let r = &*result;
                let text = |p: *const std::os::raw::c_char| {
                    if p.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(p).to_string_lossy().into_owned()
                    }
                };
                out.0 = text(r.text);
                let n = r.count.max(0) as usize;
                if n > 0 && !r.tokens_arr.is_null() {
                    out.1 = std::slice::from_raw_parts(r.tokens_arr, n).iter().map(|&p| text(p)).collect();
                }
                if n > 0 && !r.timestamps.is_null() {
                    out.2 = std::slice::from_raw_parts(r.timestamps, n).to_vec();
                }
                sys::SherpaOnnxDestroyOfflineRecognizerResult(result);
            }
            sys::SherpaOnnxDestroyOfflineStream(stream);
            out
        }
    }
}

/// Parakeet's tokens (" Ma", "ña", "na", " nos") → words with times. A token
/// starting with a space starts a word; a word ends where the next token
/// begins (or a moment after its last token, for the last word). From
/// Coucou's speech/mod.rs.
pub fn words_from_tokens(tokens: &[String], timestamps: &[f32], offset: f64, length: f64) -> Vec<TimedWord> {
    let mut out: Vec<TimedWord> = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        let at = offset + timestamps.get(i).copied().unwrap_or(0.0) as f64;
        let piece = token.trim_start_matches([' ', '▁']);
        // Numbers come without the space before them: "las" "4".
        let number_after_word = piece.starts_with(|c: char| c.is_ascii_digit())
            && out.last().and_then(|w| w.text.chars().last()).is_some_and(char::is_alphabetic);
        let starts_word = token.starts_with(' ') || token.starts_with('▁') || out.is_empty() || number_after_word;
        if starts_word {
            if let Some(last) = out.last_mut() {
                last.end = last.end.max(at);
            }
            if piece.is_empty() {
                continue;
            }
            out.push(TimedWord { text: piece.to_string(), start: at, end: at + 0.08 });
        } else if let Some(last) = out.last_mut() {
            last.text.push_str(piece);
            last.end = at + 0.08;
        }
    }
    if let Some(last) = out.last_mut() {
        last.end = (last.end + 0.25).min(offset + length).max(last.start);
    }
    out.retain(|w| !w.text.is_empty());
    out
}

struct Loaded {
    rec: Arc<Mutex<Recognizer>>,
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

fn recognizer() -> Option<Arc<Mutex<Recognizer>>> {
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
    match Recognizer::new() {
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
    transcribe_timed(samples).map(|h| h.text)
}

/// Like `transcribe`, with each word's place in `samples`.
pub fn transcribe_timed(samples: &[f32]) -> Option<Heard> {
    let rec = recognizer()?;
    let mut out = Heard::default();
    let mut rest = samples;
    let mut offset = 0usize;
    while !rest.is_empty() {
        let cut = if rest.len() > MAX_CHUNK { crate::meeting::split_point(rest, MAX_CHUNK) } else { rest.len() };
        let (chunk, tail) = rest.split_at(cut);
        rest = tail;
        let start = offset as f64 / RATE as f64;
        offset += chunk.len();
        if chunk.len() < RATE / 4 {
            continue;
        }
        let (text, tokens, times) = rec.lock().unwrap_or_else(|p| p.into_inner()).decode(chunk);
        let text = text.trim();
        if !text.is_empty() {
            if !out.text.is_empty() {
                out.text.push(' ');
            }
            out.text.push_str(text);
        }
        out.words.extend(words_from_tokens(&tokens, &times, start, chunk.len() as f64 / RATE as f64));
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
    fn tokens_make_words_with_their_times() {
        let toks: Vec<String> = [" Ma", "ña", "na", " nos", " ve", "mos", " a", " las", "4", "."].iter().map(|s| s.to_string()).collect();
        let words = words_from_tokens(&toks, &[0.0, 0.16, 0.32, 0.4, 0.56, 0.72, 0.8, 0.9, 1.0, 1.1], 10.0, 2.0);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Mañana", "nos", "vemos", "a", "las", "4."]);
        assert!((words[1].start - 10.4).abs() < 1e-6);
        assert!(words[1].end >= 10.56 - 1e-9);
        assert!(words[5].end <= 12.0);
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
        let heard = transcribe_timed(&pcm).expect("model");
        println!("{} ({:?})", heard.text, t.elapsed());
        println!("{:?}", heard.words.iter().map(|w| (&w.text, (w.start * 100.0).round() / 100.0)).collect::<Vec<_>>());
        assert!(!heard.text.is_empty());
        assert!(!heard.words.is_empty());
    }
}
