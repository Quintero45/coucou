//! What a Grok Bot shows on its pill besides a status: a progress step
//! (`aria-hook --step "…" --bot <Name>`) and a file card
//! (`aria-hook --attach <path> --bot <Name> [--caption "…"]`).
//!
//! Both only display something, so neither asks the owner. A file card does
//! make its file openable from the island, which is why `open_attachment` only
//! opens paths announced this session (the allowlist below), and opens
//! anything that could run code by revealing it in Explorer instead.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::island::WINDOW_LABEL;
use crate::{files, log, platform, policy};

/// Longest step / caption the island receives, in characters.
pub const STEP_MAX: usize = 200;
/// More files than this in one session and the oldest announcements are
/// forgotten wholesale; a Bot cannot grow the set without bound.
const ALLOWLIST_MAX: usize = 512;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BotStep {
    pub agent: String,
    pub bot: String,
    pub text: String,
    pub ts: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BotAttach {
    pub agent: String,
    pub bot: String,
    pub path: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// Redacted (tokens, keys, passwords), on one line, at most `max` characters.
pub fn sanitize(text: &str, max: usize) -> String {
    let redacted = policy::redact(text);
    let mut flat = String::with_capacity(redacted.len());
    for c in redacted.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' && (flat.is_empty() || flat.ends_with(' ')) {
            continue;
        }
        flat.push(c);
    }
    let flat = flat.trim_end();
    if flat.chars().count() <= max {
        return flat.to_string();
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// `aria_agent` must be this Bot's pill: a card cannot speak for Claude Code
/// or for another Bot. Same rule as pipe.rs' tool requests.
pub fn identity(payload: &Value) -> Result<(String, String), String> {
    let bot = payload.get("aria_bot").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let slug = crate::grokbot::slug(&bot);
    if slug.is_empty() {
        return Err("missing the Bot's name (--bot)".into());
    }
    let agent = format!("bot-{slug}");
    if payload.get("aria_agent").and_then(Value::as_str) != Some(agent.as_str()) {
        return Err(format!("aria_agent must be {agent}"));
    }
    Ok((agent, bot))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn parse_step(payload: &Value, ts: u64) -> Result<BotStep, String> {
    let (agent, bot) = identity(payload)?;
    let raw = payload.get("text").and_then(Value::as_str).unwrap_or_default();
    let text = sanitize(raw, STEP_MAX);
    if text.is_empty() {
        return Err("empty step".into());
    }
    Ok(BotStep { agent, bot, text, ts })
}

/// The allowlist key: canonical, and case-folded where the file system is.
fn key(path: &Path) -> Option<String> {
    let canon = std::fs::canonicalize(path).ok()?;
    let s = canon.to_string_lossy().to_string();
    Some(if cfg!(windows) { s.to_lowercase() } else { s })
}

/// An absolute path to a regular file (a symlink to one is fine; a folder, a
/// device or a relative path is not). Returns the card minus agent/bot, and
/// the allowlist key.
pub fn validate_attach(path: &str) -> Result<(String, String, String, u64, String), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("missing path".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("invalid path".into());
    }
    let p = PathBuf::from(trimmed);
    if !p.is_absolute() {
        return Err(format!("not an absolute path: {trimmed}"));
    }
    let meta = std::fs::metadata(&p).map_err(|_| format!("file not found: {trimmed}"))?;
    if !meta.is_file() {
        return Err(format!("not a regular file: {trimmed}"));
    }
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if name.is_empty() {
        return Err(format!("not a file: {trimmed}"));
    }
    let k = key(&p).ok_or_else(|| format!("cannot resolve {trimmed}"))?;
    let mime = files::guess_mime(&name).to_string();
    Ok((trimmed.to_string(), name, mime, meta.len(), k))
}

pub fn parse_attach(payload: &Value) -> Result<(BotAttach, String), String> {
    let (agent, bot) = identity(payload)?;
    let raw = payload.get("path").and_then(Value::as_str).unwrap_or_default();
    let (path, name, mime, size, k) = validate_attach(raw)?;
    let caption = payload
        .get("caption")
        .and_then(Value::as_str)
        .map(|c| sanitize(c, STEP_MAX))
        .filter(|c| !c.is_empty());
    Ok((BotAttach { agent, bot, path, name, mime, size, caption }, k))
}

/// Paths a Bot announced with `--attach` this session.
#[derive(Default)]
pub struct Allowlist(HashSet<String>);

impl Allowlist {
    pub fn add(&mut self, key: String) {
        if self.0.len() >= ALLOWLIST_MAX && !self.0.contains(&key) {
            self.0.clear();
        }
        self.0.insert(key);
    }
    pub fn contains(&self, key: &str) -> bool {
        self.0.contains(key)
    }
}

static ALLOWED: LazyLock<Mutex<Allowlist>> = LazyLock::new(|| Mutex::new(Allowlist::default()));

/// Extensions that run something when "opened": those cards reveal the file
/// in its folder instead.
const RUNNABLE: &[&str] = &[
    "exe", "com", "bat", "cmd", "ps1", "psm1", "psd1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi", "msp",
    "msc", "lnk", "url", "scr", "pif", "hta", "cpl", "reg", "jar", "appx", "msix", "appref-ms", "application",
    "gadget", "inf", "sct", "settingcontent-ms", "library-ms", "dll", "sys", "ocx", "chm", "xll", "website",
];

#[derive(Debug, PartialEq)]
pub enum OpenAction {
    Open,
    Reveal,
}

pub fn open_action(path: &Path) -> OpenAction {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if RUNNABLE.contains(&ext.as_str()) {
        OpenAction::Reveal
    } else {
        OpenAction::Open
    }
}

/// From pipe.rs: `aria_kind: "bot_step"`. Fire and forget.
pub fn step(app: &AppHandle, payload: &Value) {
    match parse_step(payload, now_ms()) {
        Ok(step) => {
            log::line(format!("bot-step [{}] {}", step.bot, step.text));
            let _ = app.emit_to(WINDOW_LABEL, "bot-step", step);
        }
        Err(e) => log::line(format!("bot-step refused: {e}")),
    }
}

/// From pipe.rs: `aria_kind: "bot_attach"`. Answers `{ok}` or `{ok:false,error}`.
pub fn attach(app: &AppHandle, payload: &Value) -> Value {
    match parse_attach(payload) {
        Ok((card, k)) => {
            log::line(format!(
                "bot-attach [{}] {} ({}, {} bytes) {}",
                card.bot, card.name, card.mime, card.size, card.path
            ));
            ALLOWED.lock().unwrap().add(k);
            let _ = app.emit_to(WINDOW_LABEL, "bot-attach", card);
            json!({ "ok": true })
        }
        Err(e) => {
            log::line(format!("bot-attach refused: {e}"));
            json!({ "ok": false, "error": e })
        }
    }
}

/// For other modules (meeting.rs' transcript, …): the same as a Bot's
/// `--attach` — validates the file, adds it to `open_attachment`'s allowlist,
/// emits `bot-attach` to the island and logs it. `bot` is the Bot's display
/// name (its pill is `bot-<slug>`).
#[allow(dead_code)] // meeting.rs moves to it from `attach` (its TODO)
pub fn announce_attachment(app: &AppHandle, bot: &str, path: &Path, caption: Option<&str>) -> Result<(), String> {
    let answer = attach(app, &attach_payload(bot, path, caption));
    if answer["ok"] == json!(true) {
        Ok(())
    } else {
        Err(answer["error"].as_str().unwrap_or("not shown").to_string())
    }
}

fn attach_payload(bot: &str, path: &Path, caption: Option<&str>) -> Value {
    let mut v = json!({
        "aria_agent": format!("bot-{}", crate::grokbot::slug(bot)),
        "aria_bot": bot,
        "path": path.to_string_lossy(),
    });
    if let Some(c) = caption {
        v["caption"] = json!(c);
    }
    v
}

/// Click on a file card. Only paths announced through `bot-attach` this
/// session; anything runnable is shown in its folder rather than started.
#[tauri::command]
pub fn open_attachment(path: String) -> Result<(), String> {
    let p = PathBuf::from(path.trim());
    let allowed = p.is_absolute() && key(&p).map(|k| ALLOWED.lock().unwrap().contains(&k)).unwrap_or(false);
    if !allowed {
        log::line(format!("open_attachment refused (not announced this session): {path}"));
        return Err("this file was not shared by a Bot in this session".into());
    }
    match open_action(&p) {
        OpenAction::Open => {
            log::line(format!("open_attachment opened {path}"));
            platform::shell_open(&p.to_string_lossy(), None)
        }
        OpenAction::Reveal => {
            log::line(format!("open_attachment revealed (runnable) {path}"));
            let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            platform::reveal_folder(&dir);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aria-botcards-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn steps_are_flattened_and_capped() {
        assert_eq!(sanitize("  Leyendo\n\tcorreos\r\n  nuevos ", STEP_MAX), "Leyendo correos nuevos");
        let long = "a".repeat(500);
        let s = sanitize(&long, STEP_MAX);
        assert_eq!(s.chars().count(), STEP_MAX);
        assert!(s.ends_with('…'));
        assert_eq!(sanitize("\u{7}\u{1b}", STEP_MAX), "");
    }

    #[test]
    fn steps_are_redacted() {
        let s = sanitize("llamando con Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456", STEP_MAX);
        assert!(!s.contains("abcdefghijklmnopqrstuvwxyz123456"), "{s}");
    }

    #[test]
    fn steps_need_the_bots_own_pill() {
        let ok = json!({"aria_agent":"bot-ventas","aria_bot":" Ventas ","text":"Paso 1"});
        let s = parse_step(&ok, 42).unwrap();
        assert_eq!(s, BotStep { agent: "bot-ventas".into(), bot: "Ventas".into(), text: "Paso 1".into(), ts: 42 });
        assert!(parse_step(&json!({"aria_agent":"claude","aria_bot":"Ventas","text":"x"}), 0).is_err());
        assert!(parse_step(&json!({"aria_agent":"bot-","aria_bot":"","text":"x"}), 0).is_err());
        assert!(parse_step(&json!({"aria_agent":"bot-ventas","aria_bot":"Ventas","text":" \n "}), 0).is_err());
    }

    #[test]
    fn attach_validation() {
        let f = tmp("informe.pdf", b"%PDF-1.4");
        let (path, name, mime, size, _) = validate_attach(&f.to_string_lossy()).unwrap();
        assert_eq!(path, f.to_string_lossy());
        assert_eq!(name, "informe.pdf");
        assert_eq!(mime, "application/pdf");
        assert_eq!(size, 8);
        assert!(validate_attach("informe.pdf").is_err(), "relative");
        assert!(validate_attach("").is_err());
        let dir = f.parent().unwrap().to_string_lossy().to_string();
        assert!(validate_attach(&dir).is_err(), "a folder");
        assert!(validate_attach(&format!("{dir}/no-such-file.txt")).is_err());
        let payload = json!({"aria_agent":"bot-a","aria_bot":"A","path": f.to_string_lossy(),"caption":"  el\ninforme "});
        let (card, _) = parse_attach(&payload).unwrap();
        assert_eq!(card.caption.as_deref(), Some("el informe"));
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["agent"], "bot-a");
        let no_caption = json!({"aria_agent":"bot-a","aria_bot":"A","path": f.to_string_lossy()});
        let v = serde_json::to_value(parse_attach(&no_caption).unwrap().0).unwrap();
        assert!(v.get("caption").is_none());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn announcements_from_other_modules_use_the_bots_pill() {
        let f = tmp("transcripcion.txt", b"hola");
        let (card, _) = parse_attach(&attach_payload("Asistente de Ventas", &f, Some("Transcripción"))).unwrap();
        assert_eq!(card.agent, "bot-asistente-de-ventas");
        assert_eq!(card.caption.as_deref(), Some("Transcripción"));
        assert!(attach_payload("V", &f, None).get("caption").is_none());
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn allowlist_matches_canonical_paths_only() {
        let f = tmp("Nota.TXT", b"hola");
        let (_, _, _, _, k) = validate_attach(&f.to_string_lossy()).unwrap();
        let mut list = Allowlist::default();
        assert!(!list.contains(&k));
        list.add(k.clone());
        assert!(list.contains(&k));
        // Same file through a `..` detour resolves to the same key.
        let detour = f.parent().unwrap().join("..").join(f.parent().unwrap().file_name().unwrap()).join("Nota.TXT");
        assert_eq!(key(&detour).unwrap(), k);
        let other = tmp("otra.txt", b"x");
        assert!(!list.contains(&key(&other).unwrap()));
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_file(&other);
    }

    #[test]
    fn allowlist_is_bounded() {
        let mut list = Allowlist::default();
        for i in 0..ALLOWLIST_MAX + 3 {
            list.add(format!("k{i}"));
        }
        assert!(list.0.len() <= ALLOWLIST_MAX);
        assert!(list.contains(&format!("k{}", ALLOWLIST_MAX + 2)));
    }

    #[test]
    fn runnable_files_are_revealed_not_opened() {
        assert_eq!(open_action(Path::new("C:/x/informe.pdf")), OpenAction::Open);
        assert_eq!(open_action(Path::new("C:/x/foto.PNG")), OpenAction::Open);
        assert_eq!(open_action(Path::new("C:/x/setup.EXE")), OpenAction::Reveal);
        assert_eq!(open_action(Path::new("C:/x/script.ps1")), OpenAction::Reveal);
        assert_eq!(open_action(Path::new("C:/x/acceso.lnk")), OpenAction::Reveal);
        assert_eq!(open_action(Path::new("C:/x/sin-extension")), OpenAction::Open);
    }

    #[test]
    fn unannounced_paths_are_refused() {
        let f = tmp("nunca-anunciado.txt", b"x");
        assert!(open_attachment(f.to_string_lossy().to_string()).is_err());
        assert!(open_attachment("relativo.txt".into()).is_err());
        let _ = std::fs::remove_file(&f);
    }
}
