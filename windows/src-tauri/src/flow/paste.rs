// The words go where you type: onto the clipboard (kept out of the clipboard
// history and the cloud clipboard), Ctrl+V in the app in front, and the
// clipboard gets back the text it held 700 ms later — unless something else
// was copied meanwhile. From Coucou's flow/paste.rs, with the Win32 clipboard
// called directly instead of a clipboard crate.

use std::time::Duration;

/// What happened to the words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pasted,
    /// On the clipboard only: the keystroke could not be sent.
    Copied,
    Failed,
}

/// Copies `text` and leaves it there (the dictation could not be pasted).
pub fn copy(text: &str) -> bool {
    clip::set_text(text)
}

/// Pastes `text` in the app in front. `shift` sends Ctrl+Shift+V (terminals).
pub fn paste(text: &str, shift: bool) -> Outcome {
    let previous = clip::get_text();
    if !clip::set_text(text) {
        return Outcome::Failed;
    }
    // Give the clipboard a moment to be announced before the app reads it.
    std::thread::sleep(Duration::from_millis(40));
    if let Err(err) = super::os::send_paste(shift) {
        crate::log::line(format!("flow: paste keystroke failed: {err}"));
        return Outcome::Copied;
    }
    let ours = text.to_string();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let Some(previous) = previous else { return };
        // Something else was copied meanwhile: leave it.
        if clip::get_text().as_deref() == Some(ours.as_str()) {
            clip::set_text(&previous);
        }
    });
    Outcome::Pasted
}

#[cfg(windows)]
mod clip {
    use std::ffi::c_void;

    type Handle = isize;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(owner: isize) -> i32;
        fn CloseClipboard() -> i32;
        fn EmptyClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> Handle;
        fn SetClipboardData(format: u32, mem: Handle) -> Handle;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
        fn GlobalFree(mem: Handle) -> Handle;
        fn GlobalLock(mem: Handle) -> *mut c_void;
        fn GlobalUnlock(mem: Handle) -> i32;
        fn GlobalSize(mem: Handle) -> usize;
    }

    const CF_UNICODETEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x0002;
    /// Never read more than this back (a runaway clipboard would mean gigabytes).
    const MAX_CHARS: usize = 4_000_000;

    /// Another app may hold the clipboard for a moment.
    fn open() -> bool {
        for _ in 0..10 {
            if unsafe { OpenClipboard(0) } != 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        false
    }

    fn global(bytes: &[u8]) -> Option<Handle> {
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if mem == 0 {
                return None;
            }
            let p = GlobalLock(mem) as *mut u8;
            if p.is_null() {
                GlobalFree(mem);
                return None;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            GlobalUnlock(mem);
            Some(mem)
        }
    }

    /// Hands `bytes` to the clipboard under `format`; the clipboard owns it after.
    fn put(format: u32, bytes: &[u8]) -> bool {
        let Some(mem) = global(bytes) else { return false };
        if unsafe { SetClipboardData(format, mem) } == 0 {
            unsafe { GlobalFree(mem) };
            return false;
        }
        true
    }

    fn format(name: &str) -> u32 {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    }

    pub fn set_text(text: &str) -> bool {
        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let bytes: Vec<u8> = wide.iter().flat_map(|c| c.to_le_bytes()).collect();
        if !open() {
            return false;
        }
        let ok = unsafe { EmptyClipboard() } != 0 && put(CF_UNICODETEXT, &bytes);
        if ok {
            // Kept out of Win+V's history, the cloud clipboard and clipboard managers.
            let zero = 0u32.to_le_bytes();
            put(format("CanIncludeInClipboardHistory"), &zero);
            put(format("CanUploadToCloudClipboard"), &zero);
            put(format("ExcludeClipboardContentFromMonitorProcessing"), &zero);
        }
        unsafe { CloseClipboard() };
        ok
    }

    pub fn get_text() -> Option<String> {
        if !open() {
            return None;
        }
        let mut text = None;
        unsafe {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 {
                let h = GetClipboardData(CF_UNICODETEXT);
                if h != 0 {
                    let p = GlobalLock(h) as *const u16;
                    if !p.is_null() {
                        let max = (GlobalSize(h) / 2).min(MAX_CHARS);
                        let slice = std::slice::from_raw_parts(p, max);
                        let end = slice.iter().position(|&c| c == 0).unwrap_or(max);
                        text = Some(String::from_utf16_lossy(&slice[..end]));
                        GlobalUnlock(h);
                    }
                }
            }
            CloseClipboard();
        }
        text
    }
}

#[cfg(not(windows))]
mod clip {
    pub fn set_text(_text: &str) -> bool {
        false
    }

    pub fn get_text() -> Option<String> {
        None
    }
}
