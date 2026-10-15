// ARIA's own voice: Kokoro 1.0 (Apache 2.0) through the sherpa-onnx ARIA
// already links, with its Spanish voices (Dora, Alex, Santa) read by espeak-ng.
// Everything runs on this PC.
//
// Downloaded only on a click (about 350 MB, pinned by size and SHA-256) into
// %LOCALAPPDATA%\ARIA\voice\kokoro\. Loaded when she speaks, released after a
// few quiet minutes.

use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sherpa_rs::sherpa_rs_sys as sys;
use tauri::AppHandle;

use super::{download, emit_engine, emit_ready, extract_zip, find_file, install_lock, voice_dir, EngineEvent};
use crate::log;

pub const ENGINE: &str = "kokoro";

// The full-precision model: on a 4-core laptop it speaks about 2.7× faster than
// real time, the int8 one barely keeps up and she would stutter between sentences.
const ARCHIVE_NAME: &str = "kokoro-multi-lang-v1_0.tar.bz2";
const ARCHIVE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2";
const ARCHIVE_SIZE: u64 = 349_906_910;
const ARCHIVE_SHA256: &str = "c5f7e2d2caf082bc1d20fb70334a61d99d20b484500aad32e7cf84c128ea3298";

/// Unloaded after this long without speaking.
const IDLE: Duration = Duration::from_secs(180);

/// Her voices, by the value Settings stores: (setting, speaker id in the model).
pub const VOICES: &[(&str, i32)] = &[("kokoro:dora", 28), ("kokoro:alex", 29), ("kokoro:santa", 53)];

pub fn speaker(voice: &str) -> Option<i32> {
    VOICES.iter().find(|(v, _)| *v == voice).map(|(_, id)| *id)
}

pub fn dir() -> PathBuf {
    voice_dir().join("kokoro")
}

pub fn total_mb() -> u64 {
    ARCHIVE_SIZE.div_ceil(1_000_000)
}

pub fn is_ready() -> bool {
    let d = dir();
    ["model.onnx", "voices.bin", "tokens.txt"].iter().all(|f| d.join(f).is_file()) && d.join("espeak-ng-data").is_dir()
}

/// Downloads, checks and unpacks the voice, with progress on `voice-engine`.
pub async fn ensure(app: &AppHandle) -> Result<(), String> {
    if is_ready() {
        emit_ready(app, ENGINE, true);
        return Ok(());
    }
    let _guard = install_lock().lock().await;
    if is_ready() {
        return Ok(());
    }
    let archive = voice_dir().join(ARCHIVE_NAME);
    download(app, ENGINE, "Kokoro", ARCHIVE_URL, &archive).await?;
    let check = archive.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || {
        std::fs::metadata(&check).map(|m| m.len() == ARCHIVE_SIZE).unwrap_or(false)
            && super::stt::sha256_file(&check).map(|h| h == ARCHIVE_SHA256).unwrap_or(false)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !ok {
        let _ = std::fs::remove_file(&archive);
        log::line(format!("voice: {ARCHIVE_NAME} failed its SHA-256 check, deleted"));
        let msg = format!("{ARCHIVE_NAME} no pasó la comprobación SHA-256");
        emit_engine(app, EngineEvent { engine: ENGINE.into(), error: Some(msg.clone()), ..Default::default() });
        return Err(msg);
    }
    let staging = voice_dir().join("kokoro-unpack");
    let _ = std::fs::remove_dir_all(&staging);
    let unpacked = extract_zip(archive.clone(), staging.clone()).await;
    let _ = std::fs::remove_file(&archive);
    unpacked?;
    let model = find_file(&staging, "model.onnx", 2).ok_or("Kokoro no trae model.onnx")?;
    let _ = std::fs::remove_dir_all(dir());
    std::fs::rename(model.parent().unwrap_or(&staging), dir()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    log::line("voice: Kokoro ready");
    emit_ready(app, ENGINE, true);
    Ok(())
}

pub fn remove() {
    unload();
    let _ = std::fs::remove_dir_all(dir());
}

struct Tts(*const sys::SherpaOnnxOfflineTts);

// The engine is only used behind ENGINE_SLOT's lock.
unsafe impl Send for Tts {}

impl Drop for Tts {
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxDestroyOfflineTts(self.0) }
    }
}

impl Tts {
    fn new() -> Result<Tts, String> {
        let d = dir();
        let c = |p: PathBuf| CString::new(p.to_string_lossy().into_owned()).map_err(|e| e.to_string());
        let model = c(d.join("model.onnx"))?;
        let voices = c(d.join("voices.bin"))?;
        let tokens = c(d.join("tokens.txt"))?;
        let data = c(d.join("espeak-ng-data"))?;
        // Required by the multilingual model even when it reads Spanish through espeak-ng.
        let dict = c(d.join("dict"))?;
        let lexicon = CString::new(format!(
            "{},{}",
            d.join("lexicon-us-en.txt").to_string_lossy(),
            d.join("lexicon-zh.txt").to_string_lossy()
        ))
        .map_err(|e| e.to_string())?;
        let empty = CString::default();
        let cpu = CString::new("cpu").unwrap();
        let lang = CString::new("es").unwrap();
        let threads = std::thread::available_parallelism().map(|n| n.get().clamp(1, 4)).unwrap_or(2) as i32;
        let tts = unsafe {
            let mut config: sys::SherpaOnnxOfflineTtsConfig = std::mem::zeroed();
            config.model.kokoro.model = model.as_ptr();
            config.model.kokoro.voices = voices.as_ptr();
            config.model.kokoro.tokens = tokens.as_ptr();
            config.model.kokoro.data_dir = data.as_ptr();
            config.model.kokoro.length_scale = 1.0;
            config.model.kokoro.dict_dir = dict.as_ptr();
            config.model.kokoro.lexicon = lexicon.as_ptr();
            config.model.kokoro.lang = lang.as_ptr();
            config.model.num_threads = threads;
            config.model.provider = cpu.as_ptr();
            config.rule_fsts = empty.as_ptr();
            config.rule_fars = empty.as_ptr();
            config.max_num_sentences = 1;
            config.silence_scale = 0.2;
            sys::SherpaOnnxCreateOfflineTts(&config)
        };
        if tts.is_null() {
            return Err("Kokoro no se pudo cargar".into());
        }
        Ok(Tts(tts))
    }

    fn generate(&self, text: &str, sid: i32, speed: f32) -> Result<(Vec<f32>, u32), String> {
        let text = CString::new(text.replace('\0', " ")).map_err(|e| e.to_string())?;
        unsafe {
            let audio = sys::SherpaOnnxOfflineTtsGenerate(self.0, text.as_ptr(), sid, speed);
            if audio.is_null() {
                return Err("Kokoro no produjo audio".into());
            }
            let a = &*audio;
            let samples = if a.samples.is_null() || a.n <= 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(a.samples, a.n as usize).to_vec()
            };
            let rate = a.sample_rate.max(1) as u32;
            sys::SherpaOnnxDestroyOfflineTtsGeneratedAudio(audio);
            Ok((samples, rate))
        }
    }
}

static ENGINE_SLOT: Mutex<Option<(Tts, Instant)>> = Mutex::new(None);

fn unload() {
    ENGINE_SLOT.lock().unwrap().take();
}

/// `text` said by `sid`, at `speed` (1.0 normal), as mono samples and their rate.
pub fn generate(text: &str, sid: i32, speed: f32) -> Result<(Vec<f32>, u32), String> {
    with_engine(|tts| tts.generate(text, sid, speed))?
}

/// Loads the model ahead of an answer (a cold load reads 325 MB). Blocking.
pub fn preload() {
    if is_ready() {
        if let Err(e) = with_engine(|_| ()) {
            log::line(format!("voice: Kokoro could not be loaded: {e}"));
        }
    }
}

fn with_engine<R>(f: impl FnOnce(&Tts) -> R) -> Result<R, String> {
    if !is_ready() {
        return Err("Kokoro no está descargado".into());
    }
    let mut slot = ENGINE_SLOT.lock().unwrap();
    let fresh = slot.is_none();
    if fresh {
        let started = Instant::now();
        *slot = Some((Tts::new()?, Instant::now()));
        log::line(format!("voice: Kokoro loaded ({:.1} s)", started.elapsed().as_secs_f32()));
    }
    let (tts, used) = slot.as_mut().unwrap();
    *used = Instant::now();
    let out = f(tts);
    drop(slot);
    if fresh {
        std::thread::Builder::new()
            .name("aria-kokoro-idle".into())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(30));
                let mut slot = ENGINE_SLOT.lock().unwrap();
                match slot.as_ref() {
                    Some((_, used)) if used.elapsed() < IDLE => {}
                    Some(_) => {
                        slot.take();
                        log::line("voice: Kokoro unloaded");
                        return;
                    }
                    None => return,
                }
            })
            .ok();
    }
    Ok(out)
}

/// What she says, cut into sentences: the first one plays while the next is
/// made. Very short pieces join the next one; very long ones break at a comma.
pub fn sentences(text: &str) -> Vec<String> {
    const SHORT: usize = 24;
    const LONG: usize = 220;
    let mut raw: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        let next_is_space = chars.get(i + 1).is_none_or(|n| n.is_whitespace());
        let end = matches!(c, '.' | '!' | '?' | '…' | ';' | ':') && next_is_space;
        let long_break = c == ',' && next_is_space && cur.chars().count() >= LONG;
        if end || long_break || c == '\n' {
            let piece = cur.trim().to_string();
            if !piece.is_empty() {
                raw.push(piece);
            }
            cur.clear();
        }
    }
    let rest = cur.trim().to_string();
    if !rest.is_empty() {
        raw.push(rest);
    }
    let mut out: Vec<String> = Vec::new();
    let mut pending = String::new();
    for piece in raw {
        if !pending.is_empty() {
            pending.push(' ');
        }
        pending.push_str(&piece);
        if pending.chars().count() >= SHORT {
            out.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        match out.last_mut() {
            Some(last) => {
                last.push(' ');
                last.push_str(&pending);
            }
            None => out.push(pending),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_is_pinned() {
        assert_eq!(ARCHIVE_SHA256.len(), 64);
        assert!(ARCHIVE_URL.starts_with("https://github.com/k2-fsa/sherpa-onnx/"));
        assert_eq!(total_mb(), 350);
        assert_eq!(speaker("kokoro:dora"), Some(28));
        assert_eq!(speaker("piper"), None);
    }

    #[test]
    fn answers_are_cut_into_sentences() {
        assert_eq!(
            sentences("Hola. Ya abrí Spotify y puse tu lista de siempre. ¿Quieres que suba el volumen un poco?"),
            vec!["Hola. Ya abrí Spotify y puse tu lista de siempre.", "¿Quieres que suba el volumen un poco?"]
        );
        assert_eq!(sentences("Listo. ¿Algo más?"), vec!["Listo. ¿Algo más?"]);
        assert_eq!(sentences("Son las 4.30 de la tarde, Miller."), vec!["Son las 4.30 de la tarde, Miller."]);
        assert!(sentences("").is_empty());
    }

    /// Writes `%TEMP%\aria-kokoro-<name>.wav` for each voice (needs the model
    /// downloaded): `cargo test kokoro_speaks -- --ignored`.
    #[test]
    #[ignore]
    fn kokoro_speaks() {
        let text = "Hola Miller, soy ARIA. Ya terminé la fase cuatro: las notas de reuniones funcionan.";
        for (voice, sid) in VOICES {
            let started = Instant::now();
            let (samples, rate) = generate(text, *sid, 1.0).expect("generate");
            let secs = samples.len() as f32 / rate as f32;
            println!("{voice}: {secs:.1} s of audio in {:.1} s", started.elapsed().as_secs_f32());
            let pcm: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
            let name = voice.replace(':', "-");
            std::fs::write(std::env::temp_dir().join(format!("aria-{name}.wav")), super::super::wav_from_pcm16(&pcm, rate)).unwrap();
        }
    }
}
