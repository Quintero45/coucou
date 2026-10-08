// Approval policy for the assistant's own tools. Part of the protected core.
//
// Reading is free; anything with a side effect waits for a click on the island's
// approval card. No click in time, a paused island or a card already in use all
// mean no. "Permitir siempre" creates an allow rule (see `AllowRule`): for a
// file-writing tool, a folder (canonical path prefix); for any other tool, that
// tool for that caller (ARIA or one Bot). Rules expire (1 h by default, or at
// app restart), live in memory only, are logged when added, used and expired,
// and never cover a shell (run_powershell & co.): those always ask.
// Every call — free, approved, refused — is written to the audit log.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use crate::island::WINDOW_LABEL;
use crate::log;
use crate::pipe::{Pending, Reply};

/// The island only has to put the card up; it does that synchronously.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Reads only: runs without asking.
    Read,
    /// Changes something, or sends something out: needs a click.
    Act,
}

/// Default life of a "Permitir siempre" rule.
pub const RULE_TTL: Duration = Duration::from_secs(60 * 60);
/// Who asks when it is ARIA (Bots are `bot-<slug>`).
pub const ARIA: &str = "aria";

/// Tools a standing approval may never cover: anything that runs a command.
const NEVER_ALWAYS: &[&str] = &["run_powershell", "powershell", "shell", "run_shell", "run_command", "bash", "cmd", "terminal"];
/// Tools whose rules are scoped to a folder rather than to the tool.
const WRITE_TOOLS: &[&str] = &["write_file"];

pub fn never_always(tool: &str) -> bool {
    let t = tool.to_ascii_lowercase();
    NEVER_ALWAYS.contains(&t.as_str()) || t.contains("shell") || t.contains("powershell")
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleScope {
    /// Files under this folder (canonical, case-folded on Windows), write tools only.
    Folder(PathBuf),
    /// Every call of the tool.
    Tool,
}

#[derive(Debug, Clone)]
pub struct AllowRule {
    /// `aria` or `bot-<slug>`: a rule never crosses callers.
    pub who: String,
    pub tool: String,
    pub scope: RuleScope,
    /// None = until the app restarts.
    pub expires: Option<Instant>,
}

impl AllowRule {
    fn describe(&self) -> String {
        let scope = match &self.scope {
            RuleScope::Folder(p) => format!("folder:{}", p.display()),
            RuleScope::Tool => "tool".to_string(),
        };
        let until = match self.expires {
            Some(t) => format!("{}s", t.saturating_duration_since(Instant::now()).as_secs()),
            None => "restart".to_string(),
        };
        format!("who={} tool={} scope={scope} expires_in={until}", self.who, self.tool)
    }
}

static RULES: LazyLock<Mutex<Vec<AllowRule>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static COUNTER: AtomicU64 = AtomicU64::new(1);

/// Forgets every rule — called when the chat is reset (more conservative than
/// the expiry alone; drop this call to let rules outlive a chat reset).
pub fn reset_session() {
    let n = std::mem::take(&mut *RULES.lock().unwrap()).len();
    if n > 0 {
        log::line(format!("policy allow-rules cleared ({n}) — chat reset"));
    }
}

/// Canonical, case-folded form used for prefix checks. A file that does not
/// exist yet (write_file creating it) is resolved through its parent folder.
pub fn canonical(path: &Path) -> Option<PathBuf> {
    let resolved = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) => std::fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?),
    };
    let s = resolved.to_string_lossy().to_string();
    Some(PathBuf::from(if cfg!(windows) { s.to_lowercase() } else { s }))
}

/// write_file's target as `tools::describe` writes it: "<path> (N chars, overwrite|append)".
pub fn write_target_path(target: &str) -> Option<PathBuf> {
    let path = target.rsplit_once(" (").map(|(p, _)| p).unwrap_or(target).trim();
    let p = PathBuf::from(path);
    p.is_absolute().then_some(p)
}

/// The rule "Permitir siempre" would create for this call, or None when no
/// standing approval is allowed (a shell, or a write without a usable path).
pub fn rule_for(who: &str, tool: &str, target: &str, ttl: Option<Duration>) -> Option<AllowRule> {
    if never_always(tool) {
        return None;
    }
    let scope = if WRITE_TOOLS.contains(&tool) {
        let file = canonical(&write_target_path(target)?)?;
        RuleScope::Folder(file.parent()?.to_path_buf())
    } else {
        RuleScope::Tool
    };
    Some(AllowRule { who: who.to_string(), tool: tool.to_string(), scope, expires: ttl.map(|d| Instant::now() + d) })
}

/// Pure check, for tests and for `allowed_by_rule`.
pub fn rule_covers(rule: &AllowRule, who: &str, tool: &str, target: &str, now: Instant) -> bool {
    if rule.who != who || rule.tool != tool || never_always(tool) {
        return false;
    }
    if rule.expires.is_some_and(|t| now >= t) {
        return false;
    }
    match &rule.scope {
        RuleScope::Tool => true,
        // Component-wise prefix: C:\a\b covers C:\a\b\c.txt, not C:\a\bc\x.txt.
        RuleScope::Folder(dir) => write_target_path(target)
            .and_then(|p| canonical(&p))
            .is_some_and(|file| file.starts_with(dir)),
    }
}

/// Drops expired rules (logging them) and reports whether one covers this call.
pub fn allowed_by_rule(who: &str, tool: &str, target: &str) -> bool {
    let now = Instant::now();
    let mut rules = RULES.lock().unwrap();
    rules.retain(|r| {
        let alive = r.expires.is_none_or(|t| now < t);
        if !alive {
            log::line(format!("policy allow-rule expired {}", r.describe()));
        }
        alive
    });
    match rules.iter().find(|r| rule_covers(r, who, tool, target, now)) {
        Some(r) => {
            log::line(format!("policy allow-rule used {} target={}", r.describe(), audit_target(target)));
            true
        }
        None => false,
    }
}

/// The owner answered "always…". Decisions: `always` (1 h), `always-session`
/// (until restart). Returns false when no rule may be made (the call itself is
/// still allowed once).
pub fn remember_always(who: &str, tool: &str, target: &str, decision: &str) -> bool {
    let ttl = if decision == "always-session" { None } else { Some(RULE_TTL) };
    match rule_for(who, tool, target, ttl) {
        Some(rule) => {
            log::line(format!("policy allow-rule added {}", rule.describe()));
            RULES.lock().unwrap().push(rule);
            true
        }
        None => {
            log::line(format!("policy allow-rule refused who={who} tool={tool} (never for shells / no folder) — allowed once"));
            false
        }
    }
}

pub fn audit(tool: &str, verdict: &str, target: &str) {
    log::line(format!("assistant tool {tool} {verdict} · {}", audit_target(target)));
}

/// What the audit log keeps of a target: redacted first, then cut, so a secret
/// split at the 300th character is still hidden.
fn audit_target(target: &str) -> String {
    let target: String = redact(target).chars().take(300).collect();
    target.replace('\n', " ")
}

// ── Secret redaction ──────────────────────────────────────────────────────────
// Hand-written (no regex crate) equivalent of the four regexes of `TEXT_SECRETS`
// in windows/src/core/botlog.ts, applied in the same order with the same
// JavaScript semantics (`\s`, `\w`, `\b`, case rules, greedy backtracking).

const HIDDEN: &str = "<oculto>";

// JavaScript's `\s` (what botlog.ts's regexes use), not Rust's `is_whitespace`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}'
            | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// JavaScript's `\w` without the `u` flag.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `\b` before position `i`.
fn boundary(s: &[char], i: usize) -> bool {
    let before = i > 0 && word(s[i - 1]);
    let after = i < s.len() && word(s[i]);
    before != after
}

/// Case-insensitive (ASCII) literal at `i`; the index just past it.
fn lit_ci(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let mut j = i;
    for l in lit.chars() {
        if j >= s.len() || !s[j].eq_ignore_ascii_case(&l) {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// Case-sensitive literal at `i`.
fn lit(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let mut j = i;
    for l in lit.chars() {
        if j >= s.len() || s[j] != l {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// Greedy run of chars matching `f` from `i`; the index just past it.
fn run(s: &[char], i: usize, f: impl Fn(char) -> bool) -> usize {
    let mut j = i;
    while j < s.len() && f(s[j]) {
        j += 1;
    }
    j
}

/// One pass of a global replace: `at` returns (end, replacement) for a match
/// starting at `i`; the scan resumes after it, like `String.replace(/…/g)`.
fn replace_all(s: &[char], at: impl Fn(&[char], usize) -> Option<(usize, String)>) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match at(s, i) {
            Some((end, by)) if end > i => {
                out.extend(by.chars());
                i = end;
            }
            _ => {
                out.push(s[i]);
                i += 1;
            }
        }
    }
    out
}

fn quote(c: char) -> bool {
    c == '"' || c == '\''
}

// /(authorization["']?\s*[:=]\s*["']?)(?:(?:bearer|basic|token)\s+)?[^\s"',;&]+/gi → `$1<oculto>`
fn authorization_at(s: &[char], i: usize) -> Option<(usize, String)> {
    let value = |c: char| !js_space(c) && !matches!(c, '"' | '\'' | ',' | ';' | '&');
    let mut j = lit_ci(s, i, "authorization")?;
    if j < s.len() && quote(s[j]) {
        j += 1;
    }
    j = run(s, j, js_space);
    if j >= s.len() || !matches!(s[j], ':' | '=') {
        return None;
    }
    j = run(s, j + 1, js_space);
    if j < s.len() && quote(s[j]) && j + 1 < s.len() && value(s[j + 1]) {
        j += 1;
    }
    let head: String = s[i..j].iter().collect();
    let scheme = ["bearer", "basic", "token"].iter().find_map(|w| {
        let k = lit_ci(s, j, w)?;
        let k2 = run(s, k, js_space);
        (k2 > k && k2 < s.len() && value(s[k2])).then_some(k2)
    });
    let start = scheme.unwrap_or(j);
    let end = run(s, start, value);
    (end > start).then(|| (end, format!("{head}{HIDDEN}")))
}

// /\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{6,}/gi → `$1 <oculto>`
fn scheme_at(s: &[char], i: usize) -> Option<(usize, String)> {
    if !boundary(s, i) {
        return None;
    }
    let cred = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '+' | '/' | '=' | '-');
    ["bearer", "basic"].iter().find_map(|w| {
        let k = lit_ci(s, i, w)?;
        let v = run(s, k, js_space);
        let end = run(s, v, cred);
        (v > k && end - v >= 6).then(|| (end, format!("{} {HIDDEN}", s[i..k].iter().collect::<String>())))
    })
}

// /\b([\w-]*(?:password|passwd|pwd|token|secret|api[_-]?key)["']?\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s&"',;]+)/gi
// → key kept; a quoted value keeps its quotes.
fn key_value_at(s: &[char], i: usize) -> Option<(usize, String)> {
    if !boundary(s, i) {
        return None;
    }
    let name_end = run(s, i, |c| word(c) || c == '-');
    // [\w-]* is greedy: the longest prefix that still lets the rest match wins.
    for k in (i..=name_end).rev() {
        let keywords = [
            lit_ci(s, k, "password"),
            lit_ci(s, k, "passwd"),
            lit_ci(s, k, "pwd"),
            lit_ci(s, k, "token"),
            lit_ci(s, k, "secret"),
            lit_ci(s, k, "api_key"),
            lit_ci(s, k, "api-key"),
            lit_ci(s, k, "apikey"),
        ];
        for after_key in keywords.into_iter().flatten() {
            let quote_opts: &[usize] = if after_key < s.len() && quote(s[after_key]) {
                &[after_key + 1, after_key]
            } else {
                &[after_key]
            };
            for &q in quote_opts {
                let sep = run(s, q, js_space);
                if sep >= s.len() || !matches!(s[sep], ':' | '=') {
                    continue;
                }
                let v = run(s, sep + 1, js_space);
                if v >= s.len() {
                    continue;
                }
                let key: String = s[i..v].iter().collect();
                let c = s[v];
                if quote(c) {
                    if let Some(close) = s[v + 1..].iter().position(|&x| x == c) {
                        return Some((v + close + 2, format!("{key}{c}{HIDDEN}{c}")));
                    }
                    continue;
                }
                let end = run(s, v, |c| !js_space(c) && !matches!(c, '&' | '"' | '\'' | ',' | ';'));
                if end > v {
                    return Some((end, format!("{key}{HIDDEN}")));
                }
            }
        }
    }
    None
}

// /\b(?:sk-ant-|sk-|xai-|ghp_|gho_|github_pat_|glpat-|xox[abpr]-|AIza)[A-Za-z0-9_-]{8,}/g → `<oculto>`
fn provider_key_at(s: &[char], i: usize) -> Option<(usize, String)> {
    if !boundary(s, i) {
        return None;
    }
    let body = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let slack = |s: &[char], i: usize| {
        let j = lit(s, i, "xox")?;
        (j + 1 < s.len() && matches!(s[j], 'a' | 'b' | 'p' | 'r') && s[j + 1] == '-').then_some(j + 2)
    };
    let prefixes = [
        lit(s, i, "sk-ant-"),
        lit(s, i, "sk-"),
        lit(s, i, "xai-"),
        lit(s, i, "ghp_"),
        lit(s, i, "gho_"),
        lit(s, i, "github_pat_"),
        lit(s, i, "glpat-"),
        slack(s, i),
        lit(s, i, "AIza"),
    ];
    prefixes.into_iter().flatten().find_map(|p| {
        let end = run(s, p, body);
        (end - p >= 8).then(|| (end, HIDDEN.to_string()))
    })
}

/// Hides secret-looking pieces of a text and leaves the rest as it was. Mirrors
/// `redactText()` in `windows/src/core/botlog.ts` pattern for pattern, so the
/// audit log and the bot log hide the same things.
pub fn redact(text: &str) -> String {
    let mut s: Vec<char> = text.chars().collect();
    s = replace_all(&s, authorization_at);
    s = replace_all(&s, scheme_at);
    s = replace_all(&s, key_value_at);
    s = replace_all(&s, provider_key_at);
    s.into_iter().collect()
}

// ── Autonomy ──────────────────────────────────────────────────────────────────
// The owner's switch in Settings (`assistantAutonomous`, off by default): ARIA
// acts without waiting for a click and the island tells the owner what she did.
// Only the protected core stays behind a click. The guard already refuses
// ARIA's own writes and evolutions there, but a shell or an MCP server never
// goes through it: so any call whose target names a core file asks, whatever
// the switch says. Bots never get autonomy (they do not come through here).

/// Names of the protected core as they would appear in a command or a path:
/// the last component of each entry ("policy.rs", "selfmod", "hook"…).
fn core_names() -> impl Iterator<Item = String> {
    crate::selfmod::guard::PROTECTED
        .iter()
        .filter_map(|rel| rel.rsplit('/').next())
        .map(|name| name.to_ascii_lowercase())
}

/// Whether a target (a command, a path, a tool's arguments) names a protected
/// core file or folder. Words are split on anything a path or a command
/// separates with, so `windows\hook\src` names `hook` but `aria-hook.exe` doesn't.
pub fn names_core(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .filter(|w| !w.is_empty())
        .collect();
    core_names().any(|name| words.contains(&name.as_str()))
}

fn autonomous(app: &AppHandle) -> bool {
    app.try_state::<crate::Shared>().is_some_and(|s| s.settings.lock().unwrap().assistant_autonomous)
}

/// Waits for the owner's click. `target` is exactly what the card shows: the
/// command, the path, the URL — what the click authorises.
pub async fn approve(app: &AppHandle, tool: &str, target: &str) -> bool {
    approve_with_detail(app, tool, target, None).await
}

/// Same, with the full text under review (a skill's code, a diff) shown in a
/// scrollable box on the card. "Always" is never offered for these.
pub async fn approve_with_detail(app: &AppHandle, tool: &str, target: &str, detail: Option<&str>) -> bool {
    if autonomous(app) && !names_core(target) {
        audit(tool, "allowed (autonomous)", target);
        let _ = app.emit_to(WINDOW_LABEL, "assistant-acted", json!({ "tool": tool, "target": redact(target) }));
        return true;
    }
    if detail.is_none() && allowed_by_rule(ARIA, tool, target) {
        audit(tool, "allowed (rule)", target);
        return true;
    }
    let id = format!("assistant-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, mut rx) = mpsc::channel::<Reply>(4);
    app.state::<Pending>().0.lock().unwrap().insert(id.clone(), tx);
    let _ = app.emit_to(
        WINDOW_LABEL,
        "assistant-approval",
        json!({
            "requestId": id,
            "tool": tool,
            "command": target,
            "detail": detail,
            "allowAlways": detail.is_none() && !never_always(tool),
        }),
    );

    // Reading a diff or a skill's code takes longer than reading a command.
    let timeout = if detail.is_some() { REVIEW_TIMEOUT } else { DECISION_TIMEOUT };
    let verdict = wait(&mut rx, timeout).await;
    app.state::<Pending>().0.lock().unwrap().remove(&id);

    match verdict.as_deref() {
        Some("allow") => {
            audit(tool, "allowed", target);
            true
        }
        Some(d @ ("always" | "always-session")) if detail.is_none() => {
            let made = remember_always(ARIA, tool, target, d);
            audit(tool, if made { "allowed (rule added)" } else { "allowed (once; no rule for this tool)" }, target);
            true
        }
        Some(_) => {
            audit(tool, "denied", target);
            false
        }
        None => {
            audit(tool, "not answered — denied", target);
            false
        }
    }
}

async fn wait(rx: &mut mpsc::Receiver<Reply>, timeout: Duration) -> Option<String> {
    match tokio::time::timeout(ACK_TIMEOUT, rx.recv()).await {
        Ok(Some(Reply::Ack)) => {}
        Ok(Some(Reply::Decision(d))) => return Some(d),
        _ => return None,
    }
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(Reply::Decision(d))) => Some(d),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aria-policy-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        d
    }

    #[test]
    fn shells_never_get_a_rule() {
        assert!(rule_for(ARIA, "run_powershell", "Get-ChildItem", Some(RULE_TTL)).is_none());
        assert!(rule_for("bot-a", "Shell", "ls", None).is_none());
        let forged = AllowRule { who: ARIA.into(), tool: "run_powershell".into(), scope: RuleScope::Tool, expires: None };
        assert!(!rule_covers(&forged, ARIA, "run_powershell", "x", Instant::now()));
    }

    #[test]
    fn autonomy_still_asks_for_anything_naming_the_core() {
        assert!(names_core(r"Set-Content C:\Users\Dell\coucou\windows\src-tauri\src\policy.rs 'x'"));
        assert!(names_core("Remove-Item -Recurse windows/hook"));
        assert!(names_core("cd src-tauri; rm -r capabilities"));
        assert!(names_core(r#"{"path":"C:\\Users\\Dell\\coucou\\CLAUDE.md"}"#));
        assert!(names_core("notepad core-directive.md"));
        assert!(!names_core(r"C:\Users\Dell\AppData\Local\ARIA\bin\aria-hook.exe --agent cursor"));
        assert!(!names_core("Get-ChildItem C:\\Users\\Dell\\Documents"));
        assert!(!names_core("send an email to someone about the policy"));
    }

    #[test]
    fn folder_rules_cover_the_folder_only() {
        let d = tmpdir("folder");
        let target = |p: &Path| format!("{} (10 chars, overwrite)", p.display());
        let rule = rule_for(ARIA, "write_file", &target(&d.join("a.txt")), Some(RULE_TTL)).unwrap();
        let now = Instant::now();
        assert!(rule_covers(&rule, ARIA, "write_file", &target(&d.join("new.txt")), now));
        assert!(rule_covers(&rule, ARIA, "write_file", &target(&d.join("sub").join("b.txt")), now));
        assert!(!rule_covers(&rule, ARIA, "write_file", &target(&d.parent().unwrap().join("x.txt")), now));
        let sibling = PathBuf::from(format!("{}x", d.display())).join("y.txt");
        assert!(!rule_covers(&rule, ARIA, "write_file", &target(&sibling), now), "prefix is per component");
        let detour = d.join("sub").join("..").join("..").join("escape.txt");
        assert!(!rule_covers(&rule, ARIA, "write_file", &target(&detour), now), "canonicalized");
        assert!(!rule_covers(&rule, "bot-a", "write_file", &target(&d.join("a.txt")), now), "never across callers");
        assert!(rule_for(ARIA, "write_file", "relative.txt (1 chars, append)", None).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tool_rules_are_per_caller_and_expire() {
        let rule = rule_for("bot-ventas", "open_url", "https://example.com", Some(Duration::from_secs(60))).unwrap();
        let now = Instant::now();
        assert!(rule_covers(&rule, "bot-ventas", "open_url", "https://other.example", now));
        assert!(!rule_covers(&rule, "bot-otro", "open_url", "https://example.com", now));
        assert!(!rule_covers(&rule, "bot-ventas", "open_app", "notepad", now));
        assert!(!rule_covers(&rule, "bot-ventas", "open_url", "x", now + Duration::from_secs(61)));
        let forever = rule_for(ARIA, "clipboard_write", "x", None).unwrap();
        assert!(rule_covers(&forever, ARIA, "clipboard_write", "y", now + Duration::from_secs(86_400)));
    }

    #[test]
    fn authorization_header_keeps_the_name() {
        assert_eq!(
            redact(r#"curl -H "Authorization: Bearer abc123def456" https://api.example.com"#),
            r#"curl -H "Authorization: <oculto>" https://api.example.com"#
        );
        assert_eq!(redact("authorization=xyz&a=1"), "authorization=<oculto>&a=1");
        assert_eq!(redact("AUTHORIZATION: token abcdef"), "AUTHORIZATION: <oculto>");
    }

    #[test]
    fn bearer_and_basic_values() {
        assert_eq!(redact("send Bearer eyJhbGciOi.payload.sig now"), "send Bearer <oculto> now");
        assert_eq!(redact("basic   dXNlcjpwYXNz"), "basic <oculto>");
        // Under six characters is not taken for a credential, as in botlog.ts.
        assert_eq!(redact("bearer abcde"), "bearer abcde");
    }

    #[test]
    fn query_token_keeps_the_rest_of_the_url() {
        assert_eq!(
            redact("GET https://x.dev/api?token=ghp_abcdefghijklmnop&page=2"),
            "GET https://x.dev/api?token=<oculto>&page=2"
        );
    }

    #[test]
    fn key_value_pairs() {
        assert_eq!(redact("login password=hunter2 ok"), "login password=<oculto> ok");
        assert_eq!(redact(r#"{"api_key": "abc def", "n": 1}"#), r#"{"api_key": "<oculto>", "n": 1}"#);
        assert_eq!(redact("client_secret='s3cr3t'"), "client_secret='<oculto>'");
        assert_eq!(redact("x-token: abc; next"), "x-token: <oculto>; next");
        assert_eq!(redact("passwd\t:\tq"), "passwd\t:\t<oculto>");
    }

    #[test]
    fn provider_key_prefixes() {
        for key in [
            "sk-abcdefghijkl",
            "sk-ant-api03-abcdefghij",
            "xai-ABCDEFGHIJKL",
            "ghp_1234567890ab",
            "gho_1234567890ab",
            "github_pat_11ABCDEFG_xyz",
            "glpat-abcdefghij",
            "xoxb-1234-5678-abcd",
            "xoxa-abcdefghij",
            "xoxp-abcdefghij",
            "xoxr-abcdefghij",
            "AIzaSyA-abcdefghijkl",
        ] {
            assert_eq!(redact(&format!("key {key} end")), "key <oculto> end", "{key}");
        }
        // Not a prefix at a word start, wrong case, too short, unknown Slack kind.
        for text in ["task-abcdefghijkl", "SK-abcdefghijkl", "ghp_short", "xoxc-abcdefghij"] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        let text = "cargo test --workspace && git status · café, 300 files; tokens are fine";
        assert_eq!(redact(text), text);
        assert_eq!(redact(""), "");
    }

    #[test]
    fn redaction_happens_before_the_cut() {
        // The key starts at 295: cut first, "sk-ab" would survive the cut and,
        // too short to match, leak half a key into the log.
        let target = format!("{} sk-abcdefghijklmnop", "a ".repeat(147));
        assert_eq!(target.find("sk-"), Some(295));
        let line = audit_target(&target);
        assert!(!line.contains("sk-"), "{line}");
        // 295 + "<oculto>" is over the cap: the marker itself gets cut, never the key.
        assert_eq!(line, format!("{} <ocul", "a ".repeat(147)));
        // Still capped at 300 characters, newlines flattened.
        assert_eq!(audit_target(&"x\n".repeat(400)).chars().count(), 300);
        assert!(!audit_target("a\nb").contains('\n'));
    }
}
