// Who speaks when in a recording: pyannote's segmentation and 3D-Speaker's
// voice embeddings through the sherpa-onnx ARIA already links. The Notetaker
// uses it to tell the people of a call apart. Ported from Coucou's
// speech/mod.rs.
//
// Downloaded only on a click (about 30 MB, pinned by size and SHA-256) into
// %LOCALAPPDATA%\ARIA\voice\diarizer\.

use std::path::PathBuf;

use sherpa_rs::diarize::{Diarize, DiarizeConfig};
use tauri::AppHandle;

use super::{download, emit_engine, emit_ready, install_lock, voice_dir, EngineEvent};
use crate::log;

pub const ENGINE: &str = "diarizer";
const RATE: usize = 16_000;

struct ModelFile {
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

const FILES: &[ModelFile] = &[
    ModelFile {
        name: "segmentation.int8.onnx",
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/9403a6902bb58e3d5ae8c7e77c3422de279db2e0/model.int8.onnx",
        size: 1_540_506,
        sha256: "d582f4b4c6b48205de7e0643c57df0df5615a3c176189be3fc461e9d18827b5d",
    },
    ModelFile {
        name: "embedding.onnx",
        url: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/0743f301363dec56491a490f6d6cbc9d67f9a3bf/3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx",
        size: 28_281_164,
        sha256: "aa3cfc16963a10586a9393f5035d6d6b57e98d358b347f80c2a30bf4f00ceba2",
    },
];

pub fn dir() -> PathBuf {
    voice_dir().join("diarizer")
}

pub fn total_mb() -> u64 {
    FILES.iter().map(|f| f.size).sum::<u64>().div_ceil(1_000_000)
}

pub fn is_ready() -> bool {
    FILES.iter().all(|f| std::fs::metadata(dir().join(f.name)).map(|m| m.len() == f.size).unwrap_or(false))
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
        download(app, ENGINE, file.name, file.url, &dest).await?;
        let (check, size, sha) = (dest.clone(), file.size, file.sha256);
        let ok = tauri::async_runtime::spawn_blocking(move || {
            std::fs::metadata(&check).map(|m| m.len() == size).unwrap_or(false)
                && super::stt::sha256_file(&check).map(|h| h == sha).unwrap_or(false)
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
    log::line("voice: speaker separation ready");
    emit_ready(app, ENGINE, true);
    Ok(())
}

/// A stretch of audio where one voice speaks, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
}

/// The voices of 16 kHz mono `samples`, as turns "0", "1"… Empty when it
/// can't tell (models missing, too short).
pub fn diarize(samples: &[f32]) -> Vec<Turn> {
    if !is_ready() || samples.len() < RATE * 2 {
        return Vec::new();
    }
    let path = |name: &str| dir().join(name);
    let config = DiarizeConfig {
        // As many voices as it hears.
        num_clusters: Some(-1),
        // Higher than the default 0.5: a voice is not split in two over a call.
        threshold: Some(0.7),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let mut d = match Diarize::new(path("segmentation.int8.onnx"), path("embedding.onnx"), config) {
        Ok(d) => d,
        Err(e) => {
            log::line(format!("voice: speaker separation could not be loaded: {e}"));
            return Vec::new();
        }
    };
    let turns: Vec<Turn> = match d.compute(samples.to_vec(), None) {
        Ok(segments) => segments
            .into_iter()
            .map(|s| Turn { speaker: s.speaker.to_string(), start: s.start as f64, end: s.end as f64 })
            .collect(),
        Err(e) => {
            log::line(format!("voice: speaker separation found nothing: {e}"));
            Vec::new()
        }
    };
    let voices: std::collections::HashSet<&str> = turns.iter().map(|t| t.speaker.as_str()).collect();
    log::line(format!(
        "voice: {} voices in {:.0} s of audio ({:.1} s)",
        voices.len(),
        samples.len() as f64 / RATE as f64,
        started.elapsed().as_secs_f32()
    ));
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_pinned() {
        assert_eq!(total_mb(), 30);
        for f in FILES {
            assert_eq!(f.sha256.len(), 64, "{}", f.name);
            assert!(f.url.starts_with("https://huggingface.co/"), "{}", f.name);
        }
    }
}
