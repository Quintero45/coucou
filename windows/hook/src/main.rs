//! coucou-hook — the relay Claude Code runs on every hook event.
//!
//! Reads the hook JSON on stdin, adds a little terminal context, and hands it to
//! Coucou over the named pipe `\\.\pipe\coucou-<sid>` (Windows) or the Unix
//! socket `$XDG_RUNTIME_DIR/coucou.sock` (Linux).
//!
//! Hard rule (docs/CLAUDE.md): **never block Claude Code.**
//! * If the pipe does not exist — Coucou is closed — we exit 0 immediately with
//!   nothing on stdout, and the session carries on untouched.
//! * Every step runs under a deadline enforced by the main thread, so a pipe that
//!   accepts the connection and then stops reading cannot wedge the session
//!   either: we abandon the worker and exit.
//! * Only `PermissionRequest` and the question tools (Claude Code's
//!   AskUserQuestion, Cursor's AskQuestion) wait for an answer, because
//!   answering from the island is the whole point. No answer means empty
//!   stdout, and the agent asks in its own UI exactly as if Coucou were not
//!   installed (Cursor gets an explicit `ask`, or `allow` for its questions).
//!
//! Usage: `coucou-hook [--agent <name>] [--approve] [--ask] [<EventName>]`
//! (the event name is also read from the JSON). See normalize.rs for the
//! per-agent dialects, bot.rs for `--bot`, which Grok Bots use, and tool.rs for
//! `coucou-hook tool …`, through which they call Mochi's tools.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

mod bot;
mod normalize;
mod tool;
use normalize::{Context, Kind};

/// Budget for getting a pipe connection. Beyond this the agent wins, always.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// Whole-run budget for an event nobody waits on: connect and write, no more.
const FIRE_AND_FORGET_BUDGET: Duration = Duration::from_secs(2);
/// How long a permission prompt may stay on screen before the terminal takes over.
const DECISION_BUDGET: Duration = Duration::from_secs(110);
/// AskUserQuestion is installed with a 130 s timeout; answer a little before.
const ASK_BUDGET: Duration = Duration::from_secs(125);

/// Fields that are pointless to forward and can be enormous (a whole file read,
/// a full command output). The island never shows them.
const DROPPED_FIELDS: &[&str] = &["tool_response", "transcript_path"];
/// Longest string forwarded for any single field; the island truncates to far
/// less than this anyway.
const MAX_FIELD_LEN: usize = 2_000;
/// Edit payloads feed the live diff, so they get far more room.
const DIFF_FIELDS: &[&str] = &["content", "old_string", "new_string", "edits"];
const MAX_DIFF_LEN: usize = 120_000;

#[cfg(windows)]
mod win;
#[cfg(windows)]
use win::connect;

#[cfg(target_os = "linux")]
mod unix;
#[cfg(target_os = "linux")]
use unix::connect;

fn main() {
    // A Grok Bot calling one of Mochi's tools. Checked before --bot, which a
    // tool call also carries.
    if let Some(code) = tool::run() {
        std::process::exit(code);
    }
    // A Grok Bot reporting in: arguments only, never stdin.
    if let Some(code) = bot::run() {
        std::process::exit(code);
    }
    let Some(ev) = read_event() else { std::process::exit(0) };

    let budget = match ev.kind {
        Kind::Permission => DECISION_BUDGET,
        Kind::Ask => ASK_BUDGET,
        Kind::Fire => FIRE_AND_FORGET_BUDGET,
    };
    let waits = ev.kind != Kind::Fire;

    // The worker owns every blocking call. If it overruns the budget we simply
    // stop listening and exit: the process dying takes the pipe handle with it.
    // (No catch_unwind here — the release profile is panic = "abort", so it would
    // be dead code. `talk` is written to have nothing to panic on instead.)
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let line = ev.line.clone();
    std::thread::spawn(move || {
        let _ = tx.send(talk(&line, waits));
    });

    let raw = rx.recv_timeout(budget).ok().flatten();
    let reply = raw.as_deref().and_then(normalize::parse_reply);
    if let Some(json) = normalize::output(&ev.agent, ev.kind, reply.as_ref(), &ev.ctx) {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{json}");
        let _ = out.flush();
    }
    // Nothing printed: the agent asks in its own UI, as if we were not here.
    std::process::exit(0);
}

/// One hook invocation, ready to forward.
struct Event {
    line: String,
    agent: String,
    kind: Kind,
    ctx: Context,
}

/// Reads stdin and returns the payload to forward plus what to do with it.
fn read_event() -> Option<Event> {
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() || raw.is_empty() {
        return None;
    }
    // Some shells hand us a UTF-8 BOM; serde_json would choke on it.
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        raw.drain(..3);
    }

    let mut payload = serde_json::from_slice::<serde_json::Value>(&raw).ok()?;
    let map = payload.as_object_mut()?;

    // Parse argv: "coucou-hook.exe [--agent <name>] [--approve] [--ask] [<EventName>]"
    // --agent tags the payload with coucou_agent so the app routes to the right pill.
    // Absent or invalid names are validated and discarded by the app, not here.
    let mut agent = String::new();
    let mut arg_event = String::new();
    let mut approve = false;
    let mut ask = false;
    {
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--agent" => agent = it.next().unwrap_or_default(),
                "--approve" => approve = true,
                "--ask" => ask = true,
                _ if arg_event.is_empty() => arg_event = arg,
                _ => {}
            }
        }
    }
    // Which agent this hook was installed for. Absent means Claude Code,
    // so existing hook commands keep working unchanged.
    if !agent.is_empty() {
        map.insert("coucou_agent".into(), serde_json::Value::String(agent.clone()));
    }
    let kind = normalize::normalize(map, &agent, &arg_event, approve, ask);
    // Captured before truncation: the answer has to echo these back verbatim.
    let ctx = Context::from(map);

    for field in DROPPED_FIELDS {
        map.remove(*field);
    }

    let cwd_missing = map
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(str::is_empty)
        .unwrap_or(true);
    if cwd_missing {
        if let Ok(cwd) = std::env::current_dir() {
            map.insert(
                "cwd".into(),
                serde_json::Value::String(cwd.to_string_lossy().to_string()),
            );
        }
    }

    // Which terminal the session runs in. Unlike macOS, Coucou here accepts
    // events from every terminal, so this is context only — never a filter.
    for (key, var) in [
        ("term_program", "TERM_PROGRAM"),
        ("wt_session", "WT_SESSION"),
        ("term_session_id", "TERM_SESSION_ID"),
        ("vscode_pid", "VSCODE_PID"),
        ("session_pid", "CLAUDE_CODE_SSE_PORT"),
    ] {
        if !map.contains_key(key) {
            let value = std::env::var(var).unwrap_or_default();
            map.insert(key.into(), serde_json::Value::String(value));
        }
    }

    truncate_strings(&mut payload);

    let mut line = payload.to_string();
    line.push('\n');
    Some(Event { line, agent, kind, ctx })
}

/// Caps every string in the payload. A single Write can carry a whole file, so
/// even the diff fields have a ceiling — just a much higher one.
fn truncate_strings(value: &mut serde_json::Value) {
    truncate_with(value, MAX_FIELD_LEN);
}

fn truncate_with(value: &mut serde_json::Value, limit: usize) {
    match value {
        serde_json::Value::String(s) => {
            if s.len() > limit {
                // Cut on a char boundary; a lone byte index can split UTF-8.
                let mut end = limit;
                while end > 0 && !s.is_char_boundary(end) {
                    end -= 1;
                }
                s.truncate(end);
                s.push('…');
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| truncate_with(v, limit)),
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                let l = if DIFF_FIELDS.contains(&k.as_str()) { MAX_DIFF_LEN.max(limit) } else { limit };
                truncate_with(v, l);
            }
        }
        _ => {}
    }
}

/// Connect, send, and — for a permission request — wait for the island's word.
fn talk(payload: &str, waits_for_answer: bool) -> Option<String> {
    let mut pipe = connect()?;

    if pipe.write_all(payload.as_bytes()).is_err() {
        return None;
    }
    let _ = pipe.flush();

    if !waits_for_answer {
        return None;
    }

    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let answer = String::from_utf8_lossy(&buf).trim().to_string();
    (!answer.is_empty()).then_some(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anything_unrecognised_prints_nothing() {
        let ctx = Context::default();
        for raw in ["", "maybe", r#"{"permissionDecision":"allow"}"#] {
            let reply = normalize::parse_reply(raw);
            assert!(normalize::output("", Kind::Permission, reply.as_ref(), &ctx).is_none(), "{raw}");
        }
    }

    #[test]
    fn long_strings_are_cut_on_a_char_boundary() {
        let mut v = serde_json::json!({ "tool_input": { "command": "é".repeat(4000) } });
        truncate_strings(&mut v);
        let s = v["tool_input"]["command"].as_str().unwrap();
        assert!(s.len() <= MAX_FIELD_LEN + 4);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn diff_fields_keep_far_more_than_other_fields() {
        let mut v = serde_json::json!({ "tool_input": { "content": "a".repeat(10_000), "command": "b".repeat(10_000) } });
        truncate_strings(&mut v);
        assert_eq!(v["tool_input"]["content"].as_str().unwrap().len(), 10_000);
        assert!(v["tool_input"]["command"].as_str().unwrap().len() <= MAX_FIELD_LEN + 4);
    }
}
