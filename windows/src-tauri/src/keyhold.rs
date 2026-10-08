// Hold Space for half a second, anywhere, and the island shows.
//
// A low-level keyboard hook (WH_KEYBOARD_LL) on its own thread watches Space.
// It never swallows or delays a key: every event, Space included, goes on to
// CallNextHookEx untouched, and the callback only stamps the event and hands
// it to a worker thread over a channel (Windows silently drops a hook whose
// callback is slow). The worker runs the hold logic (`HoldDetector`), owns the
// 500 ms timer, checks the settings and emits the existing `shortcut` event
// with "reveal" — the same event and payload style as shortcuts.rs.
//
// Only a clean hold counts: no Ctrl, Alt, Shift or Win at any point, and no
// other key pressed while Space is down (typing "a b" quickly, Space used as a
// modifier in some app). Synthesised input (LLKHF_INJECTED) is ignored.

#![cfg_attr(not(windows), allow(dead_code))]

/// How long Space has to stay down before the island shows.
pub const HOLD_MS: u64 = 500;
/// Auto-repeat comes every ~30 ms (first one after 250–1000 ms). A Space-down
/// this long after the previous event, while we still think Space is held,
/// means its key-up was lost (secure desktop, session switch): a fresh press.
const STALE_MS: u64 = 1500;

/// What the hook reports, already filtered to what the hold logic needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEv {
    /// Space went down, first press or auto-repeat. `mods`: Ctrl, Alt, Shift or
    /// Win was held at that moment.
    SpaceDown { mods: bool },
    SpaceUp,
    /// Any other key went down while Space was held.
    OtherDown,
}

/// The pure hold-timing logic, fed with events and millisecond timestamps.
#[derive(Debug, Default)]
pub struct HoldDetector {
    /// Space is down (clean or not).
    held: bool,
    /// Start of the current clean hold; None once it stopped being clean.
    since: Option<u64>,
    /// Already revealed for this hold.
    fired: bool,
    /// Time of the last event, to spot a lost key-up.
    last: u64,
}

impl HoldDetector {
    /// Feeds one event; true when the reveal should fire now.
    pub fn on_event(&mut self, ev: KeyEv, now: u64) -> bool {
        let stale = self.held && now.saturating_sub(self.last) > STALE_MS;
        self.last = now;
        match ev {
            KeyEv::SpaceDown { mods } => {
                if !self.held || stale {
                    self.held = true;
                    self.fired = false;
                    self.since = if mods { None } else { Some(now) };
                    false
                } else {
                    // Auto-repeat: still held. A modifier joining in spoils it.
                    if mods {
                        self.since = None;
                    }
                    self.check(now)
                }
            }
            KeyEv::SpaceUp => {
                self.held = false;
                self.since = None;
                self.fired = false;
                false
            }
            KeyEv::OtherDown => {
                self.since = None;
                false
            }
        }
    }

    /// The timer: true when the hold has just reached `HOLD_MS`.
    pub fn tick(&mut self, now: u64) -> bool {
        self.check(now)
    }

    /// When the timer should next look, if a clean hold is under way.
    pub fn deadline(&self) -> Option<u64> {
        match self.since {
            Some(t) if !self.fired => Some(t + HOLD_MS),
            _ => None,
        }
    }

    /// Forget everything (Space found physically up at the deadline).
    pub fn reset(&mut self) {
        *self = HoldDetector::default();
    }

    fn check(&mut self, now: u64) -> bool {
        match self.since {
            Some(t) if !self.fired && now.saturating_sub(t) >= HOLD_MS => {
                self.fired = true;
                true
            }
            _ => false,
        }
    }
}

#[cfg(windows)]
pub use self::win::{start, stop};

#[cfg(not(windows))]
pub fn start(_app: tauri::AppHandle) {}
#[cfg(not(windows))]
pub fn stop() {}

#[cfg(windows)]
mod win {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use tauri::{AppHandle, Emitter, Manager};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_SPACE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
        UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
        WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::{HoldDetector, KeyEv};
    use crate::island::WINDOW_LABEL;
    use crate::{log, Shared};

    static TX: OnceLock<Sender<(KeyEv, u64)>> = OnceLock::new();
    static CLOCK: OnceLock<Instant> = OnceLock::new();
    /// Space is down, as far as the hook has seen: other keys only matter then.
    static SPACE_HELD: AtomicBool = AtomicBool::new(false);
    static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

    fn now_ms() -> u64 {
        CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    fn key_down(vk: VIRTUAL_KEY) -> bool {
        unsafe { (GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
    }

    fn modifiers_held() -> bool {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN].into_iter().any(key_down)
    }

    fn send(ev: KeyEv) {
        if let Some(tx) = TX.get() {
            let _ = tx.send((ev, now_ms()));
        }
    }

    /// The hook. Classifies, stamps, forwards — and always passes the key on.
    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && lparam.0 != 0 {
            let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if (k.flags & LLKHF_INJECTED).0 == 0 {
                let msg = wparam.0 as u32;
                let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                if k.vkCode == VK_SPACE.0 as u32 {
                    if down {
                        SPACE_HELD.store(true, Ordering::Relaxed);
                        send(KeyEv::SpaceDown { mods: modifiers_held() });
                    } else if up {
                        SPACE_HELD.store(false, Ordering::Relaxed);
                        send(KeyEv::SpaceUp);
                    }
                } else if down && SPACE_HELD.load(Ordering::Relaxed) {
                    send(KeyEv::OtherDown);
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Installs the hook on its own thread (with the message loop a low-level
    /// hook needs) and starts the worker. Safe to call once; later calls do nothing.
    pub fn start(app: AppHandle) {
        let (tx, rx) = mpsc::channel();
        if TX.set(tx).is_err() {
            return;
        }
        let _ = now_ms(); // start the clock before the first event
        let worker_app = app.clone();
        let _ = std::thread::Builder::new()
            .name("keyhold-worker".into())
            .spawn(move || worker(worker_app, rx));
        let _ = std::thread::Builder::new().name("keyhold-hook".into()).spawn(|| unsafe {
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(err) => {
                    log::line(format!("keyhold: could not install the keyboard hook: {err}"));
                    return;
                }
            };
            log::line("keyhold: hold-Space hook installed");
            let mut msg = MSG::default();
            // 0 is WM_QUIT (from stop()), -1 an error: either way, unhook.
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(hook);
            log::line("keyhold: hook removed");
        });
    }

    /// Ends the hook thread's message loop, which unhooks. (Windows also drops
    /// the hook by itself when the process exits.)
    pub fn stop() {
        let id = HOOK_THREAD.load(Ordering::Relaxed);
        if id != 0 {
            let _ = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }

    fn worker(app: AppHandle, rx: Receiver<(KeyEv, u64)>) {
        let mut hold = HoldDetector::default();
        loop {
            let event = match hold.deadline() {
                Some(deadline) => {
                    let wait = deadline.saturating_sub(now_ms());
                    match rx.recv_timeout(Duration::from_millis(wait)) {
                        Ok(e) => Some(e),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match rx.recv() {
                    Ok(e) => Some(e),
                    Err(_) => return,
                },
            };
            let fire = match event {
                Some((ev, at)) => hold.on_event(ev, at),
                None => hold.tick(now_ms()),
            };
            if !fire {
                continue;
            }
            // A key-up we never saw must not leave a phantom hold behind.
            if !key_down(VK_SPACE) {
                hold.reset();
                continue;
            }
            let enabled = app
                .try_state::<Shared>()
                .map(|s| s.settings.lock().unwrap().space_hold)
                .unwrap_or(false);
            if !enabled {
                continue;
            }
            log::line(format!("keyhold: Space held {} ms -> shortcut reveal", super::HOLD_MS));
            let _ = app.emit_to(WINDOW_LABEL, "shortcut", "reveal".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWN: KeyEv = KeyEv::SpaceDown { mods: false };
    const DOWN_MOD: KeyEv = KeyEv::SpaceDown { mods: true };

    /// Space down at 0, auto-repeat from `first` every 33 ms until `until`.
    fn hold(d: &mut HoldDetector, first: u64, until: u64) -> Vec<u64> {
        let mut fired = Vec::new();
        if d.on_event(DOWN, 0) {
            fired.push(0);
        }
        let mut t = first;
        while t <= until {
            if d.on_event(DOWN, t) {
                fired.push(t);
            }
            t += 33;
        }
        fired
    }

    #[test]
    fn short_tap_never_fires() {
        let mut d = HoldDetector::default();
        assert!(!d.on_event(DOWN, 0));
        assert!(!d.tick(120));
        assert!(!d.on_event(KeyEv::SpaceUp, 140));
        assert_eq!(d.deadline(), None);
        assert!(!d.tick(600));
    }

    #[test]
    fn timer_fires_at_500_ms_once() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 1000);
        assert_eq!(d.deadline(), Some(1500));
        assert!(!d.tick(1499));
        assert!(d.tick(1500));
        assert!(!d.tick(1600));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn repeats_fire_once_without_timer() {
        // Default repeat delay (~500 ms) then ~30 Hz: fires on the first repeat at or past 500 ms.
        let mut d = HoldDetector::default();
        let fired = hold(&mut d, 500, 2000);
        assert_eq!(fired, vec![500]);
    }

    #[test]
    fn short_repeat_delay_fires_after_threshold() {
        let mut d = HoldDetector::default();
        let fired = hold(&mut d, 250, 1500);
        // 250, 283, ... first >= 500 is 514.
        assert_eq!(fired, vec![514]);
    }

    #[test]
    fn slow_repeat_delay_relies_on_timer() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        assert!(d.tick(500));
        // The late first repeat (1000 ms) does not fire again.
        assert!(!d.on_event(DOWN, 1000));
    }

    #[test]
    fn release_rearms() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        assert!(d.tick(500));
        d.on_event(KeyEv::SpaceUp, 700);
        d.on_event(DOWN, 900);
        assert_eq!(d.deadline(), Some(1400));
        assert!(d.tick(1400));
    }

    #[test]
    fn modifier_at_press_never_fires() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN_MOD, 0);
        assert_eq!(d.deadline(), None);
        assert!(!d.on_event(DOWN, 600));
        assert!(!d.tick(700));
    }

    #[test]
    fn modifier_joining_cancels_hold() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        assert!(!d.on_event(DOWN_MOD, 300));
        assert_eq!(d.deadline(), None);
        assert!(!d.tick(600));
    }

    #[test]
    fn other_key_cancels_hold_until_release() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        d.on_event(KeyEv::OtherDown, 100);
        assert_eq!(d.deadline(), None);
        assert!(!d.on_event(DOWN, 600));
        d.on_event(KeyEv::SpaceUp, 650);
        d.on_event(DOWN, 700);
        assert!(d.tick(1200));
    }

    #[test]
    fn lost_key_up_counts_next_press_as_fresh() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        assert!(d.tick(500));
        // No key-up ever arrived; a press much later starts a new hold.
        assert!(!d.on_event(DOWN, 10_000));
        assert_eq!(d.deadline(), Some(10_500));
        assert!(d.tick(10_500));
    }

    #[test]
    fn up_without_down_is_harmless() {
        let mut d = HoldDetector::default();
        assert!(!d.on_event(KeyEv::SpaceUp, 5));
        assert!(!d.on_event(KeyEv::OtherDown, 6));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn reset_forgets_hold() {
        let mut d = HoldDetector::default();
        d.on_event(DOWN, 0);
        d.reset();
        assert_eq!(d.deadline(), None);
        assert!(!d.tick(600));
    }
}
