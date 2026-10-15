// Flow dictation is Windows-only in ARIA for now: elsewhere the key is never
// watched and nothing is pasted.

use std::sync::mpsc::Sender;
use std::time::Instant;

use super::logic::FlowKey;
use super::KeyEvent;

pub struct KeyListener;

pub fn listen(_key: FlowKey, _tx: Sender<(KeyEvent, Instant)>) -> Result<KeyListener, String> {
    Err("not on this system".into())
}

pub fn wait_key_released(_key: FlowKey) {}

pub fn send_paste(_shift: bool) -> Result<(), String> {
    Err("not on this system".into())
}

pub fn foreground_app() -> Option<(String, String)> {
    None
}

pub fn running_apps() -> Vec<(String, String)> {
    Vec::new()
}

pub fn keys_supported() -> Result<(), &'static str> {
    Err("unsupported")
}
