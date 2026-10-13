// What "Hey ARIA" downloads the first time it is turned on, pinned by size and
// SHA-256: the voice detector, the wake-phrase spotter and, for her own voice,
// Piper with its Mexican Spanish voice. Never in the installer, never in git.

use std::path::{Path, PathBuf};

use tauri::AppHandle;

use super::{download, emit_ready, extract_zip, find_file, install_lock, voice_dir};
use crate::log;
use crate::selfmod::guard::sha256::sha256_hex;

pub struct Model {
    pub name: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub const VAD: Model = Model {
    name: "silero_vad.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
    size: 643_854,
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
};

pub const KWS: Model = Model {
    name: "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models/sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01.tar.bz2",
    size: 17_626_723,
    sha256: "f170013b4716e41b62b9bfd809687c207cef798ef9bc6534d524e17af9b6561a",
};

pub const PIPER: Model = Model {
    name: "piper_windows_amd64.zip",
    url: "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip",
    size: 22_477_236,
    sha256: "f3c58906402b24f3a96d92145f58acba6d86c9b5db896d207f78dc80811efcea",
};

pub const PIPER_VOICE: Model = Model {
    name: "es_MX-claude-high.onnx",
    url: "https://huggingface.co/rhasspy/piper-voices/resolve/main/es/es_MX/claude/high/es_MX-claude-high.onnx",
    size: 63_122_309,
    sha256: "3ef40a71ea63852cd8ab7e6fa7d2ecdcfa67a0b47c9c48e3f10e02ee02083ea0",
};

pub const PIPER_VOICE_JSON: Model = Model {
    name: "es_MX-claude-high.onnx.json",
    url: "https://huggingface.co/rhasspy/piper-voices/resolve/main/es/es_MX/claude/high/es_MX-claude-high.onnx.json",
    size: 4_963,
    sha256: "1afc81f703c0e4cb3b4d7c0dca096b8b54a98806807f0170cf5eb5557723c12d",
};

pub fn vad_path() -> PathBuf {
    voice_dir().join(VAD.name)
}

pub fn kws_dir() -> PathBuf {
    voice_dir().join("kws")
}

fn piper_dir() -> PathBuf {
    voice_dir().join("piper")
}

pub fn piper_exe() -> Option<PathBuf> {
    find_file(&piper_dir(), "piper.exe", 2)
}

pub fn piper_voice() -> PathBuf {
    piper_dir().join(PIPER_VOICE.name)
}

fn kws_ready() -> bool {
    kws_dir().join("tokens.txt").is_file()
}

fn piper_ready() -> bool {
    piper_exe().is_some() && piper_voice().is_file() && piper_dir().join(PIPER_VOICE_JSON.name).is_file()
}

/// Whether what listening needs is on disk (Whisper is meeting.rs's).
pub fn listening_ready() -> bool {
    vad_path().is_file() && kws_ready()
}

pub fn speaking_ready() -> bool {
    piper_ready()
}

/// The file matches its pinned size and hash; a bad one is deleted.
fn verify(path: &Path, model: &Model) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 == model.size && sha256_hex(&bytes) == model.sha256 {
        return Ok(());
    }
    let _ = std::fs::remove_file(path);
    log::line(format!("voice: {} failed its SHA-256 check, deleted", model.name));
    Err(format!("{} no pasó la comprobación SHA-256", model.name))
}

async fn fetch(app: &AppHandle, model: &Model, dest: &Path) -> Result<(), String> {
    download(app, "wake", model.name, model.url, dest).await?;
    let (dest, size, sha) = (dest.to_path_buf(), model.size, model.sha256);
    let name = model.name;
    tauri::async_runtime::spawn_blocking(move || verify(&dest, &Model { name, url: "", size, sha256: sha }))
        .await
        .map_err(|e| e.to_string())?
}

/// Downloads, checks and unpacks whatever listening (and, with `speak`,
/// Piper's voice) still needs.
pub async fn ensure(app: &AppHandle, speak: bool) -> Result<(), String> {
    if listening_ready() && (!speak || piper_ready()) {
        return Ok(());
    }
    let _guard = install_lock().lock().await;
    if !vad_path().is_file() {
        fetch(app, &VAD, &vad_path()).await?;
    }
    if !kws_ready() {
        let archive = voice_dir().join(KWS.name);
        fetch(app, &KWS, &archive).await?;
        let staging = voice_dir().join("kws-unpack");
        let _ = std::fs::remove_dir_all(&staging);
        let unpacked = extract_zip(archive.clone(), staging.clone()).await;
        let _ = std::fs::remove_file(&archive);
        unpacked?;
        let tokens = find_file(&staging, "tokens.txt", 2).ok_or("el modelo de la frase no trae tokens.txt")?;
        let _ = std::fs::remove_dir_all(kws_dir());
        std::fs::rename(tokens.parent().unwrap_or(&staging), kws_dir()).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&staging);
    }
    if speak && !piper_ready() {
        if piper_exe().is_none() {
            let zip = voice_dir().join(PIPER.name);
            fetch(app, &PIPER, &zip).await?;
            let unpacked = extract_zip(zip.clone(), piper_dir()).await;
            let _ = std::fs::remove_file(&zip);
            unpacked?;
        }
        for model in [&PIPER_VOICE, &PIPER_VOICE_JSON] {
            let dest = piper_dir().join(model.name);
            if !dest.is_file() {
                fetch(app, model, &dest).await?;
            }
        }
    }
    emit_ready(app, "wake", true);
    log::line("voice: wake models ready");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_does_not_match_is_deleted() {
        let dir = std::env::temp_dir().join(format!("aria-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.onnx");
        std::fs::write(&file, b"abc").unwrap();
        let good = Model {
            name: "m.onnx",
            url: "",
            size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verify(&file, &good).is_ok());
        let bad = Model { size: 4, ..good };
        assert!(verify(&file, &bad).is_err());
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_model_is_pinned() {
        for m in [&VAD, &KWS, &PIPER, &PIPER_VOICE, &PIPER_VOICE_JSON] {
            assert_eq!(m.sha256.len(), 64, "{}", m.name);
            assert!(m.url.starts_with("https://"), "{}", m.name);
            assert!(m.size > 0);
        }
    }
}
