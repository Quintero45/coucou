//! `capture_context`: what the owner is looking at, for a message to a Grok
//! Bot — the foreground window's title and process, the clipboard text and a
//! PNG of that window, registered in the inbox like `ingest_files` so the
//! island can send it as an attachment `{id}` through `grokbot_send`.
//!
//! "Foreground" means the owner's window, never ours: the island does not take
//! activation, but the settings window or a focused text box can, so the poll
//! thread remembers the last foreign foreground window (`remember_foreground`).
//!
//! The selected text is not read yet (UI Automation, as cursorlink.rs uses it).
//! The screenshot uses hand-declared gdi32/user32/dwmapi calls (no
//! Win32_Graphics_Gdi feature), and a small PNG encoder of our own (no image
//! crate).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::files::IngestedFile;
use crate::island::WINDOW_LABEL;

/// Longest clipboard text returned, in characters.
const CLIPBOARD_MAX: usize = 4_000;
const TITLE_MAX: usize = 300;
/// Screenshots wider than this are scaled down (box filter).
const SHOT_MAX_W: usize = 1280;
const SHOT_MAX_H: usize = 1600;

#[derive(Serialize, Clone, Debug, Default)]
pub struct ContextCapture {
    pub window_title: String,
    pub process_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<IngestedFile>,
}

#[tauri::command]
pub async fn capture_context() -> Result<ContextCapture, String> {
    tauri::async_runtime::spawn_blocking(capture).await.map_err(|e| e.to_string())?
}

fn capture() -> Result<ContextCapture, String> {
    let out = imp::capture()?;
    crate::log::line(format!(
        "capture_context window=\"{}\" process={} clipboard={} chars screenshot={}",
        out.window_title.chars().take(80).collect::<String>(),
        out.process_name,
        out.clipboard_text.as_ref().map(|t| t.chars().count()).unwrap_or(0),
        out.screenshot.as_ref().map(|s| format!("{} ({} bytes)", s.id, s.size)).unwrap_or_else(|| "none".into()),
    ));
    Ok(out)
}

/// Poll thread, about twice a second: remembers the owner's foreground window
/// so a capture taken while one of ours has focus still finds it.
pub fn remember_foreground() {
    imp::remember_foreground();
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Clipboard text as returned: redacted, NULs and other controls but line
/// breaks and tabs dropped, capped.
fn clean_clipboard(raw: &str) -> Option<String> {
    let redacted = crate::policy::redact(raw);
    let kept: String = redacted
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let t = kept.trim();
    if t.is_empty() {
        None
    } else {
        Some(cap_chars(t, CLIPBOARD_MAX))
    }
}

/// The owner's screen right now (the monitor of their foreground window), saved
/// in the inbox: an order to Cursor carries its path and Cursor opens it.
/// ARIA's own windows (the island with its conversations) are left out, as
/// in a screen share. Call it off the main thread.
pub fn screen_png_file(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let sharing = SHARE.lock().unwrap().is_some();
    if !sharing {
        exclude_ours_now(app, true);
    }
    let frame = imp::screen_frame();
    // A share started meanwhile keeps its exclusion.
    if !sharing && SHARE.lock().unwrap().is_none() {
        exclude_ours(app, false);
    }
    let (bgra, w, h, _) = frame.ok_or("No pude capturar la pantalla.")?;
    let (rgb, tw, th) = bgra_to_rgb_fit(&bgra, w, h, SHOT_MAX_W, SHOT_MAX_H);
    crate::files::save_to_inbox("pantalla.png", &encode_png(&rgb, tw, th))
}

/// Saves the PNG in the inbox: same ids and sweep as dropped files.
fn register_png(png: &[u8]) -> Result<IngestedFile, String> {
    let path = crate::files::save_to_inbox("captura.png", png)?;
    let id = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(IngestedFile {
        name: crate::files::display_name(&id),
        id,
        mime: "image/png".into(),
        size: png.len() as u64,
    })
}

// ---------------------------------------------------------------------------
// Pixels → PNG

/// Box-filter downscale of a top-down BGRA image to fit `max_w`×`max_h`.
/// Returns RGB rows and the new size.
pub fn bgra_to_rgb_fit(bgra: &[u8], w: usize, h: usize, max_w: usize, max_h: usize) -> (Vec<u8>, usize, usize) {
    let scale = f64::max(w as f64 / max_w as f64, h as f64 / max_h as f64).max(1.0);
    let tw = ((w as f64 / scale).round() as usize).clamp(1, w.max(1));
    let th = ((h as f64 / scale).round() as usize).clamp(1, h.max(1));
    let mut out = Vec::with_capacity(tw * th * 3);
    for ty in 0..th {
        let y0 = ty * h / th;
        let y1 = (((ty + 1) * h) / th).max(y0 + 1).min(h);
        for tx in 0..tw {
            let x0 = tx * w / tw;
            let x1 = (((tx + 1) * w) / tw).max(x0 + 1).min(w);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                let row = &bgra[(y * w + x0) * 4..(y * w + x1) * 4];
                for px in row.chunks_exact(4) {
                    b += px[0] as u32;
                    g += px[1] as u32;
                    r += px[2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.extend_from_slice(&[((r + n / 2) / n) as u8, ((g + n / 2) / n) as u8, ((b + n / 2) / n) as u8]);
        }
    }
    (out, tw, th)
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code, most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(rev, len);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn fixed_literal(bits: &mut Bits, sym: u32) {
    match sym {
        0..=143 => bits.code(0x30 + sym, 8),
        144..=255 => bits.code(0x190 + sym - 144, 9),
        256..=279 => bits.code(sym - 256, 7),
        _ => bits.code(0xC0 + sym - 280, 8),
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];

/// One fixed-Huffman deflate block. The only matches are runs of the previous
/// byte (distance 1), which is what filtered flat screen areas turn into.
pub fn deflate_fixed(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: Vec::with_capacity(data.len() / 4 + 16), acc: 0, n: 0 };
    bits.put(1, 1); // BFINAL
    bits.put(1, 2); // BTYPE = fixed Huffman
    let mut i = 0;
    while i < data.len() {
        let mut run = 0;
        if i > 0 {
            let prev = data[i - 1];
            while run < 258 && i + run < data.len() && data[i + run] == prev {
                run += 1;
            }
        }
        if run >= 3 {
            let idx = LEN_BASE.iter().rposition(|&b| b as usize <= run).unwrap();
            fixed_literal(&mut bits, 257 + idx as u32);
            if LEN_EXTRA[idx] > 0 {
                bits.put((run - LEN_BASE[idx] as usize) as u32, LEN_EXTRA[idx] as u32);
            }
            bits.code(0, 5); // distance code 0 = 1
            i += run;
        } else {
            fixed_literal(&mut bits, data[i] as u32);
            i += 1;
        }
    }
    fixed_literal(&mut bits, 256);
    bits.finish()
}

pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend(deflate_fixed(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Filtered scanlines: per row, None, Sub or Up — whichever has the smallest
/// sum of absolute values (the usual PNG heuristic).
pub fn filter_rows(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let stride = w * 3;
    let mut out = Vec::with_capacity((stride + 1) * h);
    let mut cand = [vec![0u8; stride], vec![0u8; stride], vec![0u8; stride]];
    for y in 0..h {
        let row = &rgb[y * stride..(y + 1) * stride];
        for x in 0..stride {
            let left = if x >= 3 { row[x - 3] } else { 0 };
            let up = if y > 0 { rgb[(y - 1) * stride + x] } else { 0 };
            cand[0][x] = row[x];
            cand[1][x] = row[x].wrapping_sub(left);
            cand[2][x] = row[x].wrapping_sub(up);
        }
        let score = |v: &Vec<u8>| v.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum::<u64>();
        let best = (0..3).min_by_key(|&k| score(&cand[k])).unwrap();
        out.push(best as u8); // 0 None, 1 Sub, 2 Up
        out.extend_from_slice(&cand[best]);
    }
    out
}

pub fn encode_png(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&filter_rows(rgb, w, h)));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A capture that is entirely black is what PrintWindow returns for windows it
/// cannot render; worth a second try from the screen.
pub fn all_black(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
}

// ---------------------------------------------------------------------------
// Screen sharing: the owner's screen to one Grok Bot, a frame every few
// seconds, only when it changed. The island is left out of the capture
// (WDA_EXCLUDEFROMCAPTURE while sharing).

pub const SHARE_DEFAULT_S: u32 = 5;
pub const SHARE_MIN_S: u32 = 2;
pub const SHARE_MAX_S: u32 = 60;
/// A share ends by itself after this long.
pub const SHARE_MAX_DURATION: Duration = Duration::from_secs(30 * 60);
/// This many `not_connected` answers in a row end the share.
pub const SHARE_MAX_NOT_CONNECTED: u32 = 3;
/// A frame is sent when more than this share of the thumbnail changed.
pub const SHARE_CHANGE_THRESHOLD: f64 = 0.02;
/// Thumbnail used for change detection (grayscale).
const THUMB_W: usize = 80;
const THUMB_H: usize = 45;
/// Gray levels a thumbnail pixel must move to count as changed.
const THUMB_DELTA: u8 = 12;
/// Frame widths tried until the PNG fits one inline image (IMAGE_EACH as base64).
const SHARE_WIDTHS: [usize; 3] = [1280, 960, 640];

pub fn clamp_interval(interval_s: Option<u32>) -> u32 {
    interval_s.unwrap_or(SHARE_DEFAULT_S).clamp(SHARE_MIN_S, SHARE_MAX_S)
}

pub fn share_expired(elapsed: Duration) -> bool {
    elapsed >= SHARE_MAX_DURATION
}

/// Grayscale box-filtered thumbnail of an RGB image.
pub fn gray_thumb(rgb: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(THUMB_W * THUMB_H);
    for ty in 0..THUMB_H {
        let y0 = ty * h / THUMB_H;
        let y1 = ((ty + 1) * h / THUMB_H).max(y0 + 1).min(h.max(1));
        for tx in 0..THUMB_W {
            let x0 = tx * w / THUMB_W;
            let x1 = ((tx + 1) * w / THUMB_W).max(x0 + 1).min(w.max(1));
            let (mut sum, mut n) = (0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 3;
                    if i + 2 < rgb.len() {
                        // ITU-R BT.601 luma, integer.
                        sum += (299 * rgb[i] as u64 + 587 * rgb[i + 1] as u64 + 114 * rgb[i + 2] as u64) / 1000;
                        n += 1;
                    }
                }
            }
            out.push(if n == 0 { 0 } else { (sum / n) as u8 });
        }
    }
    out
}

/// Share of thumbnail pixels that moved more than `THUMB_DELTA` levels.
pub fn changed_fraction(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }
    let changed = a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > THUMB_DELTA).count();
    changed as f64 / a.len() as f64
}

/// Whether to send this frame: always the first one, then only on change.
pub fn frame_worth_sending(last_sent: Option<&[u8]>, now: &[u8]) -> bool {
    match last_sent {
        None => true,
        Some(prev) => changed_fraction(prev, now) > SHARE_CHANGE_THRESHOLD,
    }
}

/// Counts `not_connected` answers in a row; true when the share must end.
#[derive(Default, Debug)]
pub struct SendErrors {
    not_connected: u32,
}

impl SendErrors {
    pub fn record(&mut self, result: &Result<(), String>) -> bool {
        match result {
            Ok(()) => self.not_connected = 0,
            Err(e) if e == "not_connected" => self.not_connected += 1,
            Err(_) => {}
        }
        self.not_connected >= SHARE_MAX_NOT_CONNECTED
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ScreenShareEvent {
    pub active: bool,
    pub bot: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
}

struct Share {
    generation: u64,
    bot: String,
    since: u64,
    stop: Arc<AtomicBool>,
}

static SHARE: LazyLock<Mutex<Option<Share>>> = LazyLock::new(|| Mutex::new(None));
static GENERATION: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The island (and the settings window) out of captures while sharing.
#[cfg(not(windows))]
fn exclude_ours(_app: &AppHandle, _on: bool) {}

/// `exclude_ours`, waiting (briefly) until it is applied, for a single capture.
#[cfg(not(windows))]
fn exclude_ours_now(_app: &AppHandle, _on: bool) {}

#[cfg(windows)]
fn exclude_ours_now(app: &AppHandle, on: bool) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        for (_, win) in handle.webview_windows() {
            if let Ok(hwnd) = win.hwnd() {
                imp::exclude_from_capture(hwnd.0 as isize, on);
            }
        }
        let _ = tx.send(());
    });
    if queued.is_ok() && rx.recv_timeout(Duration::from_millis(500)).is_ok() {
        // DWM applies the affinity on its next frame.
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn exclude_ours(app: &AppHandle, on: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        for (label, win) in handle.webview_windows() {
            let Ok(hwnd) = win.hwnd() else { continue };
            let ok = imp::exclude_from_capture(hwnd.0 as isize, on);
            if on && !ok {
                crate::log::line(format!("screen-share: {label} could not be excluded from capture (it may appear in frames)"));
            }
        }
    });
}

/// Ends the share `generation` (if it is still the current one).
fn end_share(app: &AppHandle, generation: Option<u64>, reason: &'static str) -> bool {
    let share = {
        let mut slot = SHARE.lock().unwrap();
        match slot.as_ref() {
            Some(s) if generation.is_none_or(|g| g == s.generation) => slot.take(),
            _ => None,
        }
    };
    let Some(share) = share else { return false };
    share.stop.store(true, Ordering::Relaxed);
    crate::log::line(format!("screen-share stop reason={reason} bot={}", share.bot));
    if reason != "replaced" && reason != "quit" {
        exclude_ours(app, false);
    }
    let _ = app.emit_to(
        WINDOW_LABEL,
        "screen-share",
        ScreenShareEvent { active: false, bot: share.bot, since: Some(share.since), reason: Some(reason) },
    );
    true
}

#[tauri::command]
pub fn start_screen_share(app: AppHandle, bot: String, interval_s: Option<u32>) -> Result<ScreenShareEvent, String> {
    let wanted = bot.trim();
    let slug = crate::grokbot::slug(wanted);
    let found = crate::grokbot::list(&app)
        .into_iter()
        .find(|b| b.id == wanted || b.id == slug || b.name.eq_ignore_ascii_case(wanted))
        .ok_or("not_connected")?;
    if !cfg!(windows) {
        return Err("screen sharing is only available on Windows".into());
    }
    let interval = clamp_interval(interval_s);
    end_share(&app, None, "replaced");
    let share = Share {
        generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        bot: found.name.clone(),
        since: now_ms(),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let event = ScreenShareEvent { active: true, bot: share.bot.clone(), since: Some(share.since), reason: None };
    let (generation, stop) = (share.generation, share.stop.clone());
    *SHARE.lock().unwrap() = Some(share);
    exclude_ours(&app, true);
    crate::log::line(format!("screen-share start bot={} interval={interval}s", found.name));
    let _ = app.emit_to(WINDOW_LABEL, "screen-share", event.clone());
    let handle = app.clone();
    std::thread::Builder::new()
        .name("screen-share".into())
        .spawn(move || share_loop(handle, generation, found.id, found.name, interval, stop))
        .map_err(|e| e.to_string())?;
    Ok(event)
}

#[tauri::command]
pub fn stop_screen_share(app: AppHandle) -> bool {
    end_share(&app, None, "user")
}

/// App quitting (RunEvent::Exit through `exit_plugin`): end the share.
pub fn stop_screen_share_on_quit(app: &AppHandle) {
    end_share(app, None, "quit");
}

/// Registers the quit hook without touching lib.rs' `.run(…)`:
/// `.plugin(context::exit_plugin())` on the builder.
pub fn exit_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("aria-screen-share")
        .on_event(|app, event| {
            if let tauri::RunEvent::Exit = event {
                stop_screen_share_on_quit(app);
            }
        })
        .build()
}

/// One frame ready to send: PNG within the inline image limit.
fn encode_frame(bgra: &[u8], w: usize, h: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let limit = crate::grokbot::IMAGE_EACH / 4 * 3;
    let mut thumb = None;
    for max_w in SHARE_WIDTHS {
        let (rgb, tw, th) = bgra_to_rgb_fit(bgra, w, h, max_w, SHOT_MAX_H);
        let t = thumb.get_or_insert_with(|| gray_thumb(&rgb, tw, th)).clone();
        let png = encode_png(&rgb, tw, th);
        if png.len() <= limit {
            return Some((png, t));
        }
    }
    None
}

fn share_loop(app: AppHandle, generation: u64, bot_id: String, bot_name: String, interval: u32, stop: Arc<AtomicBool>) {
    let started = std::time::Instant::now();
    let in_flight = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(Mutex::new(SendErrors::default()));
    let mut last_sent: Option<Vec<u8>> = None;
    let mut next = std::time::Instant::now();
    loop {
        // Short naps so a stop is honoured within a fraction of a second.
        while std::time::Instant::now() < next {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        next = std::time::Instant::now() + Duration::from_secs(interval as u64);
        if share_expired(started.elapsed()) {
            end_share(&app, Some(generation), "timeout");
            return;
        }
        if in_flight.load(Ordering::Relaxed) {
            crate::log::line("screen-share frame skipped (sending)".to_string());
            continue;
        }
        let Some((bgra, w, h, title)) = imp::screen_frame() else {
            crate::log::line("screen-share frame skipped (capture failed)".to_string());
            continue;
        };
        let Some((png, thumb)) = encode_frame(&bgra, w, h) else {
            crate::log::line("screen-share frame skipped (too large)".to_string());
            continue;
        };
        if !frame_worth_sending(last_sent.as_deref(), &thumb) {
            crate::log::line("screen-share frame skipped (unchanged)".to_string());
            continue;
        }
        last_sent = Some(thumb);
        let t = crate::platform::local_time();
        let title = cap_chars(&crate::policy::redact(&title), 120);
        let message = format!("[Pantalla compartida {:02}:{:02}:{:02}] ventana: {title}", t.hour, t.minute, t.second);
        let size = png.len();
        let attachment = crate::grokbot::AttachmentIn::Base64 {
            name: format!("pantalla-{:02}{:02}{:02}.png", t.hour, t.minute, t.second),
            mime: "image/png".into(),
            base64: crate::claude::base64_for(&png),
        };
        in_flight.store(true, Ordering::Relaxed);
        let (app2, flight, errs, stop2, bot) = (app.clone(), in_flight.clone(), errors.clone(), stop.clone(), bot_id.clone());
        let name = bot_name.clone();
        tauri::async_runtime::spawn(async move {
            let result = crate::grokbot::send(&app2, &bot, &message, vec![attachment]).await.map(|_| ());
            flight.store(false, Ordering::Relaxed);
            match &result {
                Ok(()) => crate::log::line(format!("screen-share frame sent bot={name} ({size} bytes png)")),
                Err(e) => crate::log::line(format!("screen-share frame failed bot={name}: {e}")),
            }
            let give_up = errs.lock().unwrap().record(&result);
            if give_up && !stop2.load(Ordering::Relaxed) {
                end_share(&app2, Some(generation), "error");
            }
        });
    }
}

// ---------------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use super::*;

    #[allow(non_snake_case)]
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[allow(non_snake_case)]
    #[repr(C)]
    struct BitmapInfoHeader {
        biSize: u32,
        biWidth: i32,
        biHeight: i32,
        biPlanes: u16,
        biBitCount: u16,
        biCompression: u32,
        biSizeImage: u32,
        biXPelsPerMeter: i32,
        biYPelsPerMeter: i32,
        biClrUsed: u32,
        biClrImportant: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        header: BitmapInfoHeader,
        colors: [u32; 1],
    }

    type Hwnd = isize;
    type Handle = isize;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetForegroundWindow() -> Hwnd;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn GetWindowTextW(hwnd: Hwnd, buf: *mut u16, max: i32) -> i32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn IsWindow(hwnd: Hwnd) -> i32;
        fn IsWindowVisible(hwnd: Hwnd) -> i32;
        fn IsIconic(hwnd: Hwnd) -> i32;
        fn GetDC(hwnd: Hwnd) -> Handle;
        fn ReleaseDC(hwnd: Hwnd, dc: Handle) -> i32;
        fn PrintWindow(hwnd: Hwnd, dc: Handle, flags: u32) -> i32;
        fn OpenClipboard(owner: Hwnd) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> Handle;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: Handle) -> Handle;
        fn CreateCompatibleBitmap(dc: Handle, w: i32, h: i32) -> Handle;
        fn SelectObject(dc: Handle, obj: Handle) -> Handle;
        fn DeleteObject(obj: Handle) -> i32;
        fn DeleteDC(dc: Handle) -> i32;
        fn BitBlt(dst: Handle, x: i32, y: i32, w: i32, h: i32, src: Handle, sx: i32, sy: i32, rop: u32) -> i32;
        fn GetDIBits(dc: Handle, bmp: Handle, start: u32, lines: u32, bits: *mut c_void, info: *mut BitmapInfo, usage: u32) -> i32;
    }

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmGetWindowAttribute(hwnd: Hwnd, attr: u32, value: *mut c_void, size: u32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(h: Handle) -> i32;
        fn QueryFullProcessImageNameW(h: Handle, flags: u32, buf: *mut u16, len: *mut u32) -> i32;
        fn GlobalLock(h: Handle) -> *mut c_void;
        fn GlobalUnlock(h: Handle) -> i32;
        fn GlobalSize(h: Handle) -> usize;
    }

    const PW_RENDERFULLCONTENT: u32 = 2;
    const SRCCOPY: u32 = 0x00CC_0020;
    const CAPTUREBLT: u32 = 0x4000_0000;
    const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const CF_UNICODETEXT: u32 = 13;
    /// Larger windows are not captured (a runaway rect would mean gigabytes).
    const MAX_SIDE: i32 = 8192;

    static LAST_FOREIGN: AtomicIsize = AtomicIsize::new(0);

    fn is_ours(hwnd: Hwnd) -> bool {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid == unsafe { GetCurrentProcessId() }
    }

    pub fn remember_foreground() {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd != 0 && !is_ours(hwnd) {
            LAST_FOREIGN.store(hwnd, Ordering::Relaxed);
        }
    }

    fn target() -> Option<Hwnd> {
        let fg = unsafe { GetForegroundWindow() };
        if fg != 0 && !is_ours(fg) {
            LAST_FOREIGN.store(fg, Ordering::Relaxed);
            return Some(fg);
        }
        let last = LAST_FOREIGN.load(Ordering::Relaxed);
        (last != 0 && unsafe { IsWindow(last) } != 0).then_some(last)
    }

    fn title(hwnd: Hwnd) -> String {
        let mut buf = [0u16; 512];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) }.max(0) as usize;
        cap_chars(&String::from_utf16_lossy(&buf[..n]), TITLE_MAX)
    }

    fn process_name(hwnd: Hwnd) -> String {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == 0 {
            return String::new();
        }
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h == 0 {
            return String::new();
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
    }

    fn clipboard() -> Option<String> {
        // Another app may hold the clipboard for a moment.
        let mut opened = false;
        for _ in 0..5 {
            if unsafe { OpenClipboard(0) } != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            return None;
        }
        let mut text = None;
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
                let h = GetClipboardData(CF_UNICODETEXT);
                if h != 0 {
                    let p = GlobalLock(h) as *const u16;
                    if !p.is_null() {
                        // Bounded by the allocation, and by what we keep anyway.
                        let max = (GlobalSize(h) / 2).min(CLIPBOARD_MAX * 4);
                        let slice = std::slice::from_raw_parts(p, max);
                        let end = slice.iter().position(|&c| c == 0).unwrap_or(max);
                        text = Some(String::from_utf16_lossy(&slice[..end]));
                        GlobalUnlock(h);
                    }
                }
            }
            CloseClipboard();
        }
        text.and_then(|t| clean_clipboard(&t))
    }

    /// Top-down BGRA of `bmp`, which must not be selected into a DC.
    fn read_bitmap(dc: Handle, bmp: Handle, w: i32, h: i32) -> Option<Vec<u8>> {
        let mut info = BitmapInfo {
            header: BitmapInfoHeader {
                biSize: std::mem::size_of::<BitmapInfoHeader>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            colors: [0],
        };
        let mut px = vec![0u8; w as usize * h as usize * 4];
        let lines = unsafe { GetDIBits(dc, bmp, 0, h as u32, px.as_mut_ptr() as *mut c_void, &mut info, 0) };
        (lines == h).then_some(px)
    }

    /// Captures `rect` (screen coordinates) of `hwnd`: PrintWindow first (the
    /// window alone, whatever covers it — the island included), the screen as
    /// a fallback for windows that render nothing that way.
    fn grab(hwnd: Hwnd, win: Rect, crop: Rect) -> Option<(Vec<u8>, usize, usize)> {
        let (ww, wh) = (win.right - win.left, win.bottom - win.top);
        let (cw, ch) = (crop.right - crop.left, crop.bottom - crop.top);
        if ww <= 0 || wh <= 0 || cw <= 0 || ch <= 0 || ww > MAX_SIDE || wh > MAX_SIDE {
            return None;
        }
        unsafe {
            let screen = GetDC(0);
            if screen == 0 {
                return None;
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, ww, wh);
            let mut result = None;
            if mem != 0 && bmp != 0 {
                let old = SelectObject(mem, bmp);
                let printed = PrintWindow(hwnd, mem, PW_RENDERFULLCONTENT) != 0;
                SelectObject(mem, old);
                let mut px = if printed { read_bitmap(mem, bmp, ww, wh) } else { None };
                let mut source = "PrintWindow";
                if px.as_deref().map(all_black).unwrap_or(true) {
                    let old = SelectObject(mem, bmp);
                    let ok = BitBlt(mem, 0, 0, ww, wh, screen, win.left, win.top, SRCCOPY | CAPTUREBLT) != 0;
                    SelectObject(mem, old);
                    px = if ok { read_bitmap(mem, bmp, ww, wh) } else { None };
                    source = "screen";
                }
                if let Some(px) = px {
                    // Crop away the invisible resize borders.
                    let (ox, oy) = ((crop.left - win.left).clamp(0, ww - 1), (crop.top - win.top).clamp(0, wh - 1));
                    let cw = cw.min(ww - ox) as usize;
                    let ch = ch.min(wh - oy) as usize;
                    let mut out = Vec::with_capacity(cw * ch * 4);
                    for y in 0..ch {
                        let start = ((oy as usize + y) * ww as usize + ox as usize) * 4;
                        out.extend_from_slice(&px[start..start + cw * 4]);
                    }
                    crate::log::line(format!("capture_context screenshot {cw}x{ch} via {source}"));
                    result = Some((out, cw, ch));
                }
            }
            if bmp != 0 {
                DeleteObject(bmp);
            }
            if mem != 0 {
                DeleteDC(mem);
            }
            ReleaseDC(0, screen);
            result
        }
    }

    fn screenshot(hwnd: Hwnd) -> Option<IngestedFile> {
        if unsafe { IsIconic(hwnd) } != 0 || unsafe { IsWindowVisible(hwnd) } == 0 {
            return None;
        }
        let mut win = Rect::default();
        if unsafe { GetWindowRect(hwnd, &mut win) } == 0 {
            return None;
        }
        let mut crop = Rect::default();
        let dwm = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut crop as *mut Rect as *mut c_void,
                std::mem::size_of::<Rect>() as u32,
            )
        };
        if dwm != 0 || crop.right <= crop.left || crop.bottom <= crop.top {
            crop = win;
        }
        let (bgra, w, h) = grab(hwnd, win, crop)?;
        let (rgb, tw, th) = bgra_to_rgb_fit(&bgra, w, h, SHOT_MAX_W, SHOT_MAX_H);
        let png = encode_png(&rgb, tw, th);
        match register_png(&png) {
            Ok(f) => Some(f),
            Err(e) => {
                crate::log::line(format!("capture_context: cannot save the screenshot: {e}"));
                None
            }
        }
    }

    #[repr(C)]
    struct MonitorInfo {
        cb_size: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MonitorFromWindow(hwnd: Hwnd, flags: u32) -> Handle;
        fn GetMonitorInfoW(monitor: Handle, info: *mut MonitorInfo) -> i32;
        fn SetWindowDisplayAffinity(hwnd: Hwnd, affinity: u32) -> i32;
    }

    const MONITOR_DEFAULTTOPRIMARY: u32 = 1;
    const WDA_NONE: u32 = 0;
    /// Windows 10 2004+: the window is left out of every capture (BitBlt of the
    /// screen included) but stays on screen.
    const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;

    /// The monitor with the owner's foreground window (the primary one when
    /// there is none), as top-down BGRA, plus that window's title.
    pub fn screen_frame() -> Option<(Vec<u8>, usize, usize, String)> {
        let fg = target();
        let monitor = unsafe { MonitorFromWindow(fg.unwrap_or(0), MONITOR_DEFAULTTOPRIMARY) };
        if monitor == 0 {
            return None;
        }
        let mut info = MonitorInfo { cb_size: std::mem::size_of::<MonitorInfo>() as u32, monitor: Rect::default(), work: Rect::default(), flags: 0 };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }
        let r = info.monitor;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 || w > MAX_SIDE || h > MAX_SIDE {
            return None;
        }
        let px = unsafe {
            let screen = GetDC(0);
            if screen == 0 {
                return None;
            }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let mut px = None;
            if mem != 0 && bmp != 0 {
                let old = SelectObject(mem, bmp);
                let ok = BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, SRCCOPY | CAPTUREBLT) != 0;
                SelectObject(mem, old);
                if ok {
                    px = read_bitmap(mem, bmp, w, h);
                }
            }
            if bmp != 0 {
                DeleteObject(bmp);
            }
            if mem != 0 {
                DeleteDC(mem);
            }
            ReleaseDC(0, screen);
            px
        }?;
        let title = fg.map(title).unwrap_or_default();
        Some((px, w as usize, h as usize, title))
    }

    /// Leaves our window out of screen captures (or puts it back).
    pub fn exclude_from_capture(hwnd: isize, on: bool) -> bool {
        unsafe { SetWindowDisplayAffinity(hwnd, if on { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }) != 0 }
    }

    pub fn capture() -> Result<ContextCapture, String> {
        let clipboard_text = clipboard();
        let Some(hwnd) = target() else {
            return Ok(ContextCapture { clipboard_text, ..Default::default() });
        };
        Ok(ContextCapture {
            window_title: title(hwnd),
            process_name: process_name(hwnd),
            selected_text: None,
            clipboard_text,
            screenshot: screenshot(hwnd),
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn remember_foreground() {}

    pub fn screen_frame() -> Option<(Vec<u8>, usize, usize, String)> {
        None
    }

    pub fn exclude_from_capture(_hwnd: isize, _on: bool) -> bool {
        false
    }

    pub fn capture() -> Result<ContextCapture, String> {
        Err("capture_context is only available on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Just enough inflate for what `deflate_fixed` writes.
    fn inflate_fixed(data: &[u8]) -> Vec<u8> {
        let mut pos = 0usize; // bit position
        let bit = |pos: &mut usize| -> u32 {
            let b = (data[*pos / 8] >> (*pos % 8)) & 1;
            *pos += 1;
            b as u32
        };
        let bits = |pos: &mut usize, n: u32| -> u32 {
            let mut v = 0;
            for i in 0..n {
                v |= bit(pos) << i;
            }
            v
        };
        assert_eq!(bits(&mut pos, 1), 1);
        assert_eq!(bits(&mut pos, 2), 1);
        let mut out: Vec<u8> = Vec::new();
        loop {
            // Read 7 bits MSB first, extend as needed.
            let mut code = 0u32;
            for _ in 0..7 {
                code = (code << 1) | bit(&mut pos);
            }
            let sym = if code <= 0b0010111 {
                256 + code
            } else {
                code = (code << 1) | bit(&mut pos);
                if (0x30..=0xBF).contains(&code) {
                    code - 0x30
                } else if (0xC0..=0xC7).contains(&code) {
                    280 + code - 0xC0
                } else {
                    code = (code << 1) | bit(&mut pos);
                    144 + code - 0x190
                }
            };
            match sym {
                0..=255 => out.push(sym as u8),
                256 => break,
                _ => {
                    let idx = (sym - 257) as usize;
                    let len = LEN_BASE[idx] as usize + bits(&mut pos, LEN_EXTRA[idx] as u32) as usize;
                    let mut d = 0;
                    for _ in 0..5 {
                        d = (d << 1) | bit(&mut pos);
                    }
                    assert_eq!(d, 0, "only distance 1 is written");
                    for _ in 0..len {
                        out.push(*out.last().unwrap());
                    }
                }
            }
        }
        out
    }

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn deflate_round_trips() {
        let mut data = b"hola hola".to_vec();
        data.extend(std::iter::repeat(0u8).take(1000));
        data.extend((0..=255u8).cycle().take(700));
        data.extend(std::iter::repeat(7u8).take(259));
        data.extend([1, 1, 1, 2, 2]);
        let packed = deflate_fixed(&data);
        assert_eq!(inflate_fixed(&packed), data);
        assert!(packed.len() < data.len());
        assert_eq!(inflate_fixed(&deflate_fixed(&[])), Vec::<u8>::new());
    }

    #[test]
    fn png_is_well_formed() {
        let (w, h) = (5usize, 3usize);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 % 256) as u8).collect();
        let png = encode_png(&rgb, w, h);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 5);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 3);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        assert_eq!(&png[png.len() - 4..], &0xAE42_6082u32.to_be_bytes());
        // IDAT: zlib header, then deflate that inflates back to the filtered rows.
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        assert_eq!(&png[37..41], b"IDAT");
        let z = &png[41..41 + idat_len];
        assert_eq!(&z[..2], &[0x78, 0x01]);
        let raw = inflate_fixed(&z[2..z.len() - 4]);
        assert_eq!(raw, filter_rows(&rgb, w, h));
        assert_eq!(u32::from_be_bytes(z[z.len() - 4..].try_into().unwrap()), adler32(&raw));
    }

    /// Undoing the filters gives the image back.
    #[test]
    fn filters_are_reversible() {
        let (w, h) = (4usize, 4usize);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3) % 2 * 200 + i % 3) as u8).collect();
        let f = filter_rows(&rgb, w, h);
        let stride = w * 3;
        let mut back = vec![0u8; rgb.len()];
        for y in 0..h {
            let kind = f[y * (stride + 1)];
            for x in 0..stride {
                let v = f[y * (stride + 1) + 1 + x];
                let left = if x >= 3 { back[y * stride + x - 3] } else { 0 };
                let up = if y > 0 { back[(y - 1) * stride + x] } else { 0 };
                back[y * stride + x] = match kind {
                    0 => v,
                    1 => v.wrapping_add(left),
                    _ => v.wrapping_add(up),
                };
            }
        }
        assert_eq!(back, rgb);
    }

    #[test]
    fn downscale_fits_and_averages() {
        // 4x2 BGRA: left half white, right half black.
        let mut bgra = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                let v = if x < 2 { 255 } else { 0 };
                bgra.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let (rgb, w, h) = bgra_to_rgb_fit(&bgra, 4, 2, 2, 100);
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgb, vec![255, 255, 255, 0, 0, 0]);
        let (_, w, h) = bgra_to_rgb_fit(&bgra, 4, 2, 1280, 1600);
        assert_eq!((w, h), (4, 2), "small images are kept as they are");
        // BGRA → RGB order.
        let (rgb, _, _) = bgra_to_rgb_fit(&[1, 2, 3, 255], 1, 1, 10, 10);
        assert_eq!(rgb, vec![3, 2, 1]);
    }

    #[test]
    fn black_frames_are_detected() {
        assert!(all_black(&[0, 0, 0, 255, 0, 0, 0, 0]));
        assert!(!all_black(&[0, 0, 1, 255]));
    }

    #[test]
    fn clipboard_text_is_cleaned() {
        assert_eq!(clean_clipboard("  hola\r\nmundo\u{0}  ").as_deref(), Some("hola\nmundo"));
        assert_eq!(clean_clipboard(" \u{0} "), None);
        let long = "x".repeat(CLIPBOARD_MAX + 50);
        assert_eq!(clean_clipboard(&long).unwrap().chars().count(), CLIPBOARD_MAX);
    }

    #[test]
    fn share_interval_is_clamped() {
        assert_eq!(clamp_interval(None), 5);
        assert_eq!(clamp_interval(Some(0)), 2);
        assert_eq!(clamp_interval(Some(1)), 2);
        assert_eq!(clamp_interval(Some(10)), 10);
        assert_eq!(clamp_interval(Some(600)), 60);
    }

    #[test]
    fn share_times_out_after_30_minutes() {
        assert!(!share_expired(Duration::from_secs(29 * 60 + 59)));
        assert!(share_expired(Duration::from_secs(30 * 60)));
    }

    fn solid(w: usize, h: usize, v: u8) -> Vec<u8> {
        vec![v; w * h * 3]
    }

    #[test]
    fn unchanged_frames_are_not_sent() {
        let a = gray_thumb(&solid(320, 180, 100), 320, 180);
        assert_eq!(a.len(), 80 * 45);
        assert!(frame_worth_sending(None, &a), "the first frame always goes");
        // Noise below the per-pixel delta: same frame.
        let b = gray_thumb(&solid(320, 180, 105), 320, 180);
        assert!(!frame_worth_sending(Some(&a), &b));
        assert_eq!(changed_fraction(&a, &a), 0.0);
    }

    #[test]
    fn a_small_change_is_ignored_a_real_one_is_sent() {
        let (w, h) = (800usize, 450usize);
        let base = solid(w, h, 30);
        let thumb = gray_thumb(&base, w, h);
        // A blinking caret: a few pixels.
        let mut caret = base.clone();
        for y in 100..112 {
            let i = (y * w + 400) * 3;
            caret[i..i + 3].copy_from_slice(&[255, 255, 255]);
        }
        assert!(!frame_worth_sending(Some(&thumb), &gray_thumb(&caret, w, h)));
        // A new window over a tenth of the screen.
        let mut window = base.clone();
        for y in 0..h / 2 {
            for x in 0..w / 5 {
                let i = (y * w + x) * 3;
                window[i..i + 3].copy_from_slice(&[240, 240, 240]);
            }
        }
        let f = changed_fraction(&thumb, &gray_thumb(&window, w, h));
        assert!(f > 0.09 && f < 0.11, "{f}");
        assert!(frame_worth_sending(Some(&thumb), &gray_thumb(&window, w, h)));
        assert_eq!(changed_fraction(&thumb, &[1, 2]), 1.0, "different sizes count as changed");
    }

    #[test]
    fn three_not_connected_in_a_row_end_the_share() {
        let mut e = SendErrors::default();
        let nc: Result<(), String> = Err("not_connected".into());
        assert!(!e.record(&nc));
        assert!(!e.record(&nc));
        assert!(!e.record(&Ok(())), "a success resets the count");
        assert!(!e.record(&nc));
        assert!(!e.record(&Err("http_0".into())), "other errors neither count nor reset");
        assert!(!e.record(&nc));
        assert!(e.record(&nc));
    }

    #[test]
    fn share_event_shape() {
        let on = ScreenShareEvent { active: true, bot: "Ventas".into(), since: Some(1), reason: None };
        assert_eq!(serde_json::to_value(&on).unwrap(), serde_json::json!({"active": true, "bot": "Ventas", "since": 1}));
        let off = ScreenShareEvent { active: false, bot: "Ventas".into(), since: Some(1), reason: Some("timeout") };
        assert_eq!(serde_json::to_value(&off).unwrap()["reason"], "timeout");
    }

    #[test]
    fn frames_fit_the_inline_image_limit() {
        let (w, h) = (1920usize, 1080usize);
        // Noisy content: the worst case for the encoder.
        let mut bgra = vec![0u8; w * h * 4];
        let mut x: u32 = 12345;
        for px in bgra.chunks_exact_mut(4) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            px[..3].copy_from_slice(&x.to_le_bytes()[..3]);
        }
        if let Some((png, thumb)) = encode_frame(&bgra, w, h) {
            assert!(png.len() <= crate::grokbot::IMAGE_EACH / 4 * 3);
            assert_eq!(thumb.len(), 80 * 45);
        }
        let flat = vec![200u8; w * h * 4];
        let (png, _) = encode_frame(&flat, w, h).expect("a flat screen fits at 1280");
        assert!(png.len() < 100_000);
    }

    #[test]
    fn capture_serializes_without_missing_fields() {
        let c = ContextCapture { window_title: "Doc".into(), process_name: "winword.exe".into(), ..Default::default() };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["window_title"], "Doc");
        assert!(v.get("selected_text").is_none());
        assert!(v.get("screenshot").is_none());
    }
}
