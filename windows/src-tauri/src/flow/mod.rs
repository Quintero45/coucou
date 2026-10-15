// Flow dictation: hold a key anywhere, speak, let go, and the words are pasted
// where you type. From Coucou's flow/ (FlowDictation.swift).
//
// One thread owns the dictation: the key's events come in from the OS listener
// (os_windows.rs) and the pill's buttons, KeyLogic turns them into start /
// lock / finish / cancel, the microphone is opened on the first press, and
// when you let go the words go to Parakeet on this computer (voice/stt.rs),
// then through the dictionary, the snippets and (if turned on) the AI
// clean-up, and are pasted. Off by default; while off nothing listens. While
// on, the thread sleeps on its channel until the key moves.

pub mod cleanup;
pub mod logic;
pub mod paste;
pub mod pill;
pub mod store;
pub mod text;

#[cfg(windows)]
#[path = "os_windows.rs"]
pub mod os;
#[cfg(not(windows))]
#[path = "os_other.rs"]
pub mod os;

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::i18n::t;
use crate::meeting::Capture;
use crate::voice::stt;
use logic::{Action, KeyLogic};
use store::{AppStyle, Entry, Prefs, Store, Words};
use text::Snippet;

/// What reaches the dictation thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Down,
    Up,
    /// Another key while the dictation key is held: a shortcut.
    Other,
    Escape,
    /// The pill's ✓: paste now.
    Finish,
    /// The pill's ✕.
    Cancel,
    Quit,
}

pub struct Flow {
    pub store: Mutex<Store>,
    running: Mutex<Option<Running>>,
}

struct Running {
    tx: Sender<(KeyEvent, Instant)>,
    /// The OS's key listener; dropping it stops it.
    _listener: Option<os::KeyListener>,
}

impl Flow {
    fn send(&self, event: KeyEvent) {
        if let Some(r) = self.running.lock().unwrap().as_ref() {
            let _ = r.tx.send((event, Instant::now()));
        }
    }
}

/// A program's name as people know it: "outlook" → "Outlook".
pub fn display_name(stem: &str) -> String {
    let id = stem.to_lowercase();
    if let Some(s) = store::default_app_styles().into_iter().find(|s| s.id == id) {
        return s.name;
    }
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Parts of the desktop itself, never a place to dictate into.
pub fn is_shell(id: &str) -> bool {
    matches!(
        id,
        "explorer" | "applicationframehost" | "textinputhost" | "shellexperiencehost" | "searchhost"
            | "startmenuexperiencehost" | "lockapp" | "aria" | "coucou"
    )
}

// ── Setup, on and off ─────────────────────────────────────────────────────────

pub fn setup(app: &AppHandle) {
    let flow = Arc::new(Flow { store: Mutex::new(Store::load()), running: Mutex::new(None) });
    app.manage(flow.clone());
    pill::setup(app);
    let prefs = flow.store.lock().unwrap().prefs.clone();
    pill::update(app, |s| s.key = key_id(prefs.key));
    if prefs.enabled {
        enable(app, &flow);
    }
}

/// The key as the page writes it ("right_ctrl"…).
fn key_id(key: logic::FlowKey) -> String {
    serde_json::to_value(key).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

fn enable(app: &AppHandle, flow: &Arc<Flow>) {
    let mut running = flow.running.lock().unwrap();
    if running.is_some() {
        return;
    }
    let prefs = flow.store.lock().unwrap().prefs.clone();
    let (tx, rx) = mpsc::channel::<(KeyEvent, Instant)>();
    let listener = match os::keys_supported().map_err(String::from).and_then(|()| os::listen(prefs.key, tx.clone())) {
        Ok(l) => Some(l),
        Err(err) => {
            crate::log::line(format!("flow: the key can't be watched: {err}"));
            None
        }
    };
    let app2 = app.clone();
    let flow2 = flow.clone();
    std::thread::Builder::new()
        .name("aria-flow".into())
        .spawn(move || run(app2, flow2, rx))
        .ok();
    *running = Some(Running { tx, _listener: listener });
    crate::log::line(format!("flow dictation on ({})", prefs.key.label()));
}

fn disable(flow: &Arc<Flow>) {
    let running = flow.running.lock().unwrap().take();
    if let Some(r) = running {
        let _ = r.tx.send((KeyEvent::Quit, Instant::now()));
        crate::log::line("flow dictation off");
    }
}

// ── The dictation thread ──────────────────────────────────────────────────────

struct Listening {
    capture: Capture,
    samples: Vec<f32>,
    peak: f32,
    started: Instant,
    app: Option<(String, String)>,
}

impl Listening {
    /// Moves what the microphone caught into the take (ten minutes at most: a
    /// dictation left open by mistake stops growing).
    fn pull(&mut self) {
        let new = self.capture.take();
        self.peak = new.iter().fold(self.peak, |m, &s| m.max(s.abs()));
        if self.samples.len() < 16_000 * 600 {
            self.samples.extend_from_slice(&new);
        }
    }
}

fn run(app: AppHandle, flow: Arc<Flow>, rx: mpsc::Receiver<(KeyEvent, Instant)>) {
    let epoch = Instant::now();
    let secs = |at: Instant| at.saturating_duration_since(epoch).as_secs_f64();
    let mut logic = KeyLogic::default();
    let mut listening: Option<Listening> = None;
    let mut meter = Meter::default();
    loop {
        let active = listening.is_some() || logic.is_active();
        let received = if active {
            match rx.recv_timeout(Duration::from_millis(33)) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            }
        };
        let action = match received {
            Some((KeyEvent::Quit, _)) => {
                drop(listening.take());
                break;
            }
            Some((KeyEvent::Down, at)) => logic.key_down(secs(at)),
            Some((KeyEvent::Up, at)) => logic.key_up(secs(at)),
            Some((KeyEvent::Other, _)) => logic.other_key(),
            Some((KeyEvent::Escape, _)) => logic.escape(),
            Some((KeyEvent::Finish, _)) => {
                if logic.is_active() || listening.is_some() {
                    logic.reset();
                    Action::Finish
                } else {
                    Action::None
                }
            }
            Some((KeyEvent::Cancel, _)) => {
                logic.reset();
                Action::Cancel
            }
            None => logic.tick(secs(Instant::now())),
        };
        match action {
            Action::None => {}
            Action::Start => {
                listening = begin(&app);
                if listening.is_none() {
                    logic.reset();
                }
            }
            Action::Restart => {
                drop(listening.take());
                listening = begin(&app);
                if listening.is_none() {
                    logic.reset();
                }
            }
            Action::Lock => pill::update(&app, |s| s.phase = "locked".into()),
            Action::Cancel => {
                drop(listening.take());
                pill::update(&app, |s| s.phase = "idle".into());
            }
            Action::Finish => {
                if let Some(l) = listening.take() {
                    finish(&app, &flow, l);
                } else {
                    pill::update(&app, |s| s.phase = "idle".into());
                }
            }
        }
        // The pill's dots, about 30 times a second while you speak.
        if let Some(l) = listening.as_mut() {
            let from = l.samples.len();
            l.pull();
            meter.feed(&l.samples[from..]);
            let _ = app.emit_to(pill::LABEL, "pill-bands", meter.levels);
        }
    }
    pill::update(&app, |s| s.phase = "idle".into());
}

/// Opens the microphone. None (and the pill says why) when it can't.
fn begin(app: &AppHandle) -> Option<Listening> {
    if !stt::is_ready() {
        pill::announce(
            app,
            "download",
            "#3B9EFF",
            t("To dictate, download Parakeet first: Settings → Voice → Speech recognition."),
            Some((t("Open Settings"), "openVoice")),
            10,
        );
        return None;
    }
    // Loads while you speak, so it is ready when you let go.
    stt::preload();
    match Capture::start(true, false) {
        Ok(capture) => {
            let app_front = os::foreground_app().filter(|(id, _)| !is_shell(id));
            pill::update(app, |s| s.phase = "listening".into());
            Some(Listening { capture, samples: Vec::new(), peak: 0.0, started: Instant::now(), app: app_front })
        }
        Err(err) => {
            crate::log::line(format!("flow: microphone: {err}"));
            pill::update(app, |s| s.phase = "failed".into());
            pill::rest_after(app, "failed", 1800);
            pill::announce(
                app,
                "mic",
                "#FF6B6B",
                t("ARIA can't hear you: the microphone is not allowed or not available."),
                None,
                8,
            );
            None
        }
    }
}

/// Stops listening and turns what was said into pasted text, on a worker
/// thread so the key works again at once.
fn finish(app: &AppHandle, flow: &Arc<Flow>, mut l: Listening) {
    l.pull();
    drop(l.capture);
    let seconds = l.started.elapsed().as_secs_f64();
    // Development builds only: a 16 kHz mono WAV stands in for the microphone,
    // so the whole path (key, model, paste) can be tested without a voice.
    #[cfg(debug_assertions)]
    if let Some(bytes) = std::env::var("ARIA_FLOW_TEST_WAV").ok().and_then(|p| std::fs::read(p).ok()) {
        if let Some(at) = bytes.windows(4).position(|w| w == b"data") {
            l.samples = bytes[at + 8..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect();
            l.peak = 1.0;
        }
    }
    // Too short or silent: nothing to send to the model.
    if l.samples.len() < 16_000 / 4 || l.peak < 0.004 {
        pill::update(app, |s| s.phase = "idle".into());
        return;
    }
    pill::update(app, |s| s.phase = "thinking".into());
    let app = app.clone();
    let flow = flow.clone();
    let front = l.app.clone();
    let samples = l.samples;
    std::thread::spawn(move || {
        if let Err(err) = process(&app, &flow, &samples, front.as_ref(), seconds) {
            crate::log::line(format!("flow: {err}"));
            pill::update(&app, |s| s.phase = "idle".into());
        }
    });
}

fn process(app: &AppHandle, flow: &Arc<Flow>, samples: &[f32], front: Option<&(String, String)>, seconds: f64) -> Result<(), String> {
    let started = Instant::now();
    let raw = stt::transcribe(samples).ok_or("Parakeet is not loaded")?.trim().to_string();
    if raw.is_empty() {
        pill::update(app, |s| s.phase = "idle".into());
        return Ok(());
    }
    let (prepared, style, cleanup, words, key) = {
        let store = flow.store.lock().unwrap();
        let app_id = front.map(|(id, _)| id.as_str());
        (store.prepare(&raw), store.style_for(app_id), store.prefs.cleanup, store.words.words.clone(), store.prefs.key)
    };
    let mut written = prepared.text.clone();
    if cleanup && style != store::Style::Verbatim {
        let app_name = front.map(|(_, n)| n.clone());
        let cleaned = tauri::async_runtime::block_on(cleanup::clean(app, &prepared.text, style, app_name.as_deref(), &words));
        if text::keeps_marks(&cleaned, &prepared.snippets) {
            written = cleaned;
        }
    }
    let final_text = text::finish(&written, &prepared.snippets);
    crate::log::line(format!(
        "flow: {} words pasted in {} ({:?})",
        text::word_count(&final_text),
        front.map(|(_, n)| n.as_str()).unwrap_or("?"),
        started.elapsed()
    ));

    os::wait_key_released(key);
    let outcome = paste::paste(&final_text, false);
    let entry = {
        let mut store = flow.store.lock().unwrap();
        store.record(&final_text, front.map(|(_, n)| n.as_str()), seconds, store::today(), store::now_ms())
    };
    let _ = app.emit("flow-history", entry);
    match outcome {
        paste::Outcome::Pasted => pill::update(app, |s| s.phase = "idle".into()),
        paste::Outcome::Copied => {
            pill::update(app, |s| s.phase = "copied".into());
            pill::rest_after(app, "copied", 1800);
            pill::announce(
                app,
                "clipboard",
                "#F7B32B",
                t("Your words are copied: click where you want them and press Ctrl+V."),
                None,
                8,
            );
        }
        paste::Outcome::Failed => {
            pill::update(app, |s| s.phase = "idle".into());
            return Err("the clipboard refused the text".into());
        }
    }
    Ok(())
}

// ── The pill's dots ───────────────────────────────────────────────────────────

const FFT: usize = 512;

/// Nine frequency bands of the last 32 ms of voice, 0…1 each.
struct Meter {
    window: Vec<f32>,
    ranges: Vec<(usize, usize)>,
    levels: [f32; logic::bands::COUNT],
}

impl Default for Meter {
    fn default() -> Self {
        Meter {
            window: Vec::with_capacity(FFT),
            ranges: logic::bands::bin_ranges(FFT, 16_000.0, logic::bands::COUNT),
            levels: [0.0; logic::bands::COUNT],
        }
    }
}

impl Meter {
    fn feed(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        self.window.extend_from_slice(samples);
        if self.window.len() > FFT {
            self.window.drain(..self.window.len() - FFT);
        }
        if self.window.len() < FFT {
            return;
        }
        let n = FFT as f64;
        // Hann window: no smearing between bands.
        let x: Vec<f64> = self
            .window
            .iter()
            .enumerate()
            .map(|(i, &s)| s as f64 * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1.0)).cos()))
            .collect();
        for (band, &(from, to)) in self.ranges.iter().enumerate() {
            let mut power = 0.0;
            for k in from..to {
                // One DFT bin, the phasor turned by multiplication rather than sin/cos.
                let w = 2.0 * std::f64::consts::PI * k as f64 / n;
                let (step_re, step_im) = (w.cos(), -w.sin());
                let (mut p_re, mut p_im) = (1.0f64, 0.0f64);
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for &v in &x {
                    re += v * p_re;
                    im += v * p_im;
                    (p_re, p_im) = (p_re * step_re - p_im * step_im, p_re * step_im + p_im * step_re);
                }
                power += (re * re + im * im) / (n * n);
            }
            let db = 10.0 * (power / (to - from).max(1) as f64 + 1e-12).log10();
            self.levels[band] = logic::bands::level(db) as f32;
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowStatus {
    prefs: Prefs,
    /// Parakeet is on disk.
    model_ready: bool,
    /// "ok", or why the key can't be watched here.
    keys: String,
    /// The provider the clean-up would use ("xai"…), if any.
    cleanup_provider: Option<String>,
}

#[tauri::command]
pub fn flow_status(app: AppHandle, flow: State<Arc<Flow>>) -> FlowStatus {
    FlowStatus {
        prefs: flow.store.lock().unwrap().prefs.clone(),
        model_ready: stt::is_ready(),
        keys: match os::keys_supported() {
            Ok(()) => "ok".into(),
            Err(why) => why.into(),
        },
        cleanup_provider: cleanup::endpoint(&app).map(|ep| ep.provider.info().name.to_string()),
    }
}

#[tauri::command]
pub fn flow_set_prefs(app: AppHandle, flow: State<Arc<Flow>>, prefs: Prefs) {
    let flow = flow.inner().clone();
    let before = {
        let mut store = flow.store.lock().unwrap();
        let before = store.prefs.clone();
        store.prefs = prefs.clone();
        store.save_prefs();
        if before.keep_days != prefs.keep_days {
            store.prune(store::now_ms());
        }
        before
    };
    pill::update(&app, |s| s.key = key_id(prefs.key));
    let restart = before.key != prefs.key;
    if before.enabled && (!prefs.enabled || restart) {
        disable(&flow);
    }
    if prefs.enabled && (!before.enabled || restart) {
        enable(&app, &flow);
    }
}

#[tauri::command]
pub fn flow_history(flow: State<Arc<Flow>>) -> Vec<Entry> {
    flow.store.lock().unwrap().history.clone()
}

#[tauri::command]
pub fn flow_delete(flow: State<Arc<Flow>>, id: String) {
    flow.store.lock().unwrap().delete(&id);
}

#[tauri::command]
pub fn flow_clear(flow: State<Arc<Flow>>, everything: bool) {
    let mut store = flow.store.lock().unwrap();
    if everything {
        store.clear_everything();
    } else {
        store.clear_history();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    words: u64,
    seconds: f64,
    dictations: u64,
    words_per_minute: u64,
    minutes_saved: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsView {
    week: Totals,
    month: Totals,
    all: Totals,
    /// The words of the last 30 days, oldest first: ("2026-10-14", words).
    daily: Vec<(String, u64)>,
    apps: std::collections::BTreeMap<String, u64>,
    streak: u32,
}

#[tauri::command]
pub fn flow_stats(flow: State<Arc<Flow>>) -> StatsView {
    let store = flow.store.lock().unwrap();
    let today = store::today();
    let totals = |from: Option<text::Day>| {
        let (words, seconds, dictations) = store.totals(from);
        Totals {
            words,
            seconds,
            dictations,
            words_per_minute: text::words_per_minute(words, seconds),
            minutes_saved: text::minutes_saved(words, seconds),
        }
    };
    let daily = (0..30)
        .rev()
        .map(|back| {
            let key = today.minus(back).key();
            let words = store.stats.days.get(&key).map(|d| d.words).unwrap_or(0);
            (key, words)
        })
        .collect();
    StatsView {
        week: totals(Some(today.minus(6))),
        month: totals(Some(today.minus(29))),
        all: totals(None),
        daily,
        apps: store.stats.apps.clone(),
        streak: store.streak(today),
    }
}

#[tauri::command]
pub fn flow_words(flow: State<Arc<Flow>>) -> Words {
    flow.store.lock().unwrap().words.clone()
}

#[tauri::command]
pub fn flow_set_words(flow: State<Arc<Flow>>, words: Words) {
    let mut store = flow.store.lock().unwrap();
    store.words = words;
    store.save_words();
}

#[tauri::command]
pub fn flow_snippets(flow: State<Arc<Flow>>) -> Vec<Snippet> {
    flow.store.lock().unwrap().snippets.clone()
}

#[tauri::command]
pub fn flow_set_snippets(flow: State<Arc<Flow>>, snippets: Vec<Snippet>) {
    let mut store = flow.store.lock().unwrap();
    store.snippets = snippets;
    store.save_snippets();
}

#[tauri::command]
pub fn flow_styles(flow: State<Arc<Flow>>) -> Vec<AppStyle> {
    flow.store.lock().unwrap().app_styles.clone()
}

#[tauri::command]
pub fn flow_set_styles(flow: State<Arc<Flow>>, styles: Vec<AppStyle>) {
    let mut store = flow.store.lock().unwrap();
    store.app_styles = styles;
    store.save_styles();
}

#[derive(Serialize, Deserialize)]
pub struct RunningApp {
    id: String,
    name: String,
}

#[tauri::command]
pub async fn flow_running_apps() -> Vec<RunningApp> {
    tauri::async_runtime::spawn_blocking(|| os::running_apps().into_iter().map(|(id, name)| RunningApp { id, name }).collect())
        .await
        .unwrap_or_default()
}

#[tauri::command]
pub fn flow_copy(text: String) -> bool {
    paste::copy(&text)
}

// ── The pill's page ───────────────────────────────────────────────────────────

#[tauri::command]
pub fn pill_state(app: AppHandle) -> Option<pill::PillState> {
    pill::get(&app).map(|p| p.state())
}

#[tauri::command]
pub fn pill_hot(app: AppHandle, rects: Vec<(f64, f64, f64, f64)>) {
    pill::set_hot(&app, rects);
}

#[tauri::command]
pub fn pill_hover(app: AppHandle, inside: bool) {
    if let Some(p) = pill::get(&app) {
        p.hovered.store(inside, std::sync::atomic::Ordering::Relaxed);
    }
}

#[tauri::command]
pub fn pill_action(app: AppHandle, flow: State<Arc<Flow>>, action: String, id: Option<u64>) {
    match action.as_str() {
        "finish" => flow.send(KeyEvent::Finish),
        "cancel" => flow.send(KeyEvent::Cancel),
        "stopNote" => {
            std::thread::spawn(move || crate::notes::stop(&app));
        }
        "announcement" | "dismiss" => {
            let current = pill::get(&app).and_then(|p| p.state().announcement).filter(|a| Some(a.id) == id);
            if let Some(a) = current {
                pill::dismiss(&app, a.id);
                if action == "announcement" {
                    match a.action_id.as_deref() {
                        Some("openNotes") => crate::show_settings_page(&app, "notes"),
                        Some("openVoice") => crate::show_settings_page(&app, "voice"),
                        Some("openSettings") => crate::show_settings_window(&app),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_hears_a_low_voice_on_the_left_and_a_whistle_on_the_right() {
        let tone = |hz: f64| (0..FFT).map(|i| (0.3 * (2.0 * std::f64::consts::PI * hz * i as f64 / 16_000.0).sin()) as f32).collect::<Vec<_>>();
        let mut m = Meter::default();
        m.feed(&tone(150.0));
        assert!(m.levels[0] > m.levels[8], "{:?}", m.levels);
        let mut m = Meter::default();
        m.feed(&tone(5000.0));
        assert!(m.levels[8] > m.levels[0], "{:?}", m.levels);
        let mut m = Meter::default();
        m.feed(&vec![0.0; FFT]);
        assert!(m.levels.iter().all(|&l| l == 0.0));
    }

    #[test]
    fn programs_get_their_usual_names() {
        assert_eq!(display_name("outlook"), "Outlook");
        assert_eq!(display_name("notepad"), "Notepad");
        assert!(is_shell("explorer"));
        assert!(!is_shell("slack"));
    }
}
