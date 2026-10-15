// "Hey ARIA": one thread keeps the microphone open while the setting is on.
// Silero VAD cuts speech out of it, the spotter looks for the wake phrase,
// Parakeet (or Whisper) writes what follows, and the island sends it to the chat as if typed
// — same policy, same approval cards. No audio is ever kept: samples live in
// memory for one turn; Whisper's temporary WAV is deleted as soon as it is read.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::{models, speech, turn, wake};
use crate::{log, Shared};

/// After she answers, this long to go on without the wake phrase.
const FOLLOW_UP: Duration = Duration::from_secs(8);
/// A question nobody answered frees the microphone after this long.
const REPLY_TIMEOUT: Duration = Duration::from_secs(90);
const HISTORY: usize = 30;
/// Speech up to this long (16 kHz samples) that the spotter missed is transcribed to look for the wake phrase.
const WHISPER_WAKE_MAX: usize = 4 * 16_000;
const WAKE_PROMPT: &str = "Oye ARIA. Hey ARIA. Hola ARIA.";
/// A missed phrase is logged at most this often.
const MISS_LOG_EVERY: Duration = Duration::from_secs(30);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub at: u64,
    pub text: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStatus {
    /// off, preparing, idle, listening, thinking, speaking, paused, error
    pub state: String,
    pub error: Option<String>,
    pub listening_ready: bool,
    pub speaking_ready: bool,
    pub history: Vec<Heard>,
}

struct Live {
    state: String,
    error: Option<String>,
    view: String,
    mode: String,
    pending_approval: bool,
    paused: bool,
    awaiting: Option<Instant>,
    follow_until: Option<Instant>,
    history: VecDeque<Heard>,
}

fn live() -> &'static Mutex<Live> {
    static LIVE: OnceLock<Mutex<Live>> = OnceLock::new();
    LIVE.get_or_init(|| {
        Mutex::new(Live {
            state: "off".into(),
            error: None,
            view: String::new(),
            mode: "compact".into(),
            pending_approval: false,
            paused: false,
            awaiting: None,
            follow_until: None,
            history: VecDeque::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Live> {
    live().lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Serialize, Clone)]
struct StateEvent {
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn set_state(app: &AppHandle, state: &str, error: Option<String>) {
    {
        let mut l = lock();
        if l.state == state && l.error == error {
            return;
        }
        l.state = state.into();
        l.error = error.clone();
    }
    let _ = app.emit("voice-state", StateEvent { state: state.into(), error });
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn running() -> &'static Mutex<Option<Running>> {
    static R: OnceLock<Mutex<Option<Running>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

fn settings(app: &AppHandle) -> crate::settings::Settings {
    app.state::<Shared>().settings.lock().unwrap().clone()
}

/// Starts or stops listening to match Settings (at launch and on every save).
pub fn apply(app: &AppHandle) {
    let s = settings(app);
    let want = s.voice_wake;
    let mut r = running().lock().unwrap();
    let alive = r.as_ref().is_some_and(|r| r.thread.as_ref().is_some_and(|t| !t.is_finished()));
    if want && alive && s.voice_speak && s.voice_name == "piper" && !models::speaking_ready() {
        // Her own voice picked after listening started: fetch it now.
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = models::ensure(&app, true).await {
                log::line(format!("voice: Piper not ready: {e}"));
            }
        });
    }
    if want && !alive {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (app, stop) = (app.clone(), stop.clone());
            std::thread::Builder::new().name("aria-voice".into()).spawn(move || engine(app, stop)).ok()
        };
        *r = Some(Running { stop, thread });
        log::line("voice: \"Hey ARIA\" on — the microphone stays open");
    } else if !want && r.is_some() {
        if let Some(mut old) = r.take() {
            old.stop.store(true, Ordering::SeqCst);
            speech::stop();
            // The thread notices within one tick; nothing waits on it here.
            drop(old.thread.take());
        }
        lock().paused = false;
        set_state(app, "off", None);
        log::line("voice: \"Hey ARIA\" off — microphone closed");
    }
}

// ── The engine thread ─────────────────────────────────────────────────────────

struct Turn {
    text: String,
    started: Instant,
    last: Instant,
}

fn engine(app: AppHandle, stop: Arc<AtomicBool>) {
    if let Err(e) = run(&app, &stop) {
        log::line(format!("voice: stopped: {e}"));
        set_state(&app, "error", Some(e));
    }
}

#[cfg(windows)]
fn run(app: &AppHandle, stop: &AtomicBool) -> Result<(), String> {
    use sherpa_rs::silero_vad::{SileroVad, SileroVadConfig};

    set_state(app, "preparing", None);
    let speak = settings(app).voice_speak && settings(app).voice_name == "piper";
    tauri::async_runtime::block_on(models::ensure(app, speak))?;
    let (exe, model) = tauri::async_runtime::block_on(crate::meeting::ensure_whisper(app))?;
    let spotter = wake::Spotter::new(&models::kws_dir())?;
    if !models::vad_path().is_file() {
        return Err("falta el modelo de voz (silero_vad.onnx)".into());
    }
    let mut vad = SileroVad::new(
        SileroVadConfig {
            model: models::vad_path().to_string_lossy().into_owned(),
            min_silence_duration: 0.4,
            min_speech_duration: 0.25,
            max_speech_duration: 12.0,
            threshold: 0.5,
            sample_rate: 16_000,
            window_size: 512,
            provider: Some("cpu".into()),
            num_threads: Some(1),
            debug: false,
        },
        30.0,
    )
    .map_err(|e| e.to_string())?;
    let heard = |samples: Vec<f32>, prompt: Option<&'static str>| -> String {
        match tauri::async_runtime::block_on(crate::meeting::transcribe_hinted(exe.clone(), model.clone(), samples, prompt)) {
            Ok(t) => t.trim().to_string(),
            Err(e) => {
                log::line(format!("voice: {e}"));
                String::new()
            }
        }
    };
    let transcribe = |samples: Vec<f32>| heard(samples, None);
    // Without the hint Whisper writes the name as «área», «haría», «Daria»…
    let transcribe_wake = |samples: Vec<f32>| heard(samples, Some(WAKE_PROMPT));
    super::stt::keep_loaded(true);
    super::stt::preload();
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            super::stt::keep_loaded(false);
        }
    }
    let _release = Release;

    let mut backoff = wake::Backoff::default();
    while !stop.load(Ordering::SeqCst) {
        let opened = Instant::now();
        let capture = match crate::meeting::Capture::start(true, false) {
            Ok(c) => c,
            Err(e) => {
                log::line(format!("voice: microphone: {e}"));
                match backoff.ended(opened.elapsed()) {
                    wake::Action::Stop => return Err(format!("micrófono: {e}")),
                    wake::Action::Delay(d) => std::thread::sleep(d),
                    wake::Action::Open => {}
                }
                continue;
            }
        };
        backoff.reset();
        set_state(app, if lock().paused { "paused" } else { "idle" }, None);
        log::line("voice: listening for the wake phrase");
        let mut current: Option<Turn> = None;
        let mut last_miss = Instant::now().checked_sub(MISS_LOG_EVERY).unwrap_or_else(Instant::now);

        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(60));
            let samples = capture.take();
            let (busy, follow) = {
                let mut l = lock();
                if l.awaiting.is_some_and(|t| t.elapsed() >= REPLY_TIMEOUT) {
                    l.awaiting = None;
                }
                if l.follow_until.is_some_and(|t| Instant::now() >= t) {
                    l.follow_until = None;
                }
                (l.paused || l.awaiting.is_some() || speech::speaking(), l.follow_until.is_some())
            };
            if busy {
                // Her own voice, a pause or a pending answer: nothing is heard.
                vad.clear();
                current = None;
                continue;
            }
            if !follow && current.is_none() && lock().state != "idle" {
                set_state(app, "idle", None);
            }
            if !samples.is_empty() {
                vad.accept_waveform(samples);
            }
            while !vad.is_empty() {
                let segment = louder(vad.front().samples);
                vad.pop();
                if let Some(t) = current.as_mut() {
                    let more = transcribe(segment);
                    if !more.is_empty() {
                        if !t.text.is_empty() {
                            t.text.push(' ');
                        }
                        t.text.push_str(&more);
                    }
                    t.last = Instant::now();
                } else if follow {
                    let text = transcribe(segment);
                    if !text.is_empty() {
                        set_state(app, "listening", None);
                        current = Some(Turn { text, started: Instant::now(), last: Instant::now() });
                    }
                } else if segment.len() >= 6_400 {
                    let blocked = || {
                        let l = lock();
                        wake::wake_blocked(&l.view, &l.mode, l.pending_approval)
                    };
                    let text = if let Some(keyword) = spotter.find(&segment) {
                        if let Some(why) = blocked() {
                            log::line(format!("voice: wake ignored ({why})"));
                            continue;
                        }
                        log::line(format!("voice: wake phrase heard ({keyword})"));
                        set_state(app, "listening", None);
                        let _ = crate::sounds::play_sound(app.state::<Shared>(), "recibido".into(), None);
                        let heard = transcribe_wake(segment);
                        let split = wake::split(&heard);
                        if split.matched { split.command } else { after_name(&heard) }
                    } else if segment.len() <= WHISPER_WAKE_MAX {
                        // The spotter is an English model: a Spanish "Hey ARIA" often slips past it.
                        let secs = segment.len() as f32 / 16_000.0;
                        let heard = transcribe_wake(segment);
                        let split = wake::split(&heard);
                        if !split.matched {
                            // Only the length: what was said near the microphone is never written down.
                            if last_miss.elapsed() >= MISS_LOG_EVERY {
                                last_miss = Instant::now();
                                log::line(format!("voice: speech ({secs:.1} s) without the wake phrase"));
                            }
                            continue;
                        }
                        if let Some(why) = blocked() {
                            log::line(format!("voice: wake ignored ({why})"));
                            continue;
                        }
                        log::line("voice: wake phrase heard (whisper)");
                        set_state(app, "listening", None);
                        let _ = crate::sounds::play_sound(app.state::<Shared>(), "recibido".into(), None);
                        split.command
                    } else {
                        continue;
                    };
                    current = Some(Turn { text, started: Instant::now(), last: Instant::now() });
                }
            }
            let done = current.as_ref().is_some_and(|t| {
                let quiet = !vad.is_speech() && t.last.elapsed() >= turn::silence_delay(&t.text, false);
                quiet || t.started.elapsed() >= turn::MAX_TURN
            });
            if done {
                if let Some(t) = current.take() {
                    finish(app, t.text);
                }
            }
        }
        drop(capture);
    }
    Ok(())
}

#[cfg(not(windows))]
fn run(_app: &AppHandle, _stop: &AtomicBool) -> Result<(), String> {
    Err("«Hey ARIA» solo está disponible en Windows por ahora.".into())
}

/// A quiet microphone brought up to a level the spotter and Whisper expect.
pub(super) fn louder(mut samples: Vec<f32>) -> Vec<f32> {
    let peak = samples.iter().fold(0f32, |m, x| m.max(x.abs()));
    if peak > 0.0 {
        let gain = (0.6 / peak).clamp(1.0, 30.0);
        for s in &mut samples {
            *s *= gain;
        }
    }
    samples
}

/// The spotter heard the phrase but Whisper wrote the name its own way: what
/// comes after the first word that sounds like it, within the first three.
fn after_name(heard: &str) -> String {
    let words: Vec<&str> = heard.split_whitespace().collect();
    let at = words.iter().take(3).position(|w| {
        let n = turn::normalise(w);
        n.ends_with("ria") || n.ends_with("rya") || n.ends_with("aria")
    });
    match at {
        Some(i) => words[i + 1..].join(" ").trim_start_matches([',', '.', ';', ':', '-', '—', ' ']).to_string(),
        None => String::new(),
    }
}

fn finish(app: &AppHandle, text: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        lock().follow_until = None;
        set_state(app, "idle", None);
        return;
    }
    if turn::pauses_listening(&text) {
        {
            let mut l = lock();
            l.paused = true;
            l.follow_until = None;
        }
        log::line("voice: paused by voice");
        set_state(app, "paused", None);
        return;
    }
    if turn::ends_conversation(&text) {
        lock().follow_until = None;
        set_state(app, "idle", None);
        return;
    }
    {
        let mut l = lock();
        l.follow_until = None;
        l.awaiting = Some(Instant::now());
        l.history.push_front(Heard { at: super::now_ms(), text: text.clone() });
        l.history.truncate(HISTORY);
    }
    log::line(format!("voice: heard a request ({} chars), sent to the chat", text.chars().count()));
    set_state(app, "thinking", None);
    let _ = app.emit("voice-heard", serde_json::json!({ "text": text }));
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn voice_status() -> VoiceStatus {
    let l = lock();
    VoiceStatus {
        state: l.state.clone(),
        error: l.error.clone(),
        listening_ready: models::listening_ready(),
        speaking_ready: models::speaking_ready(),
        history: l.history.iter().cloned().collect(),
    }
}

/// What the island shows, for the wake filter.
#[tauri::command]
pub fn voice_island(view: String, mode: String, pending_approval: bool) {
    let mut l = lock();
    l.view = view;
    l.mode = mode;
    l.pending_approval = pending_approval;
}

/// The chat answered a request heard by voice: say it, then listen without
/// the wake phrase for a moment.
#[tauri::command]
pub fn voice_reply(app: AppHandle, text: String) {
    lock().awaiting = None;
    let s = settings(&app);
    if !s.voice_wake {
        return;
    }
    if !s.voice_speak || text.trim().is_empty() {
        lock().follow_until = Some(Instant::now() + FOLLOW_UP);
        set_state(&app, "listening", None);
        return;
    }
    let voice = s.voice_name.clone();
    std::thread::spawn(move || {
        set_state(&app, "speaking", None);
        if let Err(e) = speech::say(&text, &voice) {
            log::line(format!("voice: could not speak: {e}"));
        }
        lock().follow_until = Some(Instant::now() + FOLLOW_UP);
        set_state(&app, "listening", None);
    });
}

/// Back to listening after «Pausa» (or a click on the paused island).
#[tauri::command]
pub fn voice_resume(app: AppHandle) {
    lock().paused = false;
    log::line("voice: listening again");
    set_state(&app, "idle", None);
}

#[tauri::command]
pub fn voice_stop_speaking() {
    speech::stop();
}

/// Says a sample sentence with `voice` (Settings → Voice → Try).
#[tauri::command]
pub async fn voice_try(voice: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || speech::say("Hola, soy ARIA. ¿En qué te ayudo?", &voice))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn voice_voices() -> Vec<String> {
    tauri::async_runtime::spawn_blocking(speech::windows_voices).await.unwrap_or_default()
}

#[tauri::command]
pub fn voice_clear_history() {
    lock().history.clear();
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParakeetStatus {
    pub ready: bool,
    pub size_mb: u64,
}

#[tauri::command]
pub fn voice_parakeet_status() -> ParakeetStatus {
    ParakeetStatus { ready: super::stt::is_ready(), size_mb: super::stt::total_mb() }
}

/// Settings → Voice → Download (progress on `voice-engine`, engine `parakeet`).
#[tauri::command]
pub async fn voice_parakeet_download(app: AppHandle) -> Result<(), String> {
    super::stt::ensure(&app).await
}

#[tauri::command]
pub fn voice_parakeet_remove(app: AppHandle) {
    super::stt::remove(&app);
}

#[cfg(test)]
mod tests {
    use super::after_name;

    #[test]
    fn a_misspelt_name_still_leaves_the_command() {
        assert_eq!(after_name("Oye María, abre Spotify"), "abre Spotify");
        assert_eq!(after_name("Hey, Arya. Pon música"), "Pon música");
        assert_eq!(after_name("abre el correo por favor"), "");
    }
}
