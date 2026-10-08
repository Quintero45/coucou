// UI sounds: four short chimes the island plays from Rust, off the UI thread.
//
//   recibido  message sent / received   ~120 ms soft pop            (peak -6 dBFS)
//   pregunta  a question or approval    ~350 ms rising two-note chime (-3 dBFS)
//   listo     a task finished           ~450 ms descending resolve    (-3 dBFS)
//   error     something failed          ~300 ms soft low double tone  (-3 dBFS)
//
// The WAVs (44.1 kHz mono 16-bit) were synthesised for Coucou, so they are
// royalty-free, and are embedded in the binary.
//
// The front calls `play_sound({ name, volume })` (core/botcmds.ts) after its
// own "Sonidos de Coucou" switch. Here the app-wide "Sonido" switch
// (Settings.sound_enabled, read-only) still wins, and the gain is the volume
// the front passes, or Settings.sound_volume when it passes none.

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tauri::State;

use crate::Shared;

/// The same sound asked for again within this window is ignored.
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiSound {
    Recibido,
    Pregunta,
    Listo,
    Error,
}

impl UiSound {
    /// The front's names (core/botcmds.ts `CoucouSound`). Case and spaces are forgiven.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "recibido" => Some(Self::Recibido),
            "pregunta" => Some(Self::Pregunta),
            "listo" => Some(Self::Listo),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Recibido => "recibido",
            Self::Pregunta => "pregunta",
            Self::Listo => "listo",
            Self::Error => "error",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn wav(self) -> &'static [u8] {
        match self {
            Self::Recibido => include_bytes!("../sounds/recibido.wav"),
            Self::Pregunta => include_bytes!("../sounds/pregunta.wav"),
            Self::Listo => include_bytes!("../sounds/listo.wav"),
            Self::Error => include_bytes!("../sounds/error.wav"),
        }
    }
}

/// The gain to play at, or None when nothing should sound: the "Sonido" switch
/// is off, or the volume is zero / not a number. `volume` (0–1, from the front)
/// wins over the stored `sound_volume`; both are clamped to 0–1.
fn resolve_gain(sound_enabled: bool, sound_volume: f64, volume: Option<f32>) -> Option<f32> {
    if !sound_enabled {
        return None;
    }
    let v = volume.map(f64::from).unwrap_or(sound_volume);
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    Some(v.min(1.0) as f32)
}

/// Last time each sound actually started; a repeat inside DEBOUNCE is dropped
/// (and does not push the window further out).
struct Debounce {
    last: [Option<Instant>; 4],
}

impl Debounce {
    const fn new() -> Self {
        Self { last: [None; 4] }
    }

    fn allow(&mut self, sound: UiSound, now: Instant) -> bool {
        let slot = &mut self.last[sound.index()];
        if let Some(prev) = *slot {
            if now.saturating_duration_since(prev) < DEBOUNCE {
                return false;
            }
        }
        *slot = Some(now);
        true
    }
}

static RECENT: Mutex<Debounce> = Mutex::new(Debounce::new());

/// A poisoned lock must never turn a chime into a panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Plays one of the four UI sounds without waiting for it. Silently does
/// nothing while "Sonido" is off, at volume 0, during a voice call, or for a
/// repeat within 250 ms; never fails for want of an audio device (logged once).
#[tauri::command]
pub fn play_sound(shared: State<Shared>, name: String, volume: Option<f32>) -> Result<(), String> {
    let sound = UiSound::parse(&name).ok_or_else(|| format!("Sonido desconocido: {name}"))?;
    let gain = {
        let s = lock(&shared.settings);
        resolve_gain(s.sound_enabled, s.sound_volume, volume)
    };
    let Some(gain) = gain else { return Ok(()) };
    // Don't chime over a voice call. (meeting.rs has no public "active" yet,
    // so meetings and dictation are not checked here.)
    if crate::call::active() {
        return Ok(());
    }
    if !lock(&RECENT).allow(sound, Instant::now()) {
        return Ok(());
    }
    player::play(sound, gain)
}

#[cfg(windows)]
mod player {
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use rodio::Source;

    use super::{lock, UiSound};
    use crate::log;

    /// The output stream is released after this much quiet, so an idle island
    /// holds no audio device (and picks up a new default device next time).
    const IDLE: Duration = Duration::from_secs(10);

    struct Job {
        sound: UiSound,
        gain: f32,
    }

    /// One long-lived thread owns the (non-Send) output stream.
    fn queue() -> Option<&'static Mutex<Sender<Job>>> {
        static TX: OnceLock<Option<Mutex<Sender<Job>>>> = OnceLock::new();
        TX.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            match std::thread::Builder::new().name("coucou-sounds".into()).spawn(move || run(rx)) {
                Ok(_) => Some(Mutex::new(tx)),
                Err(e) => {
                    log::line(format!("sounds: no player thread: {e}"));
                    None
                }
            }
        })
        .as_ref()
    }

    pub fn play(sound: UiSound, gain: f32) -> Result<(), String> {
        let tx = queue().ok_or_else(|| "Sin reproductor de sonidos".to_string())?;
        lock(tx).send(Job { sound, gain }).map_err(|_| "El reproductor de sonidos se detuvo".to_string())
    }

    fn run(rx: Receiver<Job>) {
        let mut out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
        // "Log once": one line per stretch of failures, not one per chime.
        let mut warned = false;
        loop {
            let next = if out.is_some() {
                rx.recv_timeout(IDLE)
            } else {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            let job = match next {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => {
                    out = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // A stale stream (device unplugged) gets one fresh retry.
            let mut played = Err(String::new());
            for _ in 0..2 {
                if out.is_none() {
                    match rodio::OutputStream::try_default() {
                        Ok(pair) => out = Some(pair),
                        Err(e) => {
                            played = Err(format!("no audio output: {e}"));
                            break;
                        }
                    }
                }
                let Some((_, handle)) = out.as_ref() else { break };
                played = start(handle, &job);
                if played.is_ok() {
                    break;
                }
                out = None;
            }
            match played {
                Ok(()) => warned = false,
                Err(e) if !warned => {
                    log::line(format!("sounds: {} not played: {e}", job.sound.name()));
                    warned = true;
                }
                Err(_) => {}
            }
        }
    }

    fn start(handle: &rodio::OutputStreamHandle, job: &Job) -> Result<(), String> {
        let source = rodio::Decoder::new_wav(std::io::Cursor::new(job.sound.wav()))
            .map_err(|e| format!("bad wav: {e}"))?
            .convert_samples::<f32>()
            .amplify(job.gain);
        handle.play_raw(source).map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
mod player {
    use super::UiSound;

    /// No rodio off Windows (see Cargo.toml): the island stays silent there.
    pub fn play(sound: UiSound, _gain: f32) -> Result<(), String> {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| crate::log::line(format!("sounds: {} skipped, UI sounds are Windows-only for now", sound.name())));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [UiSound; 4] = [UiSound::Recibido, UiSound::Pregunta, UiSound::Listo, UiSound::Error];

    #[test]
    fn parses_the_four_names() {
        for s in ALL {
            assert_eq!(UiSound::parse(s.name()), Some(s));
        }
        assert_eq!(UiSound::parse(" Listo "), Some(UiSound::Listo));
        assert_eq!(UiSound::parse("PREGUNTA"), Some(UiSound::Pregunta));
        assert_eq!(UiSound::parse(""), None);
        assert_eq!(UiSound::parse("finish"), None);
        assert_eq!(UiSound::parse("../error"), None);
    }

    #[test]
    fn indexes_are_distinct() {
        let mut seen = [false; 4];
        for s in ALL {
            assert!(!seen[s.index()]);
            seen[s.index()] = true;
        }
    }

    /// Header of each embedded file: PCM, mono, 44.1 kHz, 16-bit, expected length.
    #[test]
    fn embedded_wavs_are_short_mono_pcm() {
        let expected_ms = [(UiSound::Recibido, 120), (UiSound::Pregunta, 350), (UiSound::Listo, 450), (UiSound::Error, 300)];
        for (s, ms) in expected_ms {
            let b = s.wav();
            assert_eq!(&b[0..4], b"RIFF", "{}", s.name());
            assert_eq!(&b[8..12], b"WAVE", "{}", s.name());
            assert_eq!(&b[12..16], b"fmt ", "{}", s.name());
            let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
            let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            assert_eq!(u16_at(20), 1, "PCM");
            assert_eq!(u16_at(22), 1, "mono");
            assert_eq!(u32_at(24), 44_100, "rate");
            assert_eq!(u16_at(34), 16, "bits");
            assert_eq!(&b[36..40], b"data", "{}", s.name());
            let frames = u32_at(40) / 2;
            assert_eq!(frames * 1000 / 44_100, ms, "{}", s.name());
            // Starts and ends silent (fades: no click).
            let sample = |n: usize| i16::from_le_bytes([b[44 + 2 * n], b[45 + 2 * n]]);
            assert!(sample(0).unsigned_abs() < 40, "{} starts loud", s.name());
            assert!(sample(frames as usize - 1).unsigned_abs() < 40, "{} ends loud", s.name());
        }
    }

    #[test]
    fn debounce_drops_quick_repeats_of_the_same_sound_only() {
        let mut d = Debounce::new();
        let t0 = Instant::now();
        assert!(d.allow(UiSound::Listo, t0));
        assert!(!d.allow(UiSound::Listo, t0 + Duration::from_millis(100)));
        // A different sound is not affected.
        assert!(d.allow(UiSound::Error, t0 + Duration::from_millis(100)));
        // A dropped repeat does not extend the window.
        assert!(!d.allow(UiSound::Listo, t0 + Duration::from_millis(249)));
        assert!(d.allow(UiSound::Listo, t0 + Duration::from_millis(250)));
        assert!(!d.allow(UiSound::Listo, t0 + Duration::from_millis(400)));
        assert!(d.allow(UiSound::Listo, t0 + Duration::from_millis(600)));
    }

    #[test]
    fn debounce_tolerates_an_earlier_instant() {
        let mut d = Debounce::new();
        let t0 = Instant::now() + Duration::from_secs(1);
        assert!(d.allow(UiSound::Recibido, t0));
        assert!(!d.allow(UiSound::Recibido, t0 - Duration::from_millis(500)));
    }

    #[test]
    fn gain_is_the_passed_volume_else_the_stored_one() {
        assert_eq!(resolve_gain(true, 0.12, Some(0.3)), Some(0.3));
        assert_eq!(resolve_gain(true, 0.12, None), Some(0.12));
        assert_eq!(resolve_gain(true, 0.12, Some(1.7)), Some(1.0));
        assert_eq!(resolve_gain(true, 4.0, None), Some(1.0));
        assert_eq!(resolve_gain(true, 0.12, Some(0.0)), None);
        assert_eq!(resolve_gain(true, 0.12, Some(-0.5)), None);
        assert_eq!(resolve_gain(true, 0.12, Some(f32::NAN)), None);
        assert_eq!(resolve_gain(true, f64::NAN, None), None);
        assert_eq!(resolve_gain(true, 0.0, None), None);
        // A passed 0 does not fall back to the stored volume.
        assert_eq!(resolve_gain(true, 0.5, Some(0.0)), None);
    }

    #[test]
    fn sonido_off_wins_over_any_volume() {
        assert_eq!(resolve_gain(false, 0.12, None), None);
        assert_eq!(resolve_gain(false, 0.12, Some(0.8)), None);
    }

    /// The two fields this module reads, as settings.json carries them: on and
    /// 0.12 by default (also for an older file without them), and an explicit
    /// `soundEnabled: false` silences play_sound.
    #[test]
    fn reads_sound_enabled_and_volume_from_settings_json() {
        let d = crate::settings::Settings::default();
        assert_eq!(resolve_gain(d.sound_enabled, d.sound_volume, None), Some(0.12));

        let old: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert!(old.sound_enabled);
        assert_eq!(old.sound_volume, 0.12);

        let off: crate::settings::Settings =
            serde_json::from_str(r#"{"soundEnabled": false, "soundVolume": 0.3}"#).unwrap();
        assert_eq!(resolve_gain(off.sound_enabled, off.sound_volume, Some(0.5)), None);
        let quiet: crate::settings::Settings = serde_json::from_str(r#"{"soundVolume": 0.3}"#).unwrap();
        assert_eq!(resolve_gain(quiet.sound_enabled, quiet.sound_volume, None), Some(0.3));
    }

    /// Peaks the generator wrote: -6 dBFS for recibido, -3 dBFS for the rest.
    #[test]
    fn embedded_wavs_peak_where_expected() {
        for (s, peak_db) in [(UiSound::Recibido, -6.0f64), (UiSound::Pregunta, -3.0), (UiSound::Listo, -3.0), (UiSound::Error, -3.0)] {
            let b = s.wav();
            let peak = b[44..]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                .max()
                .unwrap();
            let db = 20.0 * (f64::from(peak) / 32768.0).log10();
            assert!((db - peak_db).abs() < 0.3, "{} peaks at {db:.2} dBFS", s.name());
        }
    }
}
