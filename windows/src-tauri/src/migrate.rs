// Coucou → ARIA, once. The first launch under the new name copies what the old
// app kept: preferences, memory, skills, the inbox, recaps, voice models, the
// Cursor bridge, the webview's storage (Grok Bot chats) and the keys in the
// Credential Manager. Nothing is moved or deleted: Coucou keeps working until
// the owner uninstalls it, and its keys go only on a click in Settings.
//
// Agents' hooks are not touched here: they are somebody else's config, so they
// move to ARIA's relay through the usual diff and click (Settings → Agents).

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{agents, hooks, i18n, log, platform, secrets};

/// The identifier ARIA had as Coucou: its webview data sits under it.
pub const LEGACY_IDENTIFIER: &str = "fr.louisraille.coucou";
/// ARIA's own (tauri.conf.json).
pub const IDENTIFIER: &str = "dev.miller.aria";
/// Written in ARIA's preferences folder once the copy is done.
const MARK: &str = "migrated-from-coucou.json";
/// Past this a file in `LINKED` is hard-linked rather than copied: nothing
/// rewrites those in place, and a link costs no disk space. Everything else
/// (history, recaps, the webview's databases) changes in place, so is copied.
const LINK_ABOVE: u64 = 1 << 20;
/// Folders under the local folder: voice models and the Cursor bridge's packages.
const LINKED: [&str; 2] = ["voice", "cursor-sdk-bridge"];

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Report {
    pub at: String,
    pub files: usize,
    /// Names of the keys copied, never their values.
    pub keys: Vec<String>,
    pub webview: bool,
    pub problems: Vec<String>,
}

pub enum Outcome {
    /// Nothing from Coucou, or already brought over.
    NotNeeded,
    /// Copied on this launch; the report is in the mark.
    Done,
    /// Coucou is still open and the owner chose not to close it: ARIA does not
    /// start, so its webview cannot be created before Coucou's is copied.
    Postponed,
}

/// Before settings are read and before any window exists.
pub fn run() -> Outcome {
    let (old_config, old_local) = (platform::legacy_config_dir(), platform::legacy_local_dir());
    let (config, local) = (platform::config_dir(), platform::local_dir());
    if config.join(MARK).exists() || (!old_config.is_dir() && !old_local.is_dir()) {
        return Outcome::NotNeeded;
    }
    if let Some(lang) = read_json(&old_config.join("settings.json"))
        .and_then(|v| v.get("language")?.as_str().map(String::from))
    {
        i18n::set_picked(&lang);
    }
    while platform::legacy_app_running() {
        let text = i18n::t("Coucou is still open. Close it so ARIA can bring over your settings, memory, chats and keys, then press Retry. Cancel closes ARIA; it will ask again next time.");
        if !platform::ask_to_close_legacy_app(&text) {
            log::line("migration from Coucou postponed: Coucou is still open");
            return Outcome::Postponed;
        }
    }

    let mut report = Report::default();
    for dir in [&config, &local] {
        if let Err(err) = platform::ensure_private_dir(dir) {
            report.problems.push(format!("{}: {err}", dir.display()));
        }
    }
    copy_tree(&old_config, &config, &|_| false, &|_| false, &mut report);
    retitle_notes(&config.join("memory").join("notes.md"));
    // The old relay, the old program and the old log stay with Coucou.
    let skip_local = |path: &Path| {
        path.parent() == Some(old_local.as_path())
            && path.file_name().map(|n| n.to_string_lossy().to_lowercase()).is_some_and(|name| {
                name == "bin" || name.ends_with(".exe") || name.starts_with("coucou.log")
            })
    };
    let link_local = |path: &Path| LINKED.iter().any(|dir| path.starts_with(old_local.join(dir)));
    copy_tree(&old_local, &local, &skip_local, &link_local, &mut report);

    let (old_webview, webview) = (platform::webview_dir(LEGACY_IDENTIFIER), platform::webview_dir(IDENTIFIER));
    if old_webview.is_dir() && !webview.exists() {
        let before = report.files;
        copy_tree(&old_webview, &webview, &|_| false, &|_| false, &mut report);
        report.webview = report.files > before;
    }

    for key in key_names(&config) {
        match secrets::copy_from_legacy(&key) {
            Ok(true) => report.keys.push(key),
            Ok(false) => {}
            Err(err) => report.problems.push(format!("key {key}: {err}")),
        }
    }

    let t = platform::local_time();
    report.at = format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute);
    log::line(format!(
        "migrated from Coucou: {} files, {} keys, webview storage {}, {} problems",
        report.files,
        report.keys.len(),
        if report.webview { "copied" } else { "not copied" },
        report.problems.len()
    ));
    for problem in report.problems.iter().take(20) {
        log::line(format!("migration: {problem}"));
    }
    let text = serde_json::to_string_pretty(&report).unwrap_or_default();
    if let Err(err) = std::fs::write(config.join(MARK), text) {
        log::line(format!("migration mark not written: {err}"));
    }
    Outcome::Done
}

/// Copies every file under `from` that `to` does not have yet; nothing already
/// there is ever overwritten.
fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool, link: &dyn Fn(&Path) -> bool, report: &mut Report) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    for entry in entries.flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if skip(&src) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            copy_tree(&src, &dst, skip, link, report);
        } else if kind.is_file() && !dst.exists() {
            match copy_file(&src, &dst, link(&src)) {
                Ok(()) => report.files += 1,
                Err(err) => report.problems.push(format!("{}: {err}", src.display())),
            }
        }
    }
}

fn copy_file(src: &Path, dst: &Path, link: bool) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let big = std::fs::metadata(src).is_ok_and(|m| m.len() > LINK_ABOVE);
    if link && big && std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map(|_| ())
}

/// The heading memory.rs wrote under the old name.
fn retitle_notes(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    if let Some(rest) = text.strip_prefix("# Memoria de Mochi") {
        let _ = std::fs::write(path, format!("# Memoria de ARIA{rest}"));
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Every key ARIA may hold: the fixed ones, each MCP placeholder in mcp.json
/// and each Grok Bot's webhook key.
fn key_names(config: &Path) -> Vec<String> {
    let mut keys: Vec<String> = secrets::KNOWN_KEYS.iter().map(|k| k.to_string()).collect();
    if let Ok(text) = std::fs::read_to_string(config.join("mcp.json")) {
        keys.extend(mcp_secret_keys(&text));
    }
    if let Some(Value::Array(bots)) = read_json(&config.join("settings.json")).and_then(|v| v.get("grokBots").cloned()) {
        keys.extend(bots.iter().filter_map(|b| b.get("id")?.as_str()).map(|id| format!("{}{id}", secrets::GROKBOT_PREFIX)));
    }
    keys.sort();
    keys.dedup();
    keys
}

/// `${secret:<id>}` placeholders → their Credential Manager names.
fn mcp_secret_keys(text: &str) -> Vec<String> {
    text.split("${secret:")
        .skip(1)
        .filter_map(|rest| rest.split_once('}'))
        .map(|(id, _)| format!("{}{id}", secrets::MCP_PREFIX))
        .collect()
}

// ── Settings → "Coming from Coucou" ───────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub report: Option<Report>,
    /// Keys Coucou's service still holds.
    pub old_keys: usize,
    /// Coucou's uninstaller is there to run.
    pub uninstaller: bool,
    /// Agents whose hooks still run Coucou's relay.
    pub legacy_agents: Vec<String>,
}

pub fn status() -> Status {
    let config = platform::config_dir();
    let report = read_json(&config.join(MARK)).and_then(|v| serde_json::from_value(v).ok());
    let hooks = hooks::status();
    let mut legacy_agents = Vec::new();
    if hooks.legacy || hooks.plan_relay_legacy {
        legacy_agents.push("Claude Code".to_string());
    }
    legacy_agents.extend(agents::list().into_iter().filter(|a| a.legacy).map(|a| a.name.to_string()));
    Status {
        report,
        old_keys: key_names(&config).iter().filter(|k| secrets::legacy_present(k)).count(),
        uninstaller: platform::legacy_uninstaller().is_some(),
        legacy_agents,
    }
}

/// Only from a click: deletes Coucou's copy of every key. ARIA's stay.
pub fn clear_old_keys() -> Result<usize, String> {
    let mut cleared = 0;
    for key in key_names(&platform::config_dir()) {
        if secrets::legacy_present(&key) {
            secrets::clear_legacy(&key)?;
            cleared += 1;
        }
    }
    log::line(format!("Coucou's keys deleted: {cleared}"));
    Ok(cleared)
}

/// Only from a click: starts Coucou's own uninstaller, which asks first.
pub fn uninstall_coucou() -> Result<(), String> {
    let exe = platform::legacy_uninstaller().ok_or_else(|| i18n::t("Coucou is not installed any more."))?;
    log::line("starting Coucou's uninstaller");
    std::process::Command::new(exe).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_placeholders_name_their_keys() {
        let text = r#"{"mcpServers":{"a":{"env":{"TOKEN":"${secret:a:env:TOKEN}"},"headers":{"X":"${secret:a:header:X}"}},"b":{"env":{"HOME":"${env:HOME}"}}}}"#;
        assert_eq!(mcp_secret_keys(text), ["mcp-secret:a:env:TOKEN", "mcp-secret:a:header:X"]);
        assert!(mcp_secret_keys("{}").is_empty());
    }

    #[test]
    fn a_copy_never_overwrites_and_skips_what_it_is_told_to() {
        let base = std::env::temp_dir().join(format!("aria-migrate-{}", std::process::id()));
        let (from, to) = (base.join("from"), base.join("to"));
        std::fs::create_dir_all(from.join("memory")).unwrap();
        std::fs::create_dir_all(from.join("bin")).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(from.join("settings.json"), "old").unwrap();
        std::fs::write(from.join("memory").join("notes.md"), "notes").unwrap();
        std::fs::write(from.join("bin").join("relay.exe"), "x").unwrap();
        std::fs::write(to.join("settings.json"), "new").unwrap();

        let mut report = Report::default();
        let bin = from.join("bin");
        copy_tree(&from, &to, &|p| p == bin.as_path(), &|_| true, &mut report);
        assert_eq!(report.files, 1);
        assert_eq!(std::fs::read_to_string(to.join("settings.json")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(to.join("memory").join("notes.md")).unwrap(), "notes");
        assert!(!to.join("bin").exists());
        // The original is untouched.
        assert_eq!(std::fs::read_to_string(from.join("settings.json")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(base);
    }
}
