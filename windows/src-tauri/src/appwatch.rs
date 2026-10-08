// Coding agents that live in an app of their own. Cursor fires no hook when
// it is opened (its sessionStart only comes with the first agent chat), so the
// island would stay quiet while Claude Code, launched from a terminal, shows up
// at once. Here a WinEvent hook (EVENT_SYSTEM_FOREGROUND, out of context: no
// DLL injected, no polling, nothing runs until a window comes to the front)
// spots the first time each Cursor process owns the foreground window and
// tells the island with `agent-app-opened`, which it treats as that agent's
// SessionStart.

#[cfg(windows)]
pub use self::win::start;
#[cfg(windows)]
pub(crate) use self::win::exe_name;

#[cfg(not(windows))]
pub fn start(_app: tauri::AppHandle) {}

/// Executable name (lower case) → coucou_agent name.
#[cfg_attr(not(windows), allow(dead_code))]
const APPS: &[(&str, &str)] = &[("cursor.exe", "cursor")];

#[cfg_attr(not(windows), allow(dead_code))]
fn agent_for(exe: &str) -> Option<&'static str> {
    let exe = exe.to_ascii_lowercase();
    APPS.iter().find(|(name, _)| *name == exe).map(|(_, agent)| *agent)
}

#[cfg(windows)]
mod win {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    use serde_json::json;
    use tauri::{AppHandle, Emitter};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, GetWindowThreadProcessId, EVENT_SYSTEM_FOREGROUND, MSG,
        WINEVENT_OUTOFCONTEXT,
    };

    use crate::island::WINDOW_LABEL;
    use crate::log;

    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// Processes already announced: one SessionStart per launch, not per Alt+Tab.
    static SEEN: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();

    pub(crate) fn exe_name(pid: u32) -> Option<String> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
        let _ = unsafe { CloseHandle(handle) };
        ok.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().map(str::to_string)
    }

    unsafe extern "system" fn on_foreground(
        _hook: HWINEVENTHOOK,
        _event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 || pid == std::process::id() {
            return;
        }
        let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
        if seen.lock().unwrap().contains(&pid) {
            return;
        }
        let Some(agent) = exe_name(pid).as_deref().and_then(super::agent_for) else { return };
        seen.lock().unwrap().insert(pid);
        log::line(format!("appwatch: {agent} came to the front (pid {pid})"));
        if let Some(app) = APP.get() {
            let _ = app.emit_to(WINDOW_LABEL, "agent-app-opened", json!({ "agent": agent }));
        }
    }

    pub fn start(app: AppHandle) {
        if APP.set(app).is_err() {
            return;
        }
        let _ = std::thread::Builder::new().name("appwatch".into()).spawn(|| unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.is_invalid() {
                log::line("appwatch: could not watch the foreground window");
                return;
            }
            // An out-of-context WinEvent hook is delivered through this thread's queue.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn cursor_is_recognised() {
        assert_eq!(super::agent_for("Cursor.exe"), Some("cursor"));
        assert_eq!(super::agent_for("Code.exe"), None);
    }
}
