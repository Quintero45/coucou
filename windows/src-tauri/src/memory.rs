// The assistant's memory, in %APPDATA%\ARIA\memory\: notes it was asked to
// keep (notes.md, read into every conversation) and a history of finished
// turns (history.jsonl). Plain files on this machine; nothing leaves it.

use std::io::Write;
use std::path::PathBuf;

use serde_json::json;

use crate::{platform, settings};

const MAX_NOTES_IN_PROMPT: usize = 6_000;
const MAX_HISTORY_BYTES: u64 = 2_000_000;

pub fn dir() -> PathBuf {
    settings::config_dir().join("memory")
}

fn notes_path() -> PathBuf {
    dir().join("notes.md")
}

/// Creates the memory folder and an empty notes.md on first run, so the folder
/// is there before ARIA has anything to remember.
pub fn ensure() {
    if platform::ensure_private_dir(&dir()).is_err() {
        return;
    }
    let path = notes_path();
    if !path.exists() {
        let _ = std::fs::write(&path, "# Memoria de ARIA\n");
    }
}

/// What goes into the system prompt: the newest notes, capped.
pub fn notes() -> String {
    let text = std::fs::read_to_string(notes_path()).unwrap_or_default();
    if text.len() <= MAX_NOTES_IN_PROMPT {
        return text;
    }
    let mut start = text.len() - MAX_NOTES_IN_PROMPT;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

pub fn remember(note: &str) -> Result<(), String> {
    let note = note.trim();
    if note.is_empty() {
        return Err("empty note".into());
    }
    platform::ensure_private_dir(&dir()).map_err(|e| e.to_string())?;
    let t = platform::local_time();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(notes_path())
        .map_err(|e| e.to_string())?;
    writeln!(file, "- [{:04}-{:02}-{:02}] {}", t.year, t.month, t.day, note.replace('\n', " "))
        .map_err(|e| e.to_string())
}

/// Removes every note line containing `text`. Returns how many went.
pub fn forget(text: &str) -> Result<usize, String> {
    let needle = text.trim().to_lowercase();
    if needle.is_empty() {
        return Err("nothing to forget".into());
    }
    let current = std::fs::read_to_string(notes_path()).unwrap_or_default();
    let kept: Vec<&str> = current.lines().filter(|l| !l.to_lowercase().contains(&needle)).collect();
    let removed = current.lines().count() - kept.len();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(notes_path(), out).map_err(|e| e.to_string())?;
    Ok(removed)
}

/// One finished exchange, appended to history.jsonl.
pub fn record(provider: &str, model: &str, user: &str, assistant: &str) {
    if platform::ensure_private_dir(&dir()).is_err() {
        return;
    }
    let path = dir().join("history.jsonl");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_HISTORY_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir().join("history.prev.jsonl"));
    }
    let t = platform::local_time();
    let line = json!({
        "at": format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute),
        "provider": provider,
        "model": model,
        "user": user,
        "assistant": assistant,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}
