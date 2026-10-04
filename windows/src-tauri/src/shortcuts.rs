// Global keyboard shortcuts — the Windows side of ShortcutLogic.swift.
//
// They only open, show or move between things. None of them answers a
// permission request: approving still takes a click (or Y / N with the card
// focused), as the project rules require.

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::island::WINDOW_LABEL;
use crate::{log, Shared};

/// (key, action sent to the island). All on Ctrl+Alt, like the macOS ⌃⌥ set.
const BINDINGS: &[(Code, &str)] = &[
    (Code::Space, "chat"),
    (Code::KeyA, "alert"),
    (Code::KeyH, "toggle"),
    (Code::KeyM, "mute"),
    (Code::BracketRight, "next-pill"),
    (Code::BracketLeft, "prev-pill"),
];

fn shortcut(code: Code) -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), code)
}

/// The plugin, with one handler that turns a key press into an island event.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, pressed, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let enabled = app.state::<Shared>().settings.lock().unwrap().shortcuts_enabled;
            if !enabled {
                return;
            }
            if let Some((_, action)) = BINDINGS.iter().find(|(code, _)| &shortcut(*code) == pressed) {
                let _ = app.emit_to(WINDOW_LABEL, "shortcut", action.to_string());
            }
        })
        .build()
}

/// Registers or releases every binding. A key another app already owns is
/// logged and skipped; the rest still work.
pub fn apply(app: &AppHandle, enabled: bool) {
    let gs = app.global_shortcut();
    for (code, action) in BINDINGS {
        let sc = shortcut(*code);
        let registered = gs.is_registered(sc);
        if enabled && !registered {
            if let Err(err) = gs.register(sc) {
                log::line(format!("shortcut {action} unavailable: {err}"));
            }
        } else if !enabled && registered {
            let _ = gs.unregister(sc);
        }
    }
}
