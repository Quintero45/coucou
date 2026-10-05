// Windows: Win32 for the island window and the cursor, %APPDATA% for files.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use tauri::{AppHandle, Manager, WebviewWindow};

use ::windows::core::{BOOL, PWSTR};
use ::windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, HWND, LPARAM, LocalFree, POINT};
use ::windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use ::windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use ::windows::Win32::System::SystemInformation::GetLocalTime;
use ::windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken};
use ::windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use ::windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetCursorPos, GetParent, GetPropW, GetWindowLongPtrW,
    GetWindowThreadProcessId, SetWindowLongPtrW, WindowFromPoint, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

use super::LocalTime;
use crate::island::WINDOW_LABEL;

/// File name of the Claude Code relay.
pub const HOOK_EXE: &str = "coucou-hook.exe";

/// Environment variable holding the home directory.
pub const HOME_VAR: &str = "USERPROFILE";

/// Keeps spawned helpers from flashing a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ── Files ─────────────────────────────────────────────────────────────────────

/// %APPDATA%\Coucou — preferences.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

/// %LOCALAPPDATA%\Coucou — where coucou-hook.exe, the inbox and the log live.
pub fn local_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Coucou")
}

/// %APPDATA% and %LOCALAPPDATA% are already private to the user.
pub fn ensure_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Nothing to set up before the webview starts.
pub fn prepare_environment() {}

pub fn local_time() -> LocalTime {
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear.into(),
        month: t.wMonth.into(),
        day: t.wDay.into(),
        hour: t.wHour.into(),
        minute: t.wMinute.into(),
        second: t.wSecond.into(),
    }
}

// ── Processes ─────────────────────────────────────────────────────────────────

/// Spawned helpers must never flash a console window.
pub fn no_console(cmd: &mut Command) -> &mut Command {
    cmd.creation_flags(CREATE_NO_WINDOW)
}

/// Long-lived helpers (the Cursor bridge) die with the app, even when it is
/// killed: they join a job object whose only handle closes when we exit.
pub fn tie_to_app(pid: u32) {
    use ::windows::core::PCWSTR;
    use ::windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use ::windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
    static JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let Ok(job) = CreateJobObjectW(None, PCWSTR::null()) else { return 0 };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let _ = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job.0 as usize
    });
    if job == 0 {
        return;
    }
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) {
            let _ = AssignProcessToJobObject(HANDLE(job as *mut _), process);
            let _ = CloseHandle(process);
        }
    }
}

pub fn open_url(url: &str) {
    let _ = no_console(Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]))
        .spawn();
}

pub fn reveal_folder(path: &str) {
    let _ = Command::new("explorer").arg(path).spawn();
}

/// Opens an app, a file, a folder or a URL the way Explorer's Run box would:
/// App Paths names (`notepad`, `chrome`, `excel`) work as well as full paths.
/// Only ever called after the owner approved exactly this target.
pub fn shell_open(target: &str, args: Option<&str>) -> Result<(), String> {
    use ::windows::core::HSTRING;
    use ::windows::Win32::UI::Shell::ShellExecuteW;
    use ::windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let file = HSTRING::from(target);
    let verb = HSTRING::from("open");
    let params = args.map(HSTRING::from);
    let result = unsafe {
        match &params {
            Some(p) => ShellExecuteW(None, &verb, &file, p, None, SW_SHOWNORMAL),
            None => ShellExecuteW(None, &verb, &file, None, None, SW_SHOWNORMAL),
        }
    };
    // ShellExecute reports success as a value above 32.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open {target} (code {})", result.0 as isize))
    }
}

/// The shell the assistant's commands run in.
pub fn shell_command(script: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

pub const SHELL_NAME: &str = "PowerShell";

/// Our own `where`: walks %PATH% against %PATHEXT%, no shell involved.
/// Rust quotes arguments correctly for `.cmd`/`.bat` targets since 1.77, so
/// spawning `code.cmd` directly is safe.
pub fn find_on_path(stem: &str) -> Option<PathBuf> {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    let dirs = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&dirs) {
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            let candidate = dir.join(format!("{stem}{}", ext.to_lowercase()));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

// ── Who we are ────────────────────────────────────────────────────────────────
//
// Named pipes share one machine-wide namespace, so the SID in the name is what
// keeps two accounts on the same machine from ever meeting on `coucou-*`.
// coucou-hook computes the same string (hook/src/win.rs) and additionally checks
// that the process serving the pipe really is us.

/// The SID of the account this process runs as, as `S-1-5-21-…`.
pub fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;

        // First call sizes the buffer, second fills it.
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut _)));
        sid
    }
}

// ── Cursor ────────────────────────────────────────────────────────────────────

/// The 60 Hz poll reads the cursor and flips click-through from it.
pub const CURSOR_POLL: bool = true;

/// Cursor position in physical screen pixels.
pub fn cursor_physical() -> Option<(f64, f64)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x as f64, p.y as f64))
}

/// True while the left mouse button is held — the only signal we get that a
/// drag might be in flight before it reaches the window.
pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

// ── Island window ─────────────────────────────────────────────────────────────

fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
    let raw = win.hwnd().ok()?.0 as isize;
    if raw == 0 {
        return None;
    }
    Some(HWND(raw as *mut _))
}

/// Reports the drop target on WebView2's render widget (it used to revoke it).
///
/// History: wry installs its drop target by walking the webview's child windows **once**,
/// when the webview is created. WebView2 creates `Chrome_RenderWidgetHostHWND`
/// later and registers its own target on it; being the innermost window, that one
/// wins, and since the page has no HTML5 drop handler it refuses everything — the
/// "no drop" cursor, with nothing reaching Tauri. Revoking it makes OLE fall
/// through to the target wry registered on the parent widget, which is the one
/// that feeds Tauri's drag events.
///
/// Cheap and idempotent, so it is simply re-run whenever a drag might be starting.
pub fn unblock_webview_drops(app: &AppHandle) {
    for label in [WINDOW_LABEL, "settings"] {
        let Some(win) = app.get_webview_window(label) else { continue };
        let Some(hwnd) = hwnd_of(&win) else { continue };
        unsafe {
            let _ = EnumChildWindows(Some(hwnd), Some(revoke_render_widget), LPARAM(0));
        }
    }
}

unsafe extern "system" fn revoke_render_widget(hwnd: HWND, _: LPARAM) -> BOOL {
    if class_of(hwnd) == "Chrome_RenderWidgetHostHWND" {
        // Only a widget that actually holds a target is worth a call, and a log
        // line: this runs on every mouse press. With the WebView2 runtime checked on
        // 2026-10-04 the render widget (owned by msedgewebview2.exe) holds none,
        // so this is normally silent; the line shows up if that ever changes.
        //
        // It is no longer revoked, only reported. The live test of 2026-10-04
        // showed that the target found there at startup lives in *our* heap
        // (same address range as wry's target on Chrome_WidgetWin_0, and as the
        // ones wry put on other msedgewebview2-owned windows): wry registers
        // across the process boundary. Revoking it removed wry's own target, and
        // OLE then never walked up to the one on Chrome_WidgetWin_0. Drops are
        // caught by the drag overlay (below) whichever target sits there.
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if has_drop_target(hwnd) && !REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::log::line(format!(
                "drag-diag {} ({}) holds a drop target; left in place (the drag overlay sits above it)",
                hex(hwnd),
                owner_of(hwnd),
            ));
        }
    }
    true.into()
}

// ── Drop-target diagnostics ───────────────────────────────────────────────────
//
// OLE picks a drop target by taking WindowFromPoint under the cursor and walking
// up the parents until one carries the `OleDropTargetInterface` property. These
// helpers write that chain to coucou.log so a live drag shows exactly which
// window OLE lands on and whose target answers: wry's (our process, feeds
// Tauri's drag events) or WebView2's own (msedgewebview2.exe, refuses drops
// because wry turned `AllowExternalDrop` off).

fn hex(hwnd: HWND) -> String {
    format!("0x{:X}", hwnd.0 as usize)
}

fn class_of(hwnd: HWND) -> String {
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut name) };
    String::from_utf16_lossy(&name[..len.max(0) as usize])
}

fn has_drop_target(hwnd: HWND) -> bool {
    !unsafe { GetPropW(hwnd, ::windows::core::w!("OleDropTargetInterface")) }.is_invalid()
}

fn owner_of(hwnd: HWND) -> String {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == unsafe { GetCurrentProcessId() } {
        "own".to_string()
    } else {
        format!("pid {pid}")
    }
}

fn describe(hwnd: HWND) -> String {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    format!(
        "{} {} [{}{}{}]",
        hex(hwnd),
        class_of(hwnd),
        owner_of(hwnd),
        if ex & WS_EX_TRANSPARENT.0 as isize != 0 { ", transparent" } else { "" },
        if has_drop_target(hwnd) { ", TARGET" } else { "" },
    )
}

/// The chain OLE searches under the cursor right now, innermost first, up to
/// the first window holding a drop target.
pub fn drop_chain_at_cursor() -> String {
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return "no cursor".into();
    }
    let mut hwnd = unsafe { WindowFromPoint(p) };
    let mut parts = Vec::new();
    for _ in 0..10 {
        if hwnd.is_invalid() {
            break;
        }
        parts.push(describe(hwnd));
        if has_drop_target(hwnd) {
            break;
        }
        hwnd = unsafe { GetParent(hwnd) }.unwrap_or_default();
    }
    if parts.is_empty() {
        return format!("nothing at ({}, {})", p.x, p.y);
    }
    format!("at ({}, {}): {}", p.x, p.y, parts.join(" > "))
}

/// Every window of the island, top-level first, with who owns it and whether
/// it holds a drop target.
pub fn drop_targets_summary(app: &AppHandle) -> String {
    let Some(win) = app.get_webview_window(WINDOW_LABEL) else { return "no island window".into() };
    let Some(hwnd) = hwnd_of(&win) else { return "no island hwnd".into() };
    let mut parts = vec![describe(hwnd)];
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let parts = unsafe { &mut *(lparam.0 as *mut Vec<String>) };
        parts.push(describe(hwnd));
        true.into()
    }
    unsafe {
        let _ = EnumChildWindows(Some(hwnd), Some(collect), LPARAM(&mut parts as *mut Vec<String> as isize));
    }
    parts.join(" | ")
}

/// WS_EX_NOACTIVATE keeps clicks from stealing focus; WS_EX_TOOLWINDOW keeps the
/// island out of Alt-Tab.
pub fn make_non_activating(win: &WebviewWindow) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
}

/// Temporarily allow activation so a text field inside the island can be typed in.
pub fn set_activating(win: &WebviewWindow, activating: bool) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if activating {
            ex & !(WS_EX_NOACTIVATE.0 as isize)
        } else {
            ex | WS_EX_NOACTIVATE.0 as isize
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
}

/// Click-through here is the poll's WS_EX_TRANSPARENT toggle, not a region.
pub fn set_input_region(_win: &WebviewWindow, _rect: Option<(f64, f64, f64, f64)>) {}

// ── Drag overlay ──────────────────────────────────────────────────────────────
//
// Files dragged from Explorer never reached Tauri: under the cursor OLE finds
// Chrome_RenderWidgetHostHWND, a window of the msedgewebview2.exe browser
// process, and does not get from there to the target wry registered on our own
// Chrome_WidgetWin_0 (live test, 2026-10-04: `drag-watch` and click-through OFF
// logged, never a single `drag-native`).
//
// So while a drag is coming in, an invisible window of our own covers the
// island: a layered, owned, topmost popup at alpha 1/255 (a layered *child* would
// need a Windows 8 manifest entry, and a plain child would have to paint over
// WebView2's swap chain). It holds our own IDropTarget, accepts CF_HDROP with
// DROPEFFECT_COPY and re-emits exactly what Tauri emits for a webview drop
// (`tauri://drag-enter|over|drop|leave`, see tauri/src/manager/webview.rs), so
// `getCurrentWebview().onDragDropEvent` in the page sees no difference.
//
// It is shown only from the drag watch (a press from outside reaching the island
// or the notch zone) and hidden on drop, DragLeave, Esc, button up, a click on it,
// or 3 s without DragOver or cursor movement. It never touches the island's own
// click-through flag. Everything here runs on the main thread, which already has
// OLE initialised (wry and tao register their targets there).
//
// The `windows` 0.61 features of this crate have no IDropTarget_Impl, IDataObject,
// WNDCLASSEXW or ScreenToClient (they need Win32_System_Com, SystemServices and
// Graphics_Gdi, and Cargo.toml is not ours to change), so the COM object and
// those few calls are declared by hand below.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// How often the overlay checks the button, Esc and the watchdog.
pub const OVERLAY_TICK_MS: u32 = 50;
/// After the button comes up over an entered overlay, how long OLE gets to
/// deliver Drop (or DragLeave) before the overlay goes away by itself.
pub const OVERLAY_DROP_GRACE_MS: u64 = 400;
/// No DragOver and no cursor movement this long: the overlay goes away.
pub const OVERLAY_WATCHDOG_MS: u64 = 3000;

/// Why the overlay went away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayHide {
    Dropped,
    Left,
    Escape,
    ButtonUp,
    Watchdog,
    /// It received real mouse input: no drag can be in progress.
    Mouse,
}

impl OverlayHide {
    pub fn name(self) -> &'static str {
        match self {
            OverlayHide::Dropped => "drop",
            OverlayHide::Left => "leave",
            OverlayHide::Escape => "escape",
            OverlayHide::ButtonUp => "button-up",
            OverlayHide::Watchdog => "watchdog",
            OverlayHide::Mouse => "mouse",
        }
    }

    /// Whether the overlay may come back during the same press. Only after a
    /// DragLeave (the drag went elsewhere and may return); any other end means
    /// this press is done with.
    pub fn blocks_press(self) -> bool {
        self != OverlayHide::Left
    }
}

/// The overlay's lifetime rules, free of Win32 so they can be tested.
#[derive(Debug, Default)]
pub struct OverlayLogic {
    visible: bool,
    /// OLE called DragEnter with files on it.
    entered: bool,
    last_activity: u64,
    released_at: Option<u64>,
    cursor: Option<(i32, i32)>,
}

impl OverlayLogic {
    #[cfg(test)]
    pub fn visible(&self) -> bool {
        self.visible
    }

    #[cfg(test)]
    pub fn entered(&self) -> bool {
        self.entered
    }

    pub fn show(&mut self, now: u64) {
        if !self.visible {
            *self = OverlayLogic { visible: true, last_activity: now, ..Default::default() };
        } else {
            self.last_activity = now;
        }
    }

    pub fn enter(&mut self, now: u64) {
        if self.visible {
            self.entered = true;
            self.last_activity = now;
        }
    }

    pub fn over(&mut self, now: u64) {
        if self.visible {
            self.last_activity = now;
        }
    }

    /// OLE's DragLeave / Drop, or a forced end. Returns the reason and whether
    /// the page had seen an enter (so it is owed a leave unless dropped/left).
    pub fn end(&mut self, why: OverlayHide) -> Option<(OverlayHide, bool)> {
        if !self.visible {
            return None;
        }
        let entered = self.entered;
        *self = OverlayLogic::default();
        Some((why, entered))
    }

    /// The timer. `cursor` in screen pixels.
    pub fn tick(&mut self, now: u64, button_down: bool, esc_down: bool, cursor: (i32, i32)) -> Option<(OverlayHide, bool)> {
        if !self.visible {
            return None;
        }
        if esc_down {
            return self.end(OverlayHide::Escape);
        }
        if self.cursor != Some(cursor) {
            self.cursor = Some(cursor);
            self.last_activity = now;
        }
        if button_down {
            self.released_at = None;
        } else {
            // Not entered: nothing is coming, go now. Entered: OLE delivers Drop
            // on that same button-up; give it a moment.
            let at = *self.released_at.get_or_insert(now);
            if !self.entered || now.saturating_sub(at) >= OVERLAY_DROP_GRACE_MS {
                return self.end(OverlayHide::ButtonUp);
            }
        }
        if now.saturating_sub(self.last_activity) >= OVERLAY_WATCHDOG_MS {
            return self.end(OverlayHide::Watchdog);
        }
        None
    }
}

/// Read from the poll thread: is the overlay up, and is this press done with it.
static OVERLAY_VISIBLE: AtomicBool = AtomicBool::new(false);
static OVERLAY_BLOCKED: AtomicBool = AtomicBool::new(false);
static OVERLAY_PENDING: AtomicBool = AtomicBool::new(false);

/// A new left-button press: a fresh drag may use the overlay again.
pub fn drag_overlay_new_press() {
    OVERLAY_BLOCKED.store(false, Ordering::Relaxed);
}

/// Poll thread, every tick an incoming drag is over the island or the notch
/// zone. Cheap when the overlay is already up (three atomic loads).
pub fn drag_overlay_request(app: &AppHandle) {
    if OVERLAY_VISIBLE.load(Ordering::Relaxed)
        || OVERLAY_BLOCKED.load(Ordering::Relaxed)
        || OVERLAY_PENDING.swap(true, Ordering::Relaxed)
    {
        return;
    }
    let handle = app.clone();
    if app.run_on_main_thread(move || overlay_show(&handle)).is_err() {
        OVERLAY_PENDING.store(false, Ordering::Relaxed);
    }
}

mod ffi {
    #![allow(non_snake_case)]
    use super::{HWND, LPARAM, POINT};
    use ::windows::core::{BOOL, HRESULT};
    use ::windows::Win32::Foundation::{LRESULT, WPARAM};
    use std::ffi::c_void;

    #[repr(C)]
    pub struct WndClassExW {
        pub cbSize: u32,
        pub style: u32,
        pub lpfnWndProc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
        pub cbClsExtra: i32,
        pub cbWndExtra: i32,
        pub hInstance: *mut c_void,
        pub hIcon: *mut c_void,
        pub hCursor: *mut c_void,
        pub hbrBackground: *mut c_void,
        pub lpszMenuName: *const u16,
        pub lpszClassName: *const u16,
        pub hIconSm: *mut c_void,
    }

    #[repr(C)]
    pub struct FormatEtc {
        pub cfFormat: u16,
        pub ptd: *mut c_void,
        pub dwAspect: u32,
        pub lindex: i32,
        pub tymed: u32,
    }

    #[repr(C)]
    pub struct StgMedium {
        pub tymed: u32,
        pub data: *mut c_void,
        pub pUnkForRelease: *mut c_void,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn RegisterClassExW(class: *const WndClassExW) -> u16;
        pub fn ScreenToClient(hwnd: HWND, point: *mut POINT) -> BOOL;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn RegisterDragDrop(hwnd: HWND, target: *mut c_void) -> HRESULT;
        pub fn ReleaseStgMedium(medium: *mut StgMedium);
        pub fn OleInitialize(reserved: *mut c_void) -> HRESULT;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }
}

struct Overlay {
    app: AppHandle,
    hwnd: HWND,
    island: HWND,
    logic: OverlayLogic,
    /// The current OLE drag carries files (DragEnter accepted it).
    valid: bool,
    overs_logged: u32,
    rect: (i32, i32, i32, i32),
}

thread_local! {
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn overlay_now() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

const OVERLAY_TIMER: usize = 0xC0C0;

/// The island's screen rect to cover; around the 6 px wake strip, the wider
/// notch zone the drag watch uses.
fn overlay_rect(app: &AppHandle, island: HWND) -> Option<(i32, i32, i32, i32)> {
    use ::windows::Win32::Foundation::RECT;
    use ::windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut r = RECT::default();
    unsafe { GetWindowRect(island, &mut r) }.ok()?;
    let scale = app
        .get_webview_window(WINDOW_LABEL)
        .and_then(|w| w.scale_factor().ok())
        .unwrap_or(1.0);
    if ((r.bottom - r.top) as f64) < 30.0 * scale {
        let cx = (crate::island::DRAG_CATCH_X * scale).round() as i32;
        let cy = (crate::island::DRAG_CATCH_Y * scale).round() as i32;
        r.left -= cx;
        r.right += cx;
        r.bottom += cy;
    }
    Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
}

fn overlay_create(island: HWND) -> Option<HWND> {
    use ::windows::Win32::Foundation::{COLORREF, HINSTANCE};
    use ::windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SetLayeredWindowAttributes, LWA_ALPHA, WS_EX_LAYERED, WS_EX_TOPMOST, WS_POPUP,
    };
    let class = ::windows::core::w!("CoucouDragOverlay");
    unsafe {
        let hinstance = ffi::GetModuleHandleW(std::ptr::null());
        static REGISTERED: AtomicBool = AtomicBool::new(false);
        if !REGISTERED.swap(true, Ordering::Relaxed) {
            let wc = ffi::WndClassExW {
                cbSize: std::mem::size_of::<ffi::WndClassExW>() as u32,
                style: 0,
                lpfnWndProc: Some(overlay_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                // COLOR_WINDOWTEXT + 1: a system brush, drawn at alpha 1/255.
                hbrBackground: 9 as *mut c_void,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            if ffi::RegisterClassExW(&wc) == 0 {
                crate::log::line(format!("drag-overlay class registration failed: {}", ::windows::core::Error::from_win32()));
            }
        }
        let hwnd = match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            class,
            ::windows::core::w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            Some(island), // owner: always above the island, gone with it
            None,
            Some(HINSTANCE(hinstance)),
            None,
        ) {
            Ok(h) => h,
            Err(err) => {
                crate::log::line(format!("drag-overlay create failed: {err}"));
                return None;
            }
        };
        // Alpha 1, not 0: invisible, but still hit-tested like any window.
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);

        let target = drop_target_object();
        let mut hr = ffi::RegisterDragDrop(hwnd, target);
        if hr.is_err() {
            // CO_E_NOTINITIALIZED / E_OUTOFMEMORY: OLE not set up on this thread.
            let init = ffi::OleInitialize(std::ptr::null_mut());
            crate::log::line(format!("drag-overlay RegisterDragDrop {hr:?}; OleInitialize {init:?}, retrying"));
            hr = ffi::RegisterDragDrop(hwnd, target);
        }
        crate::log::line(format!(
            "drag-overlay created {} owner={} RegisterDragDrop={}",
            hex(hwnd),
            hex(island),
            if hr.is_ok() { "ok".to_string() } else { format!("{hr:?}") }
        ));
        Some(hwnd)
    }
}

fn overlay_show(app: &AppHandle) {
    use ::windows::Win32::UI::WindowsAndMessaging::{
        SetTimer, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    };
    OVERLAY_PENDING.store(false, Ordering::Relaxed);
    if OVERLAY_BLOCKED.load(Ordering::Relaxed) || !left_button_down() {
        return;
    }
    let Some(win) = app.get_webview_window(WINDOW_LABEL) else { return };
    let Some(island) = hwnd_of(&win) else { return };
    let existing = OVERLAY.with(|o| o.borrow().as_ref().map(|o| (o.hwnd, o.island)));
    let hwnd = match existing {
        Some((h, i)) if i == island => h,
        _ => {
            let Some(h) = overlay_create(island) else { return };
            OVERLAY.with(|o| {
                *o.borrow_mut() = Some(Overlay {
                    app: app.clone(),
                    hwnd: h,
                    island,
                    logic: OverlayLogic::default(),
                    valid: false,
                    overs_logged: 0,
                    rect: (0, 0, 0, 0),
                })
            });
            h
        }
    };
    let Some(rect) = overlay_rect(app, island) else { return };
    OVERLAY.with(|o| {
        if let Some(o) = o.borrow_mut().as_mut() {
            o.logic.show(overlay_now());
            o.rect = rect;
            o.valid = false;
            o.overs_logged = 0;
        }
    });
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        SetTimer(Some(hwnd), OVERLAY_TIMER, OVERLAY_TICK_MS, None);
    }
    OVERLAY_VISIBLE.store(true, Ordering::Relaxed);
    crate::log::line(format!(
        "drag-overlay show {} at ({}, {}) {}x{} (physical)",
        hex(hwnd),
        rect.0,
        rect.1,
        rect.2,
        rect.3
    ));
}

/// Takes the overlay down after `logic` decided so. Sends the page a leave when
/// it had an enter and the drag did not end in its own drop/leave.
fn overlay_hide(why: OverlayHide, entered: bool) {
    use ::windows::Win32::UI::WindowsAndMessaging::{KillTimer, ShowWindow, SW_HIDE};
    let Some((hwnd, app)) = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            if entered {
                o.valid = false;
            }
            (o.hwnd, o.app.clone())
        })
    }) else {
        return;
    };
    unsafe {
        let _ = KillTimer(Some(hwnd), OVERLAY_TIMER);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    OVERLAY_VISIBLE.store(false, Ordering::Relaxed);
    if why.blocks_press() {
        OVERLAY_BLOCKED.store(true, Ordering::Relaxed);
    }
    let owe_leave = entered && !matches!(why, OverlayHide::Dropped | OverlayHide::Left);
    if owe_leave {
        emit_drag(&app, "tauri://drag-leave", DragPayload::None);
    }
    crate::log::line(format!(
        "drag-overlay hide reason={}{}",
        why.name(),
        if owe_leave { " (sent leave)" } else { "" }
    ));
}

fn overlay_tick() {
    use ::windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use ::windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE};
    let esc = unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0 };
    let down = left_button_down();
    let cursor = cursor_physical().map(|(x, y)| (x as i32, y as i32)).unwrap_or((0, 0));
    let Some((decision, app, hwnd, island, old_rect)) = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            (o.logic.tick(overlay_now(), down, esc, cursor), o.app.clone(), o.hwnd, o.island, o.rect)
        })
    }) else {
        return;
    };
    if let Some((why, entered)) = decision {
        overlay_hide(why, entered);
        return;
    }
    // Follow the island while it opens (wake strip -> full panel).
    if let Some(rect) = overlay_rect(&app, island) {
        if rect != old_rect {
            OVERLAY.with(|o| {
                if let Some(o) = o.borrow_mut().as_mut() {
                    o.rect = rect;
                }
            });
            unsafe {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), rect.0, rect.1, rect.2, rect.3, SWP_NOACTIVATE);
            }
        }
    }
}

unsafe extern "system" fn overlay_wndproc(hwnd: HWND, msg: u32, wparam: ::windows::Win32::Foundation::WPARAM, lparam: LPARAM) -> ::windows::Win32::Foundation::LRESULT {
    use ::windows::Win32::Foundation::LRESULT;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_TIMER,
    };
    const MA_NOACTIVATE: isize = 3;
    match msg {
        WM_TIMER if wparam.0 == OVERLAY_TIMER => {
            overlay_tick();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE),
        // During an OLE drag the source holds the mouse capture, so real mouse
        // input here means there is no drag: never sit on the island's clicks.
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            if let Some(Some((why, entered))) =
                OVERLAY.with(|o| o.borrow_mut().as_mut().map(|o| o.logic.end(OverlayHide::Mouse)))
            {
                overlay_hide(why, entered);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ── The IDropTarget ───────────────────────────────────────────────────────────

/// Payload of Tauri's drag events, serialised like tauri's own
/// `DragDropPayload` (`paths` omitted when absent, `position` physical px
/// relative to the window); leave carries `null`.
#[derive(Clone)]
enum DragPayload {
    Files { paths: Vec<std::path::PathBuf>, x: f64, y: f64 },
    Position { x: f64, y: f64 },
    None,
}

impl serde::Serialize for DragPayload {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        #[derive(serde::Serialize)]
        struct Pos {
            x: f64,
            y: f64,
        }
        match self {
            DragPayload::Files { paths, x, y } => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("paths", paths)?;
                m.serialize_entry("position", &Pos { x: *x, y: *y })?;
                m.end()
            }
            DragPayload::Position { x, y } => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("position", &Pos { x: *x, y: *y })?;
                m.end()
            }
            DragPayload::None => s.serialize_unit(),
        }
    }
}

/// Emits like tauri's `emit_to_webview`: to the island's webview / webview
/// window. Queued on the event loop, i.e. outside the incoming COM call.
fn emit_drag(app: &AppHandle, event: &'static str, payload: DragPayload) {
    use tauri::{Emitter, EventTarget};
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = handle.emit_filter(event, payload, |target| match target {
            EventTarget::Webview { label } | EventTarget::WebviewWindow { label } => label == WINDOW_LABEL,
            _ => false,
        });
    });
}

const DROPEFFECT_NONE: u32 = 0;
const DROPEFFECT_COPY: u32 = 1;

#[repr(C)]
struct DropTargetVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const ::windows::core::GUID, *mut *mut c_void) -> ::windows::core::HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    drag_enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, ::windows::Win32::Foundation::POINTL, *mut u32) -> ::windows::core::HRESULT,
    drag_over: unsafe extern "system" fn(*mut c_void, u32, ::windows::Win32::Foundation::POINTL, *mut u32) -> ::windows::core::HRESULT,
    drag_leave: unsafe extern "system" fn(*mut c_void) -> ::windows::core::HRESULT,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, ::windows::Win32::Foundation::POINTL, *mut u32) -> ::windows::core::HRESULT,
}

#[repr(C)]
struct DropTargetObject {
    vtbl: *const DropTargetVtbl,
    refs: AtomicU32,
}

static DROP_TARGET_VTBL: DropTargetVtbl = DropTargetVtbl {
    query_interface: dt_query_interface,
    add_ref: dt_add_ref,
    release: dt_release,
    drag_enter: dt_drag_enter,
    drag_over: dt_drag_over,
    drag_leave: dt_drag_leave,
    drop: dt_drop,
};

/// One object for the life of the process (never freed, so the reference count
/// is kept only to answer COM correctly).
fn drop_target_object() -> *mut c_void {
    struct Ptr(*mut c_void);
    unsafe impl Send for Ptr {}
    unsafe impl Sync for Ptr {}
    static OBJECT: std::sync::OnceLock<Ptr> = std::sync::OnceLock::new();
    OBJECT
        .get_or_init(|| {
            Ptr(Box::into_raw(Box::new(DropTargetObject { vtbl: &DROP_TARGET_VTBL, refs: AtomicU32::new(1) })) as *mut c_void)
        })
        .0
}

const IID_IUNKNOWN: ::windows::core::GUID = ::windows::core::GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IDROPTARGET: ::windows::core::GUID = ::windows::core::GUID::from_u128(0x00000122_0000_0000_c000_000000000046);

unsafe extern "system" fn dt_query_interface(this: *mut c_void, iid: *const ::windows::core::GUID, out: *mut *mut c_void) -> ::windows::core::HRESULT {
    use ::windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER, S_OK};
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { *iid };
    if iid == IID_IUNKNOWN || iid == IID_IDROPTARGET {
        unsafe {
            dt_add_ref(this);
            *out = this;
        }
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "system" fn dt_add_ref(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const DropTargetObject) };
    obj.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn dt_release(this: *mut c_void) -> u32 {
    let obj = unsafe { &*(this as *const DropTargetObject) };
    obj.refs.fetch_sub(1, Ordering::Relaxed).saturating_sub(1).max(1)
}

/// The CF_HDROP file list of an IDataObject, if it carries one.
unsafe fn dropped_files(data: *mut c_void) -> Vec<std::path::PathBuf> {
    use ::windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    use std::os::windows::ffi::OsStringExt;
    if data.is_null() {
        return Vec::new();
    }
    type GetData = unsafe extern "system" fn(*mut c_void, *const ffi::FormatEtc, *mut ffi::StgMedium) -> ::windows::core::HRESULT;
    let get_data: GetData = unsafe {
        let vtbl = *(data as *const *const usize);
        std::mem::transmute::<usize, GetData>(*vtbl.add(3)) // IUnknown (3), then GetData
    };
    let format = ffi::FormatEtc {
        cfFormat: 15, // CF_HDROP
        ptd: std::ptr::null_mut(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: 1, // TYMED_HGLOBAL
    };
    let mut medium = ffi::StgMedium { tymed: 0, data: std::ptr::null_mut(), pUnkForRelease: std::ptr::null_mut() };
    if unsafe { get_data(data, &format, &mut medium) }.is_err() || medium.data.is_null() {
        return Vec::new();
    }
    let hdrop = HDROP(medium.data);
    let mut paths = Vec::new();
    unsafe {
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            paths.push(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..len])));
        }
        ffi::ReleaseStgMedium(&mut medium);
    }
    paths
}

/// Screen point -> physical px relative to the island window (what wry reports).
fn island_point(pt: ::windows::Win32::Foundation::POINTL) -> (f64, f64) {
    let island = OVERLAY.with(|o| o.borrow().as_ref().map(|o| o.island));
    let mut p = POINT { x: pt.x, y: pt.y };
    if let Some(island) = island {
        let _ = unsafe { ffi::ScreenToClient(island, &mut p) };
    }
    (p.x as f64, p.y as f64)
}

unsafe extern "system" fn dt_drag_enter(_this: *mut c_void, data: *mut c_void, _keys: u32, pt: ::windows::Win32::Foundation::POINTL, effect: *mut u32) -> ::windows::core::HRESULT {
    let paths = unsafe { dropped_files(data) };
    let (x, y) = island_point(pt);
    let valid = !paths.is_empty();
    let app = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            o.valid = valid;
            o.overs_logged = 0;
            if valid {
                o.logic.enter(overlay_now());
            }
            o.app.clone()
        })
    });
    if !effect.is_null() {
        unsafe { *effect = if valid { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
    }
    crate::log::line(format!("drag-overlay enter files={} pos=({x:.0}, {y:.0})", paths.len()));
    if let (true, Some(app)) = (valid, app) {
        emit_drag(&app, "tauri://drag-enter", DragPayload::Files { paths, x, y });
    }
    ::windows::Win32::Foundation::S_OK
}

unsafe extern "system" fn dt_drag_over(_this: *mut c_void, _keys: u32, pt: ::windows::Win32::Foundation::POINTL, effect: *mut u32) -> ::windows::core::HRESULT {
    let (x, y) = island_point(pt);
    let state = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            o.logic.over(overlay_now());
            o.overs_logged += 1;
            (o.valid, o.overs_logged, o.app.clone())
        })
    });
    let valid = matches!(state, Some((true, _, _)));
    if !effect.is_null() {
        unsafe { *effect = if valid { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
    }
    if let Some((true, n, app)) = state {
        // The first few only: OLE calls DragOver continuously.
        if n <= 3 {
            crate::log::line(format!("drag-overlay over pos=({x:.0}, {y:.0})"));
        }
        emit_drag(&app, "tauri://drag-over", DragPayload::Position { x, y });
    }
    ::windows::Win32::Foundation::S_OK
}

unsafe extern "system" fn dt_drag_leave(_this: *mut c_void) -> ::windows::core::HRESULT {
    let state = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            let was_valid = std::mem::replace(&mut o.valid, false);
            (was_valid, o.logic.end(OverlayHide::Left), o.app.clone())
        })
    });
    crate::log::line("drag-overlay leave".to_string());
    if let Some((was_valid, ended, app)) = state {
        if was_valid {
            emit_drag(&app, "tauri://drag-leave", DragPayload::None);
        }
        if let Some((why, entered)) = ended {
            overlay_hide(why, entered && !was_valid);
        }
    }
    ::windows::Win32::Foundation::S_OK
}

unsafe extern "system" fn dt_drop(_this: *mut c_void, data: *mut c_void, _keys: u32, pt: ::windows::Win32::Foundation::POINTL, effect: *mut u32) -> ::windows::core::HRESULT {
    let paths = unsafe { dropped_files(data) };
    let (x, y) = island_point(pt);
    let state = OVERLAY.with(|o| {
        o.borrow_mut().as_mut().map(|o| {
            o.valid = false;
            (o.logic.end(OverlayHide::Dropped), o.app.clone())
        })
    });
    if !effect.is_null() {
        unsafe { *effect = if paths.is_empty() { DROPEFFECT_NONE } else { DROPEFFECT_COPY } };
    }
    crate::log::line(format!("drag-overlay drop files={} pos=({x:.0}, {y:.0})", paths.len()));
    if let Some((ended, app)) = state {
        // Even if the overlay had already timed out: a drop that reached us
        // carries files the owner meant to give.
        if !paths.is_empty() {
            emit_drag(&app, "tauri://drag-drop", DragPayload::Files { paths, x, y });
        }
        if let Some((why, _)) = ended {
            overlay_hide(why, false);
        }
    }
    ::windows::Win32::Foundation::S_OK
}

#[cfg(test)]
mod overlay_tests {
    use super::*;

    const AT: (i32, i32) = (100, 10);

    fn shown(now: u64) -> OverlayLogic {
        let mut l = OverlayLogic::default();
        l.show(now);
        l
    }

    #[test]
    fn hidden_overlay_never_decides() {
        let mut l = OverlayLogic::default();
        assert_eq!(l.tick(0, false, true, AT), None);
        assert_eq!(l.end(OverlayHide::Dropped), None);
        assert!(!l.visible());
    }

    #[test]
    fn drop_ends_it_without_extra_leave() {
        let mut l = shown(0);
        l.enter(10);
        l.over(20);
        assert_eq!(l.end(OverlayHide::Dropped), Some((OverlayHide::Dropped, true)));
        assert!(!l.visible());
        assert_eq!(l.tick(30, false, false, AT), None);
    }

    #[test]
    fn escape_ends_it_and_owes_leave_when_entered() {
        let mut l = shown(0);
        l.enter(10);
        assert_eq!(l.tick(20, true, true, AT), Some((OverlayHide::Escape, true)));
    }

    #[test]
    fn button_up_without_enter_ends_at_once() {
        let mut l = shown(0);
        assert_eq!(l.tick(50, true, false, AT), None);
        assert_eq!(l.tick(100, false, false, AT), Some((OverlayHide::ButtonUp, false)));
    }

    #[test]
    fn button_up_after_enter_waits_for_drop() {
        let mut l = shown(0);
        l.enter(10);
        assert_eq!(l.tick(100, false, false, AT), None);
        assert_eq!(l.tick(100 + OVERLAY_DROP_GRACE_MS - 1, false, false, AT), None);
        assert_eq!(
            l.tick(100 + OVERLAY_DROP_GRACE_MS, false, false, AT),
            Some((OverlayHide::ButtonUp, true))
        );
    }

    #[test]
    fn button_down_again_cancels_grace() {
        let mut l = shown(0);
        l.enter(10);
        assert_eq!(l.tick(100, false, false, AT), None);
        assert_eq!(l.tick(150, true, false, (101, 10)), None);
        assert_eq!(l.tick(1000, true, false, (102, 10)), None);
    }

    #[test]
    fn watchdog_after_three_quiet_seconds() {
        let mut l = shown(0);
        l.enter(0);
        assert_eq!(l.tick(50, true, false, AT), None);
        assert_eq!(l.tick(OVERLAY_WATCHDOG_MS - 1, true, false, AT), None);
        assert_eq!(l.tick(OVERLAY_WATCHDOG_MS + 50, true, false, AT), Some((OverlayHide::Watchdog, true)));
    }

    #[test]
    fn movement_or_dragover_feeds_the_watchdog() {
        let mut l = shown(0);
        assert_eq!(l.tick(2500, true, false, (1, 1)), None); // moved
        assert_eq!(l.tick(5000, true, false, (2, 2)), None); // moved
        l.over(7000);
        assert_eq!(l.tick(9000, true, false, (2, 2)), None);
        assert_eq!(l.tick(10_000, true, false, (2, 2)), Some((OverlayHide::Watchdog, false)));
    }

    #[test]
    fn leave_then_show_again() {
        let mut l = shown(0);
        l.enter(5);
        assert_eq!(l.end(OverlayHide::Left), Some((OverlayHide::Left, true)));
        assert!(!l.entered());
        l.show(100);
        assert!(l.visible() && !l.entered());
    }

    #[test]
    fn only_leave_lets_the_same_press_show_it_again() {
        assert!(!OverlayHide::Left.blocks_press());
        for why in [OverlayHide::Dropped, OverlayHide::Escape, OverlayHide::ButtonUp, OverlayHide::Watchdog, OverlayHide::Mouse] {
            assert!(why.blocks_press(), "{why:?}");
        }
    }

    #[test]
    fn enter_on_hidden_overlay_is_ignored() {
        let mut l = OverlayLogic::default();
        l.enter(0);
        assert!(!l.entered());
    }

    #[test]
    fn drag_payload_matches_tauri_shape() {
        let files = DragPayload::Files { paths: vec!["C:\\a.txt".into()], x: 3.0, y: 4.0 };
        assert_eq!(serde_json::to_string(&files).unwrap(), r#"{"paths":["C:\\a.txt"],"position":{"x":3.0,"y":4.0}}"#);
        let over = DragPayload::Position { x: 1.5, y: 2.0 };
        assert_eq!(serde_json::to_string(&over).unwrap(), r#"{"position":{"x":1.5,"y":2.0}}"#);
        assert_eq!(serde_json::to_string(&DragPayload::None).unwrap(), "null");
    }
}
