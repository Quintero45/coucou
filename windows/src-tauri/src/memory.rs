// The assistant's memory, in %APPDATA%\ARIA\memory\: notes it was asked to
// keep (notes.md, read into every conversation) and a history of finished
// turns (history.jsonl). Plain files on this machine; nothing leaves it.

use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

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
        "user": crate::policy::redact(user),
        "assistant": crate::policy::redact(assistant),
    });
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

// ── Reading the history back ──────────────────────────────────────────────────

/// Past this, a remembered message is cut: the prompt stays small.
const RECENT_USER_CHARS: usize = 300;
const RECENT_REPLY_CHARS: usize = 600;
const RECALL_CHARS: usize = 1_200;
const RECALL_HITS: usize = 8;

/// Both files, oldest exchange first (history.prev.jsonl is the rotated one).
fn history_text() -> String {
    let mut text = std::fs::read_to_string(dir().join("history.prev.jsonl")).unwrap_or_default();
    text.push_str(&std::fs::read_to_string(dir().join("history.jsonl")).unwrap_or_default());
    text
}

fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

fn exchanges(text: &str) -> Vec<Value> {
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect()
}

fn field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The last `n` exchanges, oldest first, for the system prompt: what was being
/// talked about before the app restarted or the chat was reset.
fn recent_from(text: &str, n: usize) -> String {
    let all = exchanges(text);
    let from = all.len().saturating_sub(n);
    all[from..]
        .iter()
        .map(|v| {
            format!(
                "[{}] Owner: {}\nYou: {}",
                field(v, "at"),
                cut(field(v, "user"), RECENT_USER_CHARS),
                cut(field(v, "assistant"), RECENT_REPLY_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn recent(n: usize) -> String {
    recent_from(&history_text(), n)
}

/// Exchanges containing every word of `query` (any case), newest first.
fn recall_from(text: &str, query: &str) -> String {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return "Give some words to look for.".into();
    }
    let hits: Vec<String> = exchanges(text)
        .iter()
        .rev()
        .filter(|v| {
            let hay = format!("{}\n{}", field(v, "user"), field(v, "assistant")).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .take(RECALL_HITS)
        .map(|v| {
            format!(
                "[{}] Owner: {}\nYou: {}",
                field(v, "at"),
                cut(field(v, "user"), RECALL_CHARS / 3),
                cut(field(v, "assistant"), RECALL_CHARS)
            )
        })
        .collect();
    if hits.is_empty() {
        format!("Nothing in past conversations mentions \"{}\".", query.trim())
    } else {
        hits.join("\n\n")
    }
}

/// The `recall` tool: past conversations that mention these words.
pub fn recall(query: &str) -> String {
    recall_from(&history_text(), query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at: &str, user: &str, assistant: &str) -> String {
        json!({ "at": at, "provider": "p", "model": "m", "user": user, "assistant": assistant }).to_string()
    }

    #[test]
    fn recent_keeps_the_last_exchanges_in_order() {
        let text = [line("1", "a", "A"), line("2", "b", "B"), "not json".into(), line("3", "c", "C")].join("\n");
        assert_eq!(recent_from(&text, 2), "[2] Owner: b\nYou: B\n\n[3] Owner: c\nYou: C");
        assert_eq!(recent_from("", 4), "");
    }

    #[test]
    fn recall_finds_every_word_newest_first() {
        let text = [
            line("1", "pon una canción en Spotify", "Abrí Spotify."),
            line("2", "otra cosa", "nada"),
            line("3", "Spotify otra vez", "Usé la skill spotify-play."),
        ]
        .join("\n");
        let found = recall_from(&text, "SPOTIFY");
        assert!(found.starts_with("[3]"), "{found}");
        assert!(found.contains("[1]"));
        assert!(!found.contains("[2]"));
        assert!(recall_from(&text, "spotify skill").starts_with("[3]"));
        assert!(recall_from(&text, "netflix").starts_with("Nothing"));
    }

    #[test]
    fn a_long_message_is_cut() {
        let long = "x".repeat(RECENT_REPLY_CHARS + 50);
        let shown = recent_from(&line("1", "q", &long), 1);
        assert!(shown.ends_with('…'));
        assert!(shown.chars().count() < RECENT_REPLY_CHARS + 40);
    }
}
