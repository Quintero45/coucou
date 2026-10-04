// Agent hook installation: Claude Code, Cursor, Codex and Gemini CLI.
//
// The rule from CLAUDE.md is strict and is followed to the letter for every
// agent: read its config file, take a dated backup, merge without touching
// anybody else's hooks, show the diff, and write only after an explicit click.
// Uninstall removes Coucou's entries and nothing else.
//
// Claude Code's command is only the quoted exe path in forward slashes plus the
// event name: on Windows Claude Code runs hook commands through Git Bash, and
// anything with PowerShell or cmd in it breaks.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager};
use crate::{platform, settings};

/// Every agent whose config file Coucou knows how to merge into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookTarget {
    #[default]
    Claude,
    Cursor,
    Codex,
    Gemini,
}

/// What the user picked next to the Install button.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HookOptions {
    /// Cursor only: route shell and MCP approvals through the island.
    pub approvals: bool,
}

/// Every event the island reacts to, with the hook timeout written to settings.json.
/// PermissionRequest waits for a human, so it gets the decision timeout + 10 s.
pub const HOOK_EVENTS: &[(&str, u64)] = &[
    ("SessionStart", 10),
    ("SessionEnd", 10),
    ("UserPromptSubmit", 10),
    ("PreToolUse", 10),
    ("PostToolUse", 10),
    ("PostToolUseFailure", 10),
    ("PermissionRequest", 120),
    ("Notification", 10),
    ("Stop", 10),
    ("StopFailure", 10),
    ("SubagentStart", 10),
    ("SubagentStop", 10),
];

/// Cursor's events (camelCase, timeouts in seconds). The relay normalises them.
const CURSOR_EVENTS: &[(&str, u64)] = &[
    ("sessionStart", 10),
    ("sessionEnd", 10),
    ("beforeSubmitPrompt", 10),
    ("preToolUse", 10),
    ("postToolUse", 10),
    ("postToolUseFailure", 10),
    ("afterFileEdit", 10),
    ("afterAgentResponse", 10),
    ("stop", 10),
    ("subagentStart", 10),
    ("subagentStop", 10),
];
/// Installed only with approvals on: they wait for a human.
const CURSOR_GATES: &[(&str, u64)] = &[("beforeShellExecution", 120), ("beforeMCPExecution", 120)];
/// Cursor's question tool, answered from the island. The relay gives up at
/// 125 s and lets Cursor show its own card.
const CURSOR_QUESTION_MATCHER: &str = "AskQuestion|AskUserQuestion";

/// Codex events, timeouts in seconds. Codex names its events itself.
const CODEX_EVENTS: &[(&str, u64)] = &[
    ("SessionStart", 10),
    ("UserPromptSubmit", 10),
    ("PreToolUse", 10),
    ("PermissionRequest", 120),
    ("PostToolUse", 10),
    ("Stop", 10),
    ("SubagentStart", 10),
    ("SubagentStop", 10),
    ("Interrupt", 3),
    ("SessionEnd", 3),
];

/// Gemini CLI: (its event key, the canonical name passed on argv, timeout in ms).
const GEMINI_EVENTS: &[(&str, &str, u64)] = &[
    ("SessionStart", "SessionStart", 10_000),
    ("SessionEnd", "SessionEnd", 10_000),
    ("BeforeTool", "PreToolUse", 5_000),
    ("AfterTool", "PostToolUse", 5_000),
    ("BeforeAgent", "UserPromptSubmit", 5_000),
    ("AfterAgent", "Stop", 5_000),
];

/// Marker that identifies a Coucou entry inside any agent's config.
const MARKER: &str = "coucou-hook";

impl HookTarget {
    pub fn path(self) -> PathBuf {
        let home = platform::home_dir();
        match self {
            HookTarget::Claude => home.join(".claude").join("settings.json"),
            HookTarget::Cursor => home.join(".cursor").join("hooks.json"),
            HookTarget::Codex => home.join(".codex").join("hooks.json"),
            HookTarget::Gemini => home.join(".gemini").join("settings.json"),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    pub installed: bool,
    pub settings_path: String,
    pub hook_path: String,
    pub hook_ready: bool,
    /// Cursor: the shell / MCP approval gates are installed too.
    pub approvals: bool,
    /// The installed entries are the ones this build would write.
    pub up_to_date: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookPreview {
    pub diff: String,
    pub backup: String,
    pub settings_path: String,
    /// Identifies the bytes this diff was computed from; handed back to `write`
    /// so we only ever apply what the user actually looked at.
    pub fingerprint: String,
}

/// Reads the target's config file.
///
/// The only error that means "start from nothing" is the file not being there.
/// Everything else — a lock held by another process, a permission problem, JSON
/// we cannot parse — is reported, because the alternative is treating somebody's
/// unreadable settings as an empty object and then writing that back over them.
fn read_settings(target: HookTarget) -> Result<Value, String> {
    let path = target.path();
    match std::fs::read(&path) {
        Ok(bytes) => parse_settings(&bytes, &path.display().to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        // A lock, a permission problem, a bad drive: all of them mean we do not
        // know what is in there, and not knowing is not the same as empty.
        Err(err) => Err(format!("Can't read {}: {err}", path.display())),
    }
}

/// The parsing half of `read_settings`, split out so it can be tested without a
/// home directory.
fn parse_settings(bytes: &[u8], path: &str) -> Result<Value, String> {
    // PowerShell writes a UTF-8 BOM with `Set-Content -Encoding utf8`, and
    // serde_json refuses it. Stripping it is safe and well defined; guessing at
    // anything else is not.
    let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if text.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    match serde_json::from_slice::<Value>(text) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err(format!("{path} isn't a JSON object — Coucou won't touch it.")),
        Err(err) => Err(format!(
            "{path} isn't valid JSON ({err}). Fix or move it, then try again — Coucou won't overwrite it."
        )),
    }
}

/// The settings as they are, or an empty object when we cannot tell. Only for
/// read-only paths like `status()`, which must never fail loudly; anything that
/// writes uses `read_settings()` and surfaces the error instead.
fn read_settings_lossy(target: HookTarget) -> Value {
    read_settings(target).unwrap_or_else(|_| json!({}))
}

#[cfg(windows)]
fn hook_command(args: &str) -> String {
    let exe = settings::hook_exe_path().to_string_lossy().replace('\\', "/");
    format!("\"{exe}\" {args}")
}

/// Cursor may run the command through cmd, PowerShell or a POSIX shell. A bare
/// path works in all three; PowerShell refuses a quoted one without `&`, so the
/// quotes only go on when a space forces them.
#[cfg(windows)]
fn cursor_command(args: &str) -> String {
    let exe = settings::hook_exe_path().to_string_lossy().replace('\\', "/");
    if exe.contains(' ') {
        format!("\"{exe}\" {args}")
    } else {
        format!("{exe} {args}")
    }
}

/// Claude Code runs the command through `sh`, which still reads `$`, `` ` ``
/// and `\` inside double quotes. Single quotes keep the path a path, whatever
/// the home directory is called.
#[cfg(unix)]
fn hook_command(args: &str) -> String {
    format!("{} {args}", sh_quote(&settings::hook_exe_path().to_string_lossy()))
}

#[cfg(unix)]
fn cursor_command(args: &str) -> String {
    hook_command(args)
}

/// `s` as one single-quoted shell word: `'` becomes `'\''`, nothing else is
/// special inside single quotes.
#[cfg(unix)]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Claude Code, Codex and Gemini nest commands in `{"hooks":[{"command":…}]}`;
/// Cursor puts `command` on the entry itself. Either shape counts.
fn entry_is_ours(entry: &Value) -> bool {
    let is_ours = |h: &Value| {
        h.get("command")
            .and_then(Value::as_str)
            .map(|c| c.contains(MARKER))
            .unwrap_or(false)
    };
    is_ours(entry)
        || entry
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| hooks.iter().any(is_ours))
            .unwrap_or(false)
}

/// Every (event, entry) pair Coucou wants in the target's config.
fn our_entries(target: HookTarget, opts: HookOptions) -> Vec<(String, Value)> {
    match target {
        HookTarget::Claude => {
            let mut out: Vec<(String, Value)> = HOOK_EVENTS
                .iter()
                .map(|(event, timeout)| {
                    let entry = json!({
                        "hooks": [{ "type": "command", "command": hook_command(event), "timeout": timeout }]
                    });
                    ((*event).to_string(), entry)
                })
                .collect();
            // AskUserQuestion answered from the island: a PreToolUse hook that only
            // acts for that one tool and stays silent for everything else.
            out.push((
                "PreToolUse".into(),
                json!({
                    "matcher": "AskUserQuestion",
                    "hooks": [{ "type": "command", "command": hook_command("--ask"), "timeout": 130 }]
                }),
            ));
            out
        }
        HookTarget::Cursor => {
            let mut out: Vec<(String, Value)> = CURSOR_EVENTS
                .iter()
                .map(|(event, timeout)| {
                    let entry = json!({ "command": cursor_command(&format!("--agent cursor {event}")), "timeout": timeout });
                    ((*event).to_string(), entry)
                })
                .collect();
            out.push((
                "preToolUse".into(),
                json!({
                    "command": cursor_command("--agent cursor --ask preToolUse"),
                    "matcher": CURSOR_QUESTION_MATCHER,
                    "timeout": 130,
                }),
            ));
            if opts.approvals {
                for (event, timeout) in CURSOR_GATES {
                    let entry = json!({
                        "command": cursor_command(&format!("--agent cursor --approve {event}")),
                        "timeout": timeout,
                    });
                    out.push(((*event).to_string(), entry));
                }
            }
            out
        }
        HookTarget::Codex => CODEX_EVENTS
            .iter()
            .map(|(event, timeout)| {
                let mut hook = json!({ "type": "command", "command": hook_command("--agent codex"), "timeout": timeout });
                if *event == "PermissionRequest" {
                    hook["statusMessage"] = json!("Waiting for your answer in Coucou");
                }
                ((*event).to_string(), json!({ "hooks": [hook] }))
            })
            .collect(),
        HookTarget::Gemini => GEMINI_EVENTS
            .iter()
            .map(|(event, canonical, timeout)| {
                let entry = json!({
                    "matcher": "*",
                    "hooks": [{
                        "type": "command",
                        "command": hook_command(&format!("--agent gemini {canonical}")),
                        "timeout": timeout,
                    }]
                });
                ((*event).to_string(), entry)
            })
            .collect(),
    }
}

/// Settings with Coucou's hooks added; everything else is left untouched.
fn merged(target: HookTarget, existing: &Value, opts: HookOptions) -> Value {
    let mut root = existing.as_object().cloned().unwrap_or_default();
    // Our previous entries go first, wherever they were: switching Cursor
    // approvals off must remove the gates, not leave them behind.
    let cleaned = without_ours(&Value::Object(root.clone()));
    let mut hooks = cleaned
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);

    for (event, entry) in our_entries(target, opts) {
        let mut list = hooks.get(&event).and_then(Value::as_array).cloned().unwrap_or_default();
        list.push(entry);
        hooks.insert(event, Value::Array(list));
    }

    if target == HookTarget::Cursor && !root.contains_key("version") {
        root.insert("version".into(), json!(1));
    }
    root.insert("hooks".into(), Value::Object(hooks));
    Value::Object(root)
}

/// Settings with every Coucou entry removed, and nothing else changed.
fn without_ours(existing: &Value) -> Value {
    let mut root = existing.as_object().cloned().unwrap_or_default();
    let Some(hooks) = root.get("hooks").and_then(Value::as_object).cloned() else {
        return Value::Object(root);
    };
    let mut out = Map::new();
    for (event, value) in hooks {
        match value.as_array() {
            Some(list) => {
                let kept: Vec<Value> =
                    list.iter().filter(|e| !entry_is_ours(e)).cloned().collect();
                if !kept.is_empty() {
                    out.insert(event, Value::Array(kept));
                }
            }
            None => {
                out.insert(event, value);
            }
        }
    }
    if out.is_empty() {
        root.remove("hooks");
    } else {
        root.insert("hooks".into(), Value::Object(out));
    }
    Value::Object(root)
}

/// Coucou's own entries per event, in order; the user's entries don't count.
fn ours_by_event(v: &Value) -> Vec<(String, Vec<Value>)> {
    let mut out: Vec<(String, Vec<Value>)> = v
        .get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks
                .iter()
                .filter_map(|(event, list)| {
                    let ours: Vec<Value> = list.as_array()?.iter().filter(|e| entry_is_ours(e)).cloned().collect();
                    (!ours.is_empty()).then(|| (event.clone(), ours))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn same_ours(a: &Value, b: &Value) -> bool {
    ours_by_event(a) == ours_by_event(b)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// Down to the second: installing then uninstalling in the same minute must not
/// quietly overwrite the first backup.
fn stamp() -> String {
    let t = platform::local_time();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

fn backup_path(target: HookTarget) -> PathBuf {
    let p = target.path();
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "settings.json".into());
    p.with_file_name(format!("{name}.bak-{}", stamp()))
}

/// Identifies the exact bytes a preview was computed from. FNV-1a is plenty:
/// the question is only "is this still the file I showed the user?".
fn fingerprint(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

fn current_fingerprint(target: HookTarget) -> String {
    match std::fs::read(target.path()) {
        Ok(bytes) => fingerprint(&bytes),
        Err(_) => fingerprint(b""),
    }
}

fn has_ours(current: &Value, event: Option<&str>) -> bool {
    current
        .get("hooks")
        .and_then(Value::as_object)
        .map(|hooks| {
            hooks
                .iter()
                .filter(|(k, _)| event.is_none_or(|e| e == k.as_str()))
                .filter_map(|(_, v)| v.as_array())
                .flatten()
                .any(entry_is_ours)
        })
        .unwrap_or(false)
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn status(target: HookTarget) -> HookStatus {
    let current = read_settings_lossy(target);
    let hook_path = settings::hook_exe_path();
    let installed = has_ours(&current, None);
    let approvals = target == HookTarget::Cursor && has_ours(&current, Some("beforeShellExecution"));
    HookStatus {
        installed,
        settings_path: target.path().to_string_lossy().to_string(),
        hook_ready: hook_path.exists(),
        hook_path: hook_path.to_string_lossy().to_string(),
        approvals,
        up_to_date: !installed || same_ours(&current, &merged(target, &current, HookOptions { approvals })),
    }
}

pub fn preview(target: HookTarget, install: bool, opts: HookOptions) -> Result<HookPreview, String> {
    let current = read_settings(target)?;
    let next = if install { merged(target, &current, opts) } else { without_ours(&current) };
    Ok(HookPreview {
        diff: unified_diff(&pretty(&current), &pretty(&next)),
        backup: backup_path(target).to_string_lossy().to_string(),
        settings_path: target.path().to_string_lossy().to_string(),
        fingerprint: current_fingerprint(target),
    })
}

/// Writes the merged (or cleaned) settings after taking a dated backup.
///
/// `fingerprint` is the one the preview was computed from. If the file changed
/// in between — another tool, another window, the user's own editor — we stop
/// and make them look at a fresh diff, because the only thing worse than not
/// installing the hooks is silently reverting somebody else's edit.
pub fn write(target: HookTarget, install: bool, fingerprint: &str, opts: HookOptions) -> Result<String, String> {
    let path = target.path();
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    // Read before the backup: an unreadable file must abort before we touch
    // anything at all.
    let current = read_settings(target)?;
    if current_fingerprint(target) != fingerprint {
        return Err(format!(
            "{} changed since the preview. Nothing was written — review the new diff.",
            path.display()
        ));
    }

    let backup = backup_path(target);
    if path.exists() {
        std::fs::copy(&path, &backup).map_err(|e| format!("backup failed: {e}"))?;
    }

    let next = if install { merged(target, &current, opts) } else { without_ours(&current) };
    let mut text = pretty(&next);
    text.push('\n');

    // A dotfiles setup often makes settings.json a symlink: write to the file it
    // points at, so the link survives the rename below.
    #[cfg(unix)]
    let path = std::fs::canonicalize(&path).unwrap_or(path);

    // Write beside the target and rename over it: a crash or a full disk leaves
    // the original settings.json intact rather than half a file.
    let temp = path.with_extension(format!("json.coucou-{}", std::process::id()));
    if let Err(err) = write_like(&temp, &path, text.as_bytes()) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("write failed: {err}"));
    }
    if let Err(err) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("write failed: {err}"));
    }
    Ok(backup.to_string_lossy().to_string())
}

/// Writes `bytes` to `temp`, which is about to replace `original`.
///
/// On Linux a fresh file would get the umask's 0644, and settings.json can hold
/// API keys in its `env` block: the new file is created readable by us only,
/// then given the original's permissions, so the rename never widens them.
fn write_like(temp: &Path, original: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(temp)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(original)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = original;
    Ok(())
}

/// Copies the relay (coucou-hook.exe / coucou-hook) into the local data dir's
/// bin/ on launch. In a bundled install it comes from the app resources; in
/// `tauri dev` it sits next to the app binary in the workspace target directory.
///
/// Every candidate is tried rather than just the first, because getting this
/// wrong is silent and fatal: `resources` used to be a glob, which made NSIS
/// mirror the source path into `_up_\target\release\`, no candidate matched, and
/// the relay was simply never installed. It only looked healthy on a developer
/// machine, where a leftover copy from `tauri dev` was already sitting in bin/.
pub fn ensure_hook_exe(app: &AppHandle) {
    let dest = settings::hook_exe_path();
    let Some(dir) = dest.parent() else { return };
    // Nobody else may swap the relay Claude Code runs: its folder is ours only.
    if platform::ensure_private_dir(&settings::local_dir()).is_err()
        || std::fs::create_dir_all(dir).is_err()
    {
        return;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = app.path().resolve(platform::HOOK_EXE, tauri::path::BaseDirectory::Resource) {
        candidates.push(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // Installed build, then `tauri dev` (target/debug) next to the
            // release hook the pre-build step produces.
            candidates.push(parent.join(platform::HOOK_EXE));
            candidates.push(parent.join("../release").join(platform::HOOK_EXE));
            // Belt and braces: where the old glob form used to land it.
            candidates.push(parent.join("_up_/target/release").join(platform::HOOK_EXE));
        }
    }

    let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
    let Some(src) = candidates.into_iter().find(|p| p.exists()) else {
        crate::log::line(format!(
            "{} not found — Claude Code hooks cannot work. Looked in: {}",
            platform::HOOK_EXE,
            tried.join(", ")
        ));
        return;
    };
    install_relay(&src, &dest);
}

#[cfg(windows)]
fn install_relay(src: &Path, dest: &Path) {
    let same = match (std::fs::metadata(src), std::fs::metadata(dest)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok() == b.modified().ok(),
        _ => false,
    };
    if same {
        return;
    }
    // A hook may be running right now and hold the file open; keeping the old
    // copy is fine, it is the same relay.
    if let Err(err) = std::fs::copy(src, dest) {
        if !dest.exists() {
            crate::log::line(format!("could not install {}: {err}", platform::HOOK_EXE));
        }
    }
}

/// Linux does not keep the modification time on copy, so the contents decide.
/// The new relay is written beside the old one and renamed over it: a hook
/// starting at that moment runs either the old relay or the new one, never half
/// of one, and a relay that is running right now does not block the update.
#[cfg(unix)]
fn install_relay(src: &Path, dest: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if matches!((std::fs::read(src), std::fs::read(dest)), (Ok(a), Ok(b)) if a == b) {
        return;
    }
    let temp = dest.with_extension(format!("new-{}", std::process::id()));
    let result = std::fs::copy(src, &temp)
        .and_then(|_| std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755)))
        .and_then(|_| std::fs::rename(&temp, dest));
    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp);
        crate::log::line(format!("could not install {}: {err}", platform::HOOK_EXE));
    }
}

// ── Minimal unified diff (LCS) ────────────────────────────────────────────────

/// settings.json is short, so a plain O(n·m) LCS is the simplest honest diff.
fn unified_diff(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());

    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out: Vec<String> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("- {}", a[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+ {}", b[j]));
        j += 1;
    }

    // Keep three lines of context around each change so the panel stays readable.
    let changed: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with('+') || l.starts_with('-'))
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return "No change.".into();
    }
    let mut keep = vec![false; out.len()];
    for idx in changed {
        let lo = idx.saturating_sub(3);
        let hi = (idx + 4).min(out.len());
        for k in lo..hi {
            keep[k] = true;
        }
    }
    let mut result = String::new();
    let mut gap = false;
    for (idx, line) in out.iter().enumerate() {
        if keep[idx] {
            result.push_str(line);
            result.push('\n');
            gap = false;
        } else if !gap {
            result.push_str("  …\n");
            gap = true;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHERE: &str = "settings.json";

    #[test]
    fn a_utf8_bom_is_stripped_not_treated_as_corruption() {
        // PowerShell 5's `Set-Content -Encoding utf8` produces exactly this.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"model":"opus","hooks":{}}"#);
        let parsed = parse_settings(&bytes, WHERE).expect("a BOM must not defeat the parser");
        assert_eq!(parsed["model"], "opus");
    }

    #[test]
    fn unreadable_content_is_an_error_never_an_empty_object() {
        // This is the whole bug: returning {} here meant `merged()` produced a
        // file containing nothing but Coucou's hooks, and the write replaced
        // everything the user had.
        for bad in [&b"{ not json"[..], &b"[1,2,3]"[..], &b"\"a string\""[..]] {
            assert!(
                parse_settings(bad, WHERE).is_err(),
                "content we cannot use must refuse, not come back empty"
            );
        }
    }

    #[test]
    fn empty_and_whitespace_files_start_from_nothing() {
        assert_eq!(parse_settings(b"", WHERE).unwrap(), json!({}));
        assert_eq!(parse_settings(b"  
	 ", WHERE).unwrap(), json!({}));
    }

    #[test]
    fn merging_keeps_every_other_setting_and_every_foreign_hook() {
        let existing = serde_json::json!({
            "model": "claude-opus-5",
            "theme": "dark",
            "enabledPlugins": ["a", "b"],
            "hooks": {
                "PreToolUse": [
                    { "hooks": [{ "type": "command", "command": "someone-elses-tool.exe" }] }
                ],
                "SomeEventWeDoNotTouch": [
                    { "hooks": [{ "type": "command", "command": "keep-me.exe" }] }
                ]
            }
        });

        let after = merged(HookTarget::Claude, &existing, HookOptions::default());
        assert_eq!(after["model"], "claude-opus-5");
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["enabledPlugins"], serde_json::json!(["a", "b"]));

        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            pre.iter().any(|e| serde_json::to_string(e).unwrap().contains("someone-elses-tool.exe")),
            "another tool's hook was dropped"
        );
        assert!(pre.iter().any(entry_is_ours), "our own hook was not added");
        assert!(after["hooks"]["SomeEventWeDoNotTouch"].is_array());

        // And removing ours puts it back exactly as it was.
        let cleaned = without_ours(&after);
        assert_eq!(cleaned, existing);
    }

    #[test]
    fn claude_gets_the_ask_user_question_hook() {
        let after = merged(HookTarget::Claude, &json!({}), HookOptions::default());
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre.iter().any(|e| e["matcher"] == "AskUserQuestion"
            && e["hooks"][0]["command"].as_str().unwrap().ends_with("--ask")));
    }

    #[test]
    fn cursor_hooks_are_flat_versioned_and_gates_are_opt_in() {
        let existing = json!({
            "version": 1,
            "hooks": { "afterFileEdit": [{ "command": "./hooks/format.sh" }] }
        });
        let plain = merged(HookTarget::Cursor, &existing, HookOptions { approvals: false });
        assert_eq!(plain["version"], 1);
        let edits = plain["hooks"]["afterFileEdit"].as_array().unwrap();
        assert!(edits.iter().any(|e| e["command"] == "./hooks/format.sh"), "foreign hook dropped");
        assert!(edits.iter().any(|e| e["command"].as_str().unwrap().contains("--agent cursor afterFileEdit")));
        assert!(plain["hooks"].get("beforeShellExecution").is_none());

        let gated = merged(HookTarget::Cursor, &plain, HookOptions { approvals: true });
        let shell = gated["hooks"]["beforeShellExecution"].as_array().unwrap();
        assert_eq!(shell.len(), 1);
        assert!(shell[0]["command"].as_str().unwrap().contains("--approve"));
        assert_eq!(shell[0]["timeout"], 120);

        // Turning approvals back off removes the gates, and uninstalling restores the original.
        let off = merged(HookTarget::Cursor, &gated, HookOptions { approvals: false });
        assert!(off["hooks"].get("beforeShellExecution").is_none());
        assert_eq!(without_ours(&off), existing);
    }

    #[test]
    fn cursor_questions_get_their_own_long_hook() {
        let after = merged(HookTarget::Cursor, &json!({}), HookOptions::default());
        let pre = after["hooks"]["preToolUse"].as_array().unwrap();
        let ask = pre.iter().find(|e| e["matcher"] == CURSOR_QUESTION_MATCHER).unwrap();
        assert!(ask["command"].as_str().unwrap().ends_with("--agent cursor --ask preToolUse"));
        assert_eq!(ask["timeout"], 130);
    }

    #[test]
    fn up_to_date_ignores_the_users_entries_and_their_order() {
        let mine = json!({ "command": "./mine.sh" });
        let fresh = merged(HookTarget::Cursor, &json!({ "hooks": { "stop": [mine.clone()] } }), HookOptions::default());
        assert!(same_ours(&fresh, &merged(HookTarget::Cursor, &fresh, HookOptions::default())));

        let mut reordered = fresh.clone();
        let stop = reordered["hooks"]["stop"].as_array_mut().unwrap();
        stop.reverse();
        assert!(same_ours(&reordered, &fresh));

        let mut old = fresh.clone();
        let pre = old["hooks"]["preToolUse"].as_array_mut().unwrap();
        pre.retain(|e| e.get("matcher").is_none());
        assert!(!same_ours(&old, &merged(HookTarget::Cursor, &old, HookOptions::default())));
    }

    #[test]
    fn a_fresh_cursor_file_gets_a_version() {
        let after = merged(HookTarget::Cursor, &json!({}), HookOptions::default());
        assert_eq!(after["version"], 1);
        assert!(after["hooks"]["stop"].is_array());
    }

    #[test]
    fn codex_and_gemini_tag_their_agent() {
        let codex = merged(HookTarget::Codex, &json!({}), HookOptions::default());
        let perm = &codex["hooks"]["PermissionRequest"][0]["hooks"][0];
        assert!(perm["command"].as_str().unwrap().contains("--agent codex"));
        assert_eq!(perm["timeout"], 120);

        let gemini = merged(HookTarget::Gemini, &json!({ "theme": "x" }), HookOptions::default());
        assert_eq!(gemini["theme"], "x");
        let before = &gemini["hooks"]["BeforeTool"][0];
        assert_eq!(before["matcher"], "*");
        assert!(before["hooks"][0]["command"].as_str().unwrap().ends_with("--agent gemini PreToolUse"));
    }

    #[test]
    fn a_fingerprint_notices_any_change() {
        assert_eq!(fingerprint(b"{}"), fingerprint(b"{}"));
        assert_ne!(fingerprint(b"{}"), fingerprint(b"{ }"));
        assert_ne!(fingerprint(b""), fingerprint(b"{}"));
    }

    #[cfg(unix)]
    #[test]
    fn the_hook_path_is_one_shell_word_whatever_it_contains() {
        assert_eq!(sh_quote("/home/a b/x"), "'/home/a b/x'");
        // $, backticks, backslashes and double quotes stay literal in single quotes.
        assert_eq!(sh_quote(r#"/h/$(id)`x`\"y"#), r#"'/h/$(id)`x`\"y'"#);
        // A single quote closes, escapes and reopens.
        assert_eq!(sh_quote("/h/it's"), r"'/h/it'\''s'");
    }

    /// settings.json can carry API keys in its `env` block: rewriting it must
    /// never make it readable by more people than before.
    #[cfg(unix)]
    #[test]
    fn rewriting_settings_never_widens_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("coucou-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("settings.json");
        let temp = dir.join("settings.json.new");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        for wanted in [0o600, 0o640, 0o644] {
            std::fs::write(&original, b"{}").unwrap();
            std::fs::set_permissions(&original, std::fs::Permissions::from_mode(wanted)).unwrap();
            let _ = std::fs::remove_file(&temp);
            write_like(&temp, &original, b"{\"a\":1}").unwrap();
            assert_eq!(mode(&temp), wanted, "the rewrite must keep {wanted:o}");
        }

        // No original: ours only.
        std::fs::remove_file(&original).unwrap();
        let _ = std::fs::remove_file(&temp);
        write_like(&temp, &original, b"{}").unwrap();
        assert_eq!(mode(&temp), 0o600);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Everything filesystem-shaped lives in one test on purpose: it points
    /// the home directory at a temp directory, and that is process-wide.
    #[test]
    fn writing_backs_up_preserves_and_refuses_a_changed_file() {
        let tmp = std::env::temp_dir().join(format!("coucou-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".claude")).unwrap();
        std::env::set_var(platform::HOME_VAR, &tmp);

        let path = HookTarget::Claude.path();
        assert!(path.starts_with(&tmp), "the test must not touch the real home");

        // A real-shaped file, written the way PowerShell 5 would: UTF-8 with BOM.
        let original = r#"{"model":"claude-opus-5","theme":"dark","tui":{"x":1},"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"other-tool.exe"}]}]}}"#;
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(original.as_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let claude = HookTarget::Claude;
        let opts = HookOptions::default();

        // Install.
        let plan = preview(claude, true, opts).expect("a BOM must not stop the preview");
        assert!(plan.diff.contains("coucou-hook"), "the diff must show what changes");
        let backup = write(claude, true, &plan.fingerprint, opts).expect("install should succeed");

        // The backup holds the original bytes, BOM and all.
        assert_eq!(std::fs::read(&backup).unwrap(), bytes);

        // Everything else survived, and so did the other tool's hook.
        let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["model"], "claude-opus-5");
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["tui"]["x"], 1);
        let pre = after["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(pre.iter().any(|e| serde_json::to_string(e).unwrap().contains("other-tool.exe")));
        assert!(status(claude).installed);

        // A file that moved since the preview is refused, and left alone.
        let stale = preview(claude, false, opts).unwrap();
        std::fs::write(&path, br#"{"model":"someone-else-edited-this"}"#).unwrap();
        let err = write(claude, false, &stale.fingerprint, opts).unwrap_err();
        assert!(err.contains("changed since the preview"), "got: {err}");
        let untouched: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(untouched["model"], "someone-else-edited-this");

        // Content we cannot parse is refused before anything is written.
        std::fs::write(&path, b"{ broken").unwrap();
        assert!(preview(claude, true, opts).is_err());
        assert!(write(claude, true, "whatever", opts).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{ broken");

        // Cursor: a missing file is created with a dated backup name of its own.
        let cursor = HookTarget::Cursor;
        let gated = HookOptions { approvals: true };
        let plan = preview(cursor, true, gated).unwrap();
        assert!(plan.backup.contains("hooks.json.bak-"));
        write(cursor, true, &plan.fingerprint, gated).unwrap();
        let st = status(cursor);
        assert!(st.installed && st.approvals);
        assert!(cursor.path().starts_with(&tmp));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
