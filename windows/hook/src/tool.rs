//! `coucou-hook tool <name> [<json>|-] --bot <name> [--timeout <secs>]` and
//! `coucou-hook tool --list` — how the owner's Grok Bots use Mochi's own tools
//! on this computer.
//!
//! The call goes to the running app over the same pipe as every hook event.
//! The app looks the tool up in Mochi's registry (`tools/`), runs reads at once,
//! and puts every side effect on the island as that Bot's approval card
//! (Permitir / Denegar) — exactly the card `--bot … --status ask` raises. Each
//! call, allowed or not, is written to coucou.log. Nothing here can approve.
//!
//! `--timeout` is how long the owner has to answer the card (default 180 s).
//! The app enforces it; we wait that long plus the time the tool itself may
//! take (`exec_grace`), so an approved call is not cut off while it runs.
//!
//! Output is always one JSON line on stdout:
//!   `{"ok":true,"tool":"…","result":"…"}`, exit 0, or
//!   `{"ok":false,"error":"<code>","message":"…"}` with a non-zero exit (see
//!   `Fail::code`). Unlike the hook path, a closed app is an error here: the Bot
//!   asked for something to happen, and must know it did not.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{json, Value};

/// Default time the owner has to answer the card: they may be away from the screen.
pub const DEFAULT_TIMEOUT_SECS: u64 = 180;
/// Time for the tool to run once approved, on top of the approval wait.
const EXEC_GRACE_SECS: u64 = 60;
/// Same ceiling as run_powershell's own `timeout_secs`.
const MAX_TOOL_TIMEOUT_SECS: u64 = 600;
const MIN_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 1800;
/// Listing needs no human: answer fast or not at all.
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
/// The app refuses anything bigger (pipe.rs MAX_PAYLOAD); say so here instead.
const MAX_REQUEST: usize = (1 << 20) - 1024;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Call { bot: String, tool: String, input: Value, timeout_secs: u64 },
    List { bot: String },
}

#[derive(Debug, PartialEq)]
pub enum Fail {
    Usage(String),
    InvalidJson(String),
    AppNotRunning,
    Timeout,
    /// The app answered with `ok:false`; its code and message are passed on.
    App { error: String, message: String },
}

impl Fail {
    /// sysexits-style, so a script can branch without parsing.
    pub fn code(&self) -> i32 {
        match self {
            Fail::Usage(_) | Fail::InvalidJson(_) => 64,
            Fail::AppNotRunning => 69,
            Fail::Timeout => 75,
            Fail::App { error, .. } => match error.as_str() {
                "denied" | "not_answered" => 2,
                "unknown_tool" | "not_allowed" => 3,
                "invalid_request" => 64,
                _ => 1, // tool_error and anything newer
            },
        }
    }

    pub fn to_json(&self) -> Value {
        let (error, message) = match self {
            Fail::Usage(m) => ("usage", m.clone()),
            Fail::InvalidJson(m) => ("invalid_json", m.clone()),
            Fail::AppNotRunning => ("app_not_running", "Coucou is not running (no relay pipe)".to_string()),
            Fail::Timeout => (
                "timeout",
                "no answer from Coucou in time; if it was already approved, the action may still have run".to_string(),
            ),
            Fail::App { error, message } => (error.as_str(), message.clone()),
        };
        json!({ "ok": false, "error": error, "message": message })
    }
}

/// None when argv is not a `tool` call (the first argument must be `tool`).
pub fn parse(args: &[String], stdin: impl FnOnce() -> Option<String>) -> Option<Result<Cmd, Fail>> {
    if args.first().map(String::as_str) != Some("tool") {
        return None;
    }
    Some(parse_rest(&args[1..], stdin))
}

fn parse_rest(args: &[String], stdin: impl FnOnce() -> Option<String>) -> Result<Cmd, Fail> {
    let mut bot = String::new();
    let mut timeout: Option<String> = None;
    let mut list = false;
    let mut positional = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bot" => bot = it.next().cloned().unwrap_or_default(),
            "--timeout" => timeout = Some(it.next().cloned().unwrap_or_default()),
            "--list" => list = true,
            _ => positional.push(arg.clone()),
        }
    }
    let bot = bot.trim().to_string();
    if list {
        return Ok(Cmd::List { bot });
    }
    if crate::bot::slug(&bot).is_empty() {
        return Err(Fail::Usage("every tool call needs the Bot's name: --bot \"Name\"".into()));
    }
    let timeout_secs = match timeout {
        None => DEFAULT_TIMEOUT_SECS,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if (MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&n) => n,
            _ => {
                return Err(Fail::Usage(format!(
                    "--timeout takes whole seconds from {MIN_TIMEOUT_SECS} to {MAX_TIMEOUT_SECS}"
                )))
            }
        },
    };
    let mut positional = positional.into_iter();
    let tool = positional.next().unwrap_or_default();
    if tool.is_empty() || tool.starts_with('-') {
        return Err(Fail::Usage(
            "usage: coucou-hook tool <name> [<json>|-] --bot <name> [--timeout <secs>]  |  coucou-hook tool --list".into(),
        ));
    }
    // `-` reads the input from stdin: quoting JSON for PowerShell is a trap.
    let raw = match positional.next() {
        None => "{}".to_string(),
        Some(s) if s == "-" => stdin().ok_or_else(|| Fail::InvalidJson("could not read the input from stdin".into()))?,
        Some(s) => s,
    };
    if let Some(extra) = positional.next() {
        return Err(Fail::Usage(format!(
            "unexpected argument {extra:?}: pass the input as ONE JSON argument (or - for stdin)"
        )));
    }
    let input = parse_input(&raw)?;
    Ok(Cmd::Call { bot, tool, input, timeout_secs })
}

fn parse_input(raw: &str) -> Result<Value, Fail> {
    let raw = raw.trim_start_matches('\u{feff}').trim();
    let raw = if raw.is_empty() { "{}" } else { raw };
    match serde_json::from_str::<Value>(raw) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err(Fail::InvalidJson("the input must be a JSON object, e.g. {\"path\":\"~/notes.txt\"}".into())),
        Err(e) => Err(Fail::InvalidJson(format!("the input is not valid JSON: {e}"))),
    }
}

/// The one line sent to the app. `coucou_kind` routes it in pipe.rs; the
/// `bot-<slug>` / `coucou_bot` pair is the same attribution `--bot` sends.
pub fn request_line(cmd: &Cmd) -> String {
    let mut v = match cmd {
        Cmd::Call { bot, tool, input, timeout_secs } => json!({
            "coucou_kind": "tool",
            "coucou_agent": format!("bot-{}", crate::bot::slug(bot)),
            "coucou_bot": bot,
            "tool_name": tool,
            "tool_input": input,
            "timeout_ms": timeout_secs * 1000,
        }),
        Cmd::List { bot } => json!({ "coucou_kind": "tool_list", "coucou_bot": bot }),
    }
    .to_string();
    v.push('\n');
    v
}

/// The app's answer line, as the JSON we print, or the failure to report.
pub fn parse_answer(raw: &str) -> Result<Value, Fail> {
    let v: Value = serde_json::from_str(raw.trim()).map_err(|_| Fail::App {
        error: "bad_answer".into(),
        message: "Coucou answered something unreadable (older app without `tool` support?)".into(),
    })?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v);
    }
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Err(Fail::App { error: if s("error").is_empty() { "tool_error".into() } else { s("error") }, message: s("message") })
}

/// How long the tool may run after the click: a fixed grace, plus the tool's
/// own `timeout_secs` when it takes one (run_powershell).
pub fn exec_grace(input: &Value) -> u64 {
    let own = input.get("timeout_secs").and_then(Value::as_u64).unwrap_or(0).min(MAX_TOOL_TIMEOUT_SECS);
    EXEC_GRACE_SECS + own
}

enum Talk {
    NoPipe,
    Answer(String),
    Silent,
}

fn exchange(line: &str) -> Talk {
    let Some(mut pipe) = crate::connect() else { return Talk::NoPipe };
    if pipe.write_all(line.as_bytes()).is_err() {
        return Talk::Silent;
    }
    let _ = pipe.flush();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
        }
    }
    let answer = String::from_utf8_lossy(&buf).trim().to_string();
    if answer.is_empty() { Talk::Silent } else { Talk::Answer(answer) }
}

fn read_stdin() -> Option<String> {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    Some(raw)
}

fn call(cmd: &Cmd) -> Result<Value, Fail> {
    let line = request_line(cmd);
    if line.len() > MAX_REQUEST {
        return Err(Fail::InvalidJson("the input is too large for the relay (1 MB)".into()));
    }
    let budget = match cmd {
        Cmd::Call { timeout_secs, input, .. } => Duration::from_secs(timeout_secs + exec_grace(input)),
        Cmd::List { .. } => LIST_TIMEOUT,
    };
    // Same pattern as main: the worker owns every blocking call, the main
    // thread owns the deadline.
    let (tx, rx) = mpsc::channel::<Talk>();
    std::thread::spawn(move || {
        let _ = tx.send(exchange(&line));
    });
    match rx.recv_timeout(budget) {
        Ok(Talk::NoPipe) => Err(Fail::AppNotRunning),
        Ok(Talk::Answer(a)) => parse_answer(&a),
        // The app hung up without a word (old build, crash): not a timeout.
        Ok(Talk::Silent) => Err(Fail::App {
            error: "no_answer".into(),
            message: "Coucou closed the connection without answering (older app without `tool` support?)".into(),
        }),
        Err(_) => Err(Fail::Timeout),
    }
}

/// None when this is not a `tool` call; otherwise the process exit code.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = parse(&args, read_stdin)?.and_then(|cmd| call(&cmd));
    let (json, code) = match result {
        Ok(v) => (v, 0),
        Err(f) => (f.to_json(), f.code()),
    };
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{json}");
    let _ = out.flush();
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }
    fn no_stdin() -> Option<String> {
        None
    }

    #[test]
    fn only_the_tool_subcommand_is_ours() {
        assert!(parse(&args(&["--bot", "Aerys", "tool", "listo"]), no_stdin).is_none());
        assert!(parse(&args(&["PreToolUse"]), no_stdin).is_none());
        assert!(parse(&args(&[]), no_stdin).is_none());
    }

    #[test]
    fn parses_a_call_in_any_order() {
        let c = parse(&args(&["tool", "--bot", "Aerys", "read_file", r#"{"path":"~/a.txt"}"#, "--timeout", "60"]), no_stdin)
            .unwrap()
            .unwrap();
        assert_eq!(
            c,
            Cmd::Call { bot: "Aerys".into(), tool: "read_file".into(), input: json!({"path":"~/a.txt"}), timeout_secs: 60 }
        );
        let c = parse(&args(&["tool", "system_info", "--bot", "Daemond"]), no_stdin).unwrap().unwrap();
        assert_eq!(
            c,
            Cmd::Call { bot: "Daemond".into(), tool: "system_info".into(), input: json!({}), timeout_secs: DEFAULT_TIMEOUT_SECS }
        );
    }

    #[test]
    fn dash_reads_stdin_and_strips_a_bom() {
        let c = parse(&args(&["tool", "write_file", "-", "--bot", "Aegon"]), || Some("\u{feff}{\"path\":\"x\"}\n".into()))
            .unwrap()
            .unwrap();
        assert!(matches!(c, Cmd::Call { input, .. } if input == json!({"path":"x"})));
    }

    #[test]
    fn list_needs_no_bot() {
        assert_eq!(parse(&args(&["tool", "--list"]), no_stdin).unwrap().unwrap(), Cmd::List { bot: String::new() });
    }

    #[test]
    fn rejects_bad_calls() {
        let err = |a: &[&str]| parse(&args(a), no_stdin).unwrap().unwrap_err();
        assert!(matches!(err(&["tool", "read_file", "{}"]), Fail::Usage(_)), "no --bot");
        assert!(matches!(err(&["tool", "--bot", "¿?", "read_file"]), Fail::Usage(_)), "empty slug");
        assert!(matches!(err(&["tool", "--bot", "A"]), Fail::Usage(_)), "no tool");
        assert!(matches!(err(&["tool", "--bot", "A", "x", "{}", "--timeout", "0"]), Fail::Usage(_)));
        assert!(matches!(err(&["tool", "--bot", "A", "x", "{}", "--timeout", "abc"]), Fail::Usage(_)));
        assert!(matches!(err(&["tool", "--bot", "A", "x", "{bad"]), Fail::InvalidJson(_)));
        assert!(matches!(err(&["tool", "--bot", "A", "x", "[1,2]"]), Fail::InvalidJson(_)));
        assert!(matches!(err(&["tool", "--bot", "A", "x", "{\"a\":", "1}"]), Fail::Usage(_)), "split JSON");
        assert!(matches!(err(&["tool", "--bot", "A", "x", "-"]), Fail::InvalidJson(_)), "stdin unreadable");
    }

    #[test]
    fn request_carries_the_bot_attribution_of_bot_rs() {
        let c = Cmd::Call { bot: "Diseño Bot".into(), tool: "open_app".into(), input: json!({"target":"notepad"}), timeout_secs: 30 };
        let line = request_line(&c);
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["coucou_kind"], "tool");
        assert_eq!(v["coucou_agent"], "bot-diseno-bot");
        assert_eq!(v["coucou_bot"], "Diseño Bot");
        assert_eq!(v["tool_name"], "open_app");
        assert_eq!(v["tool_input"], json!({"target":"notepad"}));
        assert_eq!(v["timeout_ms"], 30_000);
        let v: Value = serde_json::from_str(&request_line(&Cmd::List { bot: String::new() })).unwrap();
        assert_eq!(v["coucou_kind"], "tool_list");
    }

    #[test]
    fn the_wait_covers_the_tool_run_too() {
        assert_eq!(exec_grace(&json!({})), EXEC_GRACE_SECS);
        assert_eq!(exec_grace(&json!({"timeout_secs": 120})), EXEC_GRACE_SECS + 120);
        assert_eq!(exec_grace(&json!({"timeout_secs": 99_999})), EXEC_GRACE_SECS + MAX_TOOL_TIMEOUT_SECS);
    }

    #[test]
    fn answers_map_to_output_and_exit_codes() {
        let ok = parse_answer(r#"{"ok":true,"tool":"read_file","result":"hola"}"#).unwrap();
        assert_eq!(ok["result"], "hola");
        let denied = parse_answer(r#"{"ok":false,"error":"denied","message":"no"}"#).unwrap_err();
        assert_eq!(denied.code(), 2);
        assert_eq!(denied.to_json()["error"], "denied");
        assert_eq!(parse_answer(r#"{"ok":false,"error":"unknown_tool"}"#).unwrap_err().code(), 3);
        assert_eq!(parse_answer(r#"{"ok":false,"error":"tool_error","message":"x"}"#).unwrap_err().code(), 1);
        assert_eq!(parse_answer("allow").unwrap_err().code(), 1);
        assert_eq!(Fail::AppNotRunning.code(), 69);
        assert_eq!(Fail::Timeout.code(), 75);
        assert_eq!(Fail::InvalidJson(String::new()).to_json()["ok"], false);
    }
}
