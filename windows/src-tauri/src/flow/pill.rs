// The pill of Flow dictation: a small capsule at the bottom of the screen
// while you dictate, with ARIA and nine dots that follow your voice, and short
// messages ("copied", "no microphone"). From Coucou's flow/pill.rs.
//
// Its window is borderless, transparent, always on top, never takes the
// keyboard, and lets clicks through except where the pill and its bubble are
// (a cursor poll, like the island's). Unlike Coucou's it does not rest on the
// screen between dictations: hidden, it is parked off-screen and its poll
// sleeps, so nothing runs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::platform;

pub const LABEL: &str = "pill";

/// Window size (logical).
const SIZE: (f64, f64) = (520.0, 150.0);
/// Off-screen spot while hidden: the webview keeps running, ready.
const PARKED: (i32, i32) = (-32000, -32000);

/// What the pill shows, sent whole to the page on every change.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PillState {
    /// "idle", "listening", "thinking", "locked", "copied", "failed", "pastedAgain".
    pub phase: String,
    /// The dictation key, as the page writes it ("right_ctrl"…).
    pub key: String,
    pub announcement: Option<Announcement>,
    /// ARIA is taking a call's notes since then (ms since 1970).
    pub note_started_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Announcement {
    pub id: u64,
    /// "mic", "clipboard", "download".
    pub icon: String,
    pub tint: String,
    pub text: String,
    /// The button's label, if there is one.
    pub action: Option<String>,
    /// What the button does: "openSettings", "openVoice", "openNotes".
    pub action_id: Option<String>,
}

pub struct Pill {
    state: Mutex<PillState>,
    /// Where the page says the pill and its bubble are (window-logical rects).
    hot: Mutex<Vec<(f64, f64, f64, f64)>>,
    shown: AtomicBool,
    /// The pointer is on the pill or its bubble: a message waits for it to leave.
    pub hovered: AtomicBool,
    /// Parks the cursor poll while the pill is hidden.
    poll: (Mutex<bool>, Condvar),
    announce_seq: AtomicU64,
}

impl Pill {
    pub fn state(&self) -> PillState {
        self.state.lock().unwrap().clone()
    }
}

fn page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/pill.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("pill.html".into())
}

/// Creates the pill's window, parked. Before the island, for WebView2's sake
/// (see create_settings_window in lib.rs).
pub fn setup(app: &AppHandle) {
    if cfg!(windows) {
        let built = WebviewWindowBuilder::new(app, LABEL, page_url(app))
            .additional_browser_args(crate::BROWSER_ARGS)
            .title("ARIA")
            .inner_size(SIZE.0, SIZE.1)
            .resizable(false)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .maximizable(false)
            .minimizable(false)
            .closable(false)
            .disable_drag_drop_handler()
            .focusable(false)
            .visible(true)
            .position(PARKED.0 as f64, PARKED.1 as f64)
            .build();
        match built {
            Ok(win) => {
                platform::make_non_activating(&win);
                let _ = win.set_ignore_cursor_events(true);
            }
            Err(err) => crate::log::line(format!("flow pill window failed: {err}")),
        }
    }
    let pill = Arc::new(Pill {
        state: Mutex::new(PillState { phase: "idle".into(), ..Default::default() }),
        hot: Mutex::new(Vec::new()),
        shown: AtomicBool::new(false),
        hovered: AtomicBool::new(false),
        poll: (Mutex::new(false), Condvar::new()),
        announce_seq: AtomicU64::new(1),
    });
    app.manage(pill.clone());
    if window(app).is_some() {
        spawn_poll(app.clone(), pill);
    }
}

/// A dictation is under way: the microphone is Flow's.
pub fn dictating(app: &AppHandle) -> bool {
    get(app).is_some_and(|p| matches!(p.state.lock().unwrap().phase.as_str(), "listening" | "locked" | "thinking"))
}

pub fn get(app: &AppHandle) -> Option<Arc<Pill>> {
    app.try_state::<Arc<Pill>>().map(|s| s.inner().clone())
}

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Changes what the pill shows, and shows or hides it.
pub fn update(app: &AppHandle, change: impl FnOnce(&mut PillState)) {
    let Some(pill) = get(app) else { return };
    let state = {
        let mut s = pill.state.lock().unwrap();
        change(&mut s);
        s.clone()
    };
    let _ = app.emit_to(LABEL, "pill-state", &state);
    let visible = state.announcement.is_some() || state.phase != "idle" || state.note_started_at.is_some();
    let was = pill.shown.swap(visible, Ordering::Relaxed);
    if visible && !was {
        place(app);
    }
    if visible != was {
        show(app, &pill, visible);
    }
}

fn show(app: &AppHandle, pill: &Pill, visible: bool) {
    let Some(win) = window(app) else { return };
    if !visible {
        let _ = win.set_ignore_cursor_events(true);
        let _ = win.set_position(PhysicalPosition::new(PARKED.0, PARKED.1));
    }
    let (lock, cv) = &pill.poll;
    *lock.lock().unwrap() = visible;
    cv.notify_all();
    let _ = app.emit_to(LABEL, "pill-visible", visible);
}

/// The display under the cursor, where you are working; else the primary one.
fn monitor(app: &AppHandle) -> Option<tauri::Monitor> {
    if let Some((cx, cy)) = platform::cursor_physical() {
        if let Ok(list) = app.available_monitors() {
            if let Some(m) = list.into_iter().find(|m| {
                let (p, s) = (m.position(), m.size());
                cx >= p.x as f64 && cx < (p.x + s.width as i32) as f64 && cy >= p.y as f64 && cy < (p.y + s.height as i32) as f64
            }) {
                return Some(m);
            }
        }
    }
    app.primary_monitor().ok().flatten()
}

/// Bottom centre of the work area, just above the taskbar.
fn place(app: &AppHandle) {
    let Some(win) = window(app) else { return };
    let Some(m) = monitor(app) else { return };
    let scale = m.scale_factor();
    let wa = m.work_area();
    let (pw, ph) = ((SIZE.0 * scale).round() as i32, (SIZE.1 * scale).round() as i32);
    let gap = (6.0 * scale).round() as i32;
    let x = wa.position.x + (wa.size.width as i32 - pw) / 2;
    let y = wa.position.y + wa.size.height as i32 - ph - gap;
    let _ = win.set_size(PhysicalSize::new(pw as u32, ph as u32));
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// The page tells where the pill and its bubble are, in window-logical pixels.
pub fn set_hot(app: &AppHandle, rects: Vec<(f64, f64, f64, f64)>) {
    if let Some(pill) = get(app) {
        *pill.hot.lock().unwrap() = rects;
    }
}

/// ~30 times a second while the pill shows, the window takes the mouse only
/// over the pill and its bubble. Asleep on its condvar while hidden.
fn spawn_poll(app: AppHandle, pill: Arc<Pill>) {
    std::thread::Builder::new()
        .name("aria-pill-poll".into())
        .spawn(move || {
            let mut ignoring: Option<bool> = None;
            loop {
                {
                    let (lock, cv) = &pill.poll;
                    let mut on = lock.lock().unwrap();
                    while !*on {
                        ignoring = None;
                        on = cv.wait(on).unwrap();
                    }
                }
                std::thread::sleep(Duration::from_millis(33));
                let Some(win) = window(&app) else { continue };
                let (Ok(origin), Some((cx, cy))) = (win.outer_position(), platform::cursor_physical()) else { continue };
                let scale = win.scale_factor().unwrap_or(1.0);
                let (x, y) = ((cx - origin.x as f64) / scale, (cy - origin.y as f64) / scale);
                let over = pill.hot.lock().unwrap().iter().any(|&(rx, ry, rw, rh)| x >= rx && x <= rx + rw && y >= ry && y <= ry + rh);
                if ignoring != Some(!over) {
                    ignoring = Some(!over);
                    let _ = win.set_ignore_cursor_events(!over);
                    let _ = win.emit("pill-pointer", over);
                }
            }
        })
        .ok();
}

/// A message next to the pill, with a button to act; it goes away by itself
/// after a while unless the pointer is on it.
pub fn announce(app: &AppHandle, icon: &str, tint: &str, text: String, action: Option<(String, &str)>, seconds: u64) {
    let Some(pill) = get(app) else { return };
    let id = pill.announce_seq.fetch_add(1, Ordering::Relaxed);
    let (label, action_id) = match action {
        Some((label, id)) => (Some(label), Some(id.to_string())),
        None => (None, None),
    };
    let a = Announcement { id, icon: icon.into(), tint: tint.into(), text, action: label, action_id };
    update(app, |s| s.announcement = Some(a));
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        while get(&app).is_some_and(|p| p.hovered.load(Ordering::Relaxed)) {
            std::thread::sleep(Duration::from_secs(1));
        }
        dismiss(&app, id);
    });
}

/// Takes a message away (its ✕, its button, or its time ran out).
pub fn dismiss(app: &AppHandle, id: u64) {
    update(app, |s| {
        if s.announcement.as_ref().is_some_and(|a| a.id == id) {
            s.announcement = None;
        }
    });
}

/// Back to rest after a short message.
pub fn rest_after(app: &AppHandle, phase: &str, ms: u64) {
    let app = app.clone();
    let phase = phase.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        update(&app, |s| {
            if s.phase == phase {
                s.phase = "idle".into();
            }
        });
    });
}
