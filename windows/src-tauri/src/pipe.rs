// Relay server for coucou-hook.
//
// Windows: the named pipe `\\.\pipe\coucou-<sid>`, one instance per connection.
// Linux: the Unix socket `$XDG_RUNTIME_DIR/coucou.sock`. Every hook event is
// forwarded to the island as a `hook` event. `PermissionRequest` and
// AskUserQuestion (`coucou_kind: ask_user_question`) are the only ones that keep
// their connection open: they wait for the island's decision and write it back
// on the same connection, which is how answering from the island works.
//
// Claude Code is never blocked by us. Three things guarantee it:
//   * coucou-hook gives the connection 300 ms and exits cleanly if we are closed;
//   * we only wait for a human once the island has *confirmed* the card is on
//     screen, so a paused island or a webview that is not listening costs a few
//     hundred milliseconds, not two minutes;
//   * whatever happens we drop the connection after the decision timeout, and
//     the terminal takes over.
//
// What we write back is the bare word `allow`, `always` or `deny`, or a JSON
// line `{"decision":"answer","answers":{…}}`. Turning that into each agent's
// documented output is coucou-hook's job, so the wire formats live in one place.
//
// `coucou-hook tool` (Grok Bots calling Mochi's tools) is the one request that
// is not a hook event: `coucou_kind: "tool"` / `"tool_list"`, answered with one
// JSON line `{"ok":…}`. See `tool_request` below.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::mpsc;

use crate::island::WINDOW_LABEL;
use crate::log;
use crate::policy::{self, Risk};
use crate::tools;

/// Slightly under coucou-hook's own 110 s wait, so we always answer first.
const DECISION_TIMEOUT: Duration = Duration::from_secs(108);
/// How long the island gets to say "the card is up". This is the whole of B4:
/// without it, an island that is paused, hidden behind a crashed webview or
/// simply not listening would leave Claude Code staring at a prompt nobody can
/// see for nearly two minutes.
const ACK_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_PAYLOAD: usize = 1 << 20;
/// How long the owner has to answer a Grok Bot's tool card, unless the call
/// says otherwise (`coucou-hook tool --timeout`, which this must match).
const TOOL_DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);
const TOOL_MIN_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_MAX_TIMEOUT: Duration = Duration::from_secs(1800);

/// What the island can say about a permission request.
pub enum Reply {
    /// The card is on screen and a human can act on it.
    Ack,
    /// A human clicked: `allow` or `deny`.
    Decision(String),
    /// Nobody can act on it — paused, or another request already holds the card.
    Decline,
}

/// Permission requests the island has been told about.
#[derive(Default)]
pub struct Pending(pub Mutex<HashMap<String, mpsc::Sender<Reply>>>);

static COUNTER: AtomicU64 = AtomicU64::new(1);

/// `\\.\pipe\coucou-<sid>` — must match coucou-hook's `pipe_path()` exactly.
#[cfg(windows)]
pub fn pipe_name() -> String {
    let key = crate::platform::current_user_sid()
        .unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    format!(r"\\.\pipe\coucou-{key}")
}

#[cfg(windows)]
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let name = pipe_name();
        // first_pipe_instance also means we refuse to join a pipe somebody else
        // already owns under our name, rather than serving on top of it.
        let mut server = match ServerOptions::new().first_pipe_instance(true).create(&name) {
            Ok(s) => s,
            Err(err) => {
                log::line(format!("cannot open the relay pipe: {err}"));
                return;
            }
        };
        loop {
            if server.connect().await.is_err() {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            // Hand the connected instance to a task and listen on a fresh one.
            let next = match ServerOptions::new().create(&name) {
                Ok(s) => s,
                Err(err) => {
                    log::line(format!("cannot reopen the relay pipe: {err}"));
                    return;
                }
            };
            let connected = std::mem::replace(&mut server, next);
            let app = app.clone();
            tauri::async_runtime::spawn(async move { handle(app, connected).await });
        }
    });
}

#[cfg(target_os = "linux")]
pub fn start(app: AppHandle) {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    tauri::async_runtime::spawn(async move {
        let Some(path) = crate::platform::relay_socket_path() else {
            log::line("no private runtime directory ($XDG_RUNTIME_DIR) — Claude Code hooks are inactive");
            return;
        };
        // A socket file left behind by a crash answers nothing and can go. One
        // that answers belongs to a Coucou that is still running: like
        // first_pipe_instance on Windows, we refuse to serve on top of it.
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                log::line("another Coucou already serves the relay socket");
                return;
            }
            let _ = std::fs::remove_file(&path);
        }
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(err) => {
                log::line(format!("cannot open the relay socket: {err}"));
                return;
            }
        };
        // The runtime directory is already 0700; this is belt and braces.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let uid = unsafe { libc::getuid() };
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            // Only the relay run by our own user may drive the island.
            if !matches!(stream.peer_cred(), Ok(c) if c.uid() == uid) {
                log::line("refused a relay connection from another user");
                continue;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move { handle(app, stream).await });
        }
    });
}

/// One accepted relay connection, whatever carries it.
trait Relay: AsyncRead + AsyncWrite + Unpin {
    /// Ends the conversation once everything has been written.
    fn finish(&mut self) {}
}

#[cfg(windows)]
impl Relay for NamedPipeServer {
    fn finish(&mut self) {
        let _ = self.disconnect();
    }
}

/// Dropping the stream closes it; the relay reads up to our newline first.
#[cfg(target_os = "linux")]
impl Relay for tokio::net::UnixStream {}

async fn handle(app: AppHandle, mut pipe: impl Relay) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.contains(&b'\n') || buf.len() > MAX_PAYLOAD {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let line = match buf.iter().position(|b| *b == b'\n') {
        Some(i) => &buf[..i],
        None => &buf[..],
    };
    let Ok(mut payload) = serde_json::from_slice::<Value>(line) else { return };
    if !payload.is_object() {
        return;
    }

    // A Grok Bot calling one of Mochi's tools: always answered, never relayed.
    match payload.get("coucou_kind").and_then(Value::as_str) {
        Some("tool") => {
            let answer = tool_request(&app, payload).await;
            return write_line(pipe, &answer).await;
        }
        Some("tool_list") => {
            log::line("bot tool list");
            return write_line(pipe, &tool_list()).await;
        }
        _ => {}
    }

    let event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_question = payload.get("coucou_kind").and_then(Value::as_str) == Some("ask_user_question");

    if event != "PermissionRequest" && !is_question {
        log::line(format!("hook {event}"));
        let _ = app.emit_to(WINDOW_LABEL, "hook", payload);
        pipe.finish();
        return;
    }

    let id = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, mut rx) = mpsc::channel::<Reply>(4);
    {
        let pending = app.state::<Pending>();
        pending.0.lock().unwrap().insert(id.clone(), tx);
    }
    payload["request_id"] = json!(id);
    log::line(format!("hook {} id={id}", if is_question { "AskUserQuestion" } else { "PermissionRequest" }));
    let _ = app.emit_to(WINDOW_LABEL, "hook", payload);

    let decision = wait_for_decision(&id, &mut rx).await;
    app.state::<Pending>().0.lock().unwrap().remove(&id);

    // No decision: say nothing at all. coucou-hook then writes nothing to stdout
    // and Claude Code asks in the terminal, exactly as if Coucou were closed.
    if let Some(d) = decision {
        let _ = pipe.write_all(format!("{d}\n").as_bytes()).await;
        let _ = pipe.flush().await;
    }
    pipe.finish();
}

/// Two waits: a short one for "the card is up", then the long one for a human.
async fn wait_for_decision(id: &str, rx: &mut mpsc::Receiver<Reply>) -> Option<String> {
    wait_for_decision_within(id, rx, DECISION_TIMEOUT).await
}

/// The same, with the human's wait given by the caller.
async fn wait_for_decision_within(id: &str, rx: &mut mpsc::Receiver<Reply>, limit: Duration) -> Option<String> {
    match tokio::time::timeout(ACK_TIMEOUT, rx.recv()).await {
        Ok(Some(Reply::Ack)) => {}
        // A click that beats the ack is still a click.
        Ok(Some(Reply::Decision(d))) => {
            log::line(format!("hook id={id} answered {d}"));
            return Some(d);
        }
        Ok(Some(Reply::Decline)) => {
            log::line(format!("hook id={id} not shown — terminal takes over"));
            return None;
        }
        Ok(None) => return None,
        Err(_) => {
            log::line(format!("hook id={id} island never acknowledged — terminal takes over"));
            return None;
        }
    }

    match tokio::time::timeout(limit, rx.recv()).await {
        Ok(Some(Reply::Decision(d))) => {
            log::line(format!("hook id={id} answered {d}"));
            Some(d)
        }
        Ok(Some(Reply::Decline)) => {
            log::line(format!("hook id={id} released without a decision"));
            None
        }
        _ => {
            log::line(format!("hook id={id} timed out — terminal takes over"));
            None
        }
    }
}

fn send(app: &AppHandle, request_id: &str, reply: Reply, keep: bool) {
    let sender = {
        let pending = app.state::<Pending>();
        let mut map = pending.0.lock().unwrap();
        if keep { map.get(request_id).cloned() } else { map.remove(request_id) }
    };
    match sender {
        Some(tx) => {
            let _ = tx.try_send(reply);
        }
        None => log::line(format!("reply for id={request_id} — no pending request")),
    }
}

/// The island has the card on screen; the long wait may begin.
pub fn acknowledge(app: &AppHandle, request_id: &str) {
    send(app, request_id, Reply::Ack, true);
}

/// Nobody can act on this one — paused, or another card already holds the view.
pub fn decline(app: &AppHandle, request_id: &str) {
    log::line(format!("decline id={request_id}"));
    send(app, request_id, Reply::Decline, false);
}

/// Called by the island's Allow / Always / Deny buttons. Only ever a bare word:
/// turning it into each agent's JSON is coucou-hook's job.
pub fn answer(app: &AppHandle, request_id: &str, decision: &str) {
    let word = match decision {
        "allow" => "allow",
        "always" => "always",
        _ => "deny",
    };
    log::line(format!("decision id={request_id} {word}"));
    send(app, request_id, Reply::Decision(word.to_string()), false);
}

/// The answers to an AskUserQuestion card, as `{ question text: chosen label(s) }`.
pub fn answer_questions(app: &AppHandle, request_id: &str, answers: Value) {
    if !answers.is_object() {
        return;
    }
    log::line(format!("decision id={request_id} answer"));
    let line = json!({ "decision": "answer", "answers": answers }).to_string();
    send(app, request_id, Reply::Decision(line), false);
}

// ── Grok Bots calling Mochi's tools (`coucou-hook tool`) ─────────────────────
//
// Same registry as Mochi (tools::for_bots, built-ins only), same rule as
// tools::run: a read is audited and runs, a side effect waits for the owner's
// click. The card is the one `coucou-hook --bot … --status ask` already raises
// — a PermissionRequest hook with coucou_agent `bot-<slug>`, coucou_bot,
// tool_name and tool_input — so it lands on that Bot's pill, goes through the
// Pending / acknowledge / answer path above, and nothing new is needed in the
// island. coucou.log gets Mochi's audit line (policy::audit) for reads and
// refusals, the request line below for a card, and the decision line the
// island's answer already writes (`decision id=… allow|deny`) — nothing more,
// so a click is never logged twice. "Always" is never remembered for a Bot:
// each side effect is its own click.

/// What `coucou-hook tool` sends (hook/src/tool.rs `request_line`).
#[derive(Debug, Deserialize)]
struct ToolRequestWire {
    #[serde(default)]
    coucou_agent: String,
    #[serde(default)]
    coucou_bot: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    #[serde(default)]
    timeout_ms: u64,
}

/// A checked request.
#[derive(Debug, PartialEq)]
struct ToolRequest {
    /// `bot-<slug>`: the Bot's pill.
    agent: String,
    /// The Bot's name as the owner typed it.
    bot: String,
    tool: String,
    input: Value,
    /// How long the owner has to answer the card.
    timeout: Duration,
}

fn tool_error(error: &str, message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": error, "message": message.into() })
}

/// Checks the shape only; whether the tool exists is the registry's call.
fn parse_tool_request(payload: Value) -> Result<ToolRequest, Value> {
    let wire: ToolRequestWire =
        serde_json::from_value(payload).map_err(|e| tool_error("invalid_request", format!("bad tool request: {e}")))?;
    let bot = wire.coucou_bot.trim().to_string();
    let slug = crate::grokbot::slug(&bot);
    if slug.is_empty() {
        return Err(tool_error("invalid_request", "a tool call needs the Bot's name (--bot)"));
    }
    // The pill must be this Bot's: a tool call cannot speak for Claude Code or
    // for another Bot.
    let agent = format!("bot-{slug}");
    if wire.coucou_agent != agent {
        return Err(tool_error("invalid_request", format!("coucou_agent must be {agent}")));
    }
    let tool = wire.tool_name.trim().to_string();
    if tool.is_empty() {
        return Err(tool_error("invalid_request", "missing tool_name"));
    }
    let input = match wire.tool_input {
        Value::Null => json!({}),
        v @ Value::Object(_) => v,
        _ => return Err(tool_error("invalid_request", "tool_input must be a JSON object")),
    };
    let timeout = match wire.timeout_ms {
        0 => TOOL_DEFAULT_TIMEOUT,
        ms => Duration::from_millis(ms).clamp(TOOL_MIN_TIMEOUT, TOOL_MAX_TIMEOUT),
    };
    Ok(ToolRequest { agent, bot, tool, input, timeout })
}

/// The approval card: field for field what hook/src/bot.rs sends for
/// `--status ask`, plus the request_id every PermissionRequest gets here.
fn bot_tool_card(req: &ToolRequest, summary: &str, request_id: &str) -> Value {
    json!({
        "hook_event_name": "PermissionRequest",
        "coucou_agent": req.agent,
        "coucou_bot": req.bot,
        "message": summary,
        "cwd": "",
        "tool_name": req.tool,
        "tool_input": req.input,
        "request_id": request_id,
    })
}

/// One tool call from a Grok Bot, answered as the JSON line coucou-hook prints.
async fn tool_request(app: &AppHandle, payload: Value) -> Value {
    let req = match parse_tool_request(payload) {
        Ok(r) => r,
        Err(answer) => {
            log::line(format!("bot tool refused: {}", answer["message"].as_str().unwrap_or_default()));
            return answer;
        }
    };
    let summary = tools::describe(&req.tool, &req.input);
    // The audit line names the Bot, so the log tells Mochi's calls from theirs.
    let target = format!("[bot {}] {summary}", req.bot);
    let Some(tool) = tools::for_bot(&req.tool) else {
        policy::audit(&req.tool, "refused (not a Grok Bot tool)", &target);
        return tool_error(
            "unknown_tool",
            format!("{} is not a tool Grok Bots can call here; see `coucou-hook tool --list`", req.tool),
        );
    };
    match tool.risk {
        Risk::Read => policy::audit(&req.tool, "read", &target),
        Risk::Act => {
            if let Err(answer) = approve_bot_tool(app, &req, &summary).await {
                return answer;
            }
        }
    }
    let outcome = tools::execute(&req.tool, &req.input).await;
    if outcome.is_error {
        tool_error("tool_error", outcome.text)
    } else {
        json!({ "ok": true, "tool": req.tool, "result": outcome.text })
    }
}

/// Puts the card on the Bot's pill and waits for the owner. No answer, a busy or
/// paused island, or a timeout all mean no — as in policy.rs.
async fn approve_bot_tool(app: &AppHandle, req: &ToolRequest, summary: &str) -> Result<(), Value> {
    let id = format!("{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, mut rx) = mpsc::channel::<Reply>(4);
    app.state::<Pending>().0.lock().unwrap().insert(id.clone(), tx);
    let what: String = summary.chars().take(300).collect();
    log::line(format!(
        "hook PermissionRequest id={id} bot tool {} ({}) · {}",
        req.tool,
        req.agent,
        what.replace('\n', " ")
    ));
    let _ = app.emit_to(WINDOW_LABEL, "hook", bot_tool_card(req, summary, &id));

    let decision = wait_for_decision_within(&id, &mut rx, req.timeout).await;
    app.state::<Pending>().0.lock().unwrap().remove(&id);

    // The decision itself is already in coucou.log (answer() and
    // wait_for_decision_within write it); only the verdict is ours.
    match decision.as_deref() {
        // "always" counts once: a Bot never gets a standing approval.
        Some("allow") | Some("always") => Ok(()),
        Some(_) => Err(tool_error(
            "denied",
            "The owner declined this action. Do not retry it another way; ask what they want instead.",
        )),
        None => Err(tool_error(
            "not_answered",
            "Nobody answered the approval card in time (or the island was paused or busy). Nothing was done.",
        )),
    }
}

/// `coucou-hook tool --list`: what a Bot may call, and whether it needs a click.
fn tool_list() -> Value {
    let list: Vec<Value> = tools::for_bots()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "risk": if t.risk == Risk::Read { "read" } else { "act" },
                "schema": (t.schema)(),
            })
        })
        .collect();
    json!({ "ok": true, "tools": list })
}

async fn write_line(mut pipe: impl Relay, answer: &Value) {
    let _ = pipe.write_all(format!("{answer}\n").as_bytes()).await;
    let _ = pipe.flush().await;
    pipe.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(bot: &str, agent: &str, tool: &str, input: Value, timeout_ms: u64) -> Value {
        json!({ "coucou_kind": "tool", "coucou_agent": agent, "coucou_bot": bot,
                "tool_name": tool, "tool_input": input, "timeout_ms": timeout_ms })
    }

    #[test]
    fn a_tool_request_parses() {
        let r = parse_tool_request(wire(" Aerys ", "bot-aerys", "read_file", json!({"path":"x"}), 60_000)).unwrap();
        assert_eq!(
            r,
            ToolRequest {
                agent: "bot-aerys".into(),
                bot: "Aerys".into(),
                tool: "read_file".into(),
                input: json!({"path":"x"}),
                timeout: Duration::from_secs(60),
            }
        );
    }

    #[test]
    fn timeouts_default_and_clamp() {
        let t = |ms| parse_tool_request(wire("A", "bot-a", "x", json!({}), ms)).unwrap().timeout;
        assert_eq!(t(0), TOOL_DEFAULT_TIMEOUT);
        assert_eq!(t(1), TOOL_MIN_TIMEOUT);
        assert_eq!(t(u64::MAX), TOOL_MAX_TIMEOUT);
    }

    #[test]
    fn bad_tool_requests_are_refused() {
        let code = |v: Value| parse_tool_request(v).unwrap_err()["error"].as_str().unwrap().to_string();
        assert_eq!(code(wire("", "bot-", "x", json!({}), 0)), "invalid_request");
        assert_eq!(code(wire("Aerys", "claude", "x", json!({}), 0)), "invalid_request", "pill spoofing");
        assert_eq!(code(wire("Aerys", "bot-aegon", "x", json!({}), 0)), "invalid_request", "another Bot's pill");
        assert_eq!(code(wire("Aerys", "bot-aerys", " ", json!({}), 0)), "invalid_request");
        assert_eq!(code(wire("Aerys", "bot-aerys", "x", json!([1]), 0)), "invalid_request");
        assert_eq!(code(json!({ "coucou_kind": "tool", "timeout_ms": "soon" })), "invalid_request");
        assert!(parse_tool_request(json!({ "coucou_agent": "bot-a", "coucou_bot": "A", "tool_name": "x" })).is_ok());
    }

    #[test]
    fn the_card_is_the_bot_ask_card() {
        let r = parse_tool_request(wire("Diseño Bot", "bot-diseno-bot", "run_powershell", json!({"command":"dir"}), 0)).unwrap();
        let card = bot_tool_card(&r, "dir", "7-1");
        // The keys hook/src/bot.rs sends for `--status ask`, plus request_id.
        let mut keys: Vec<&str> = card.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["coucou_agent", "coucou_bot", "cwd", "hook_event_name", "message", "request_id", "tool_input", "tool_name"]
        );
        assert_eq!(card["hook_event_name"], "PermissionRequest");
        assert_eq!(card["coucou_agent"], "bot-diseno-bot");
        assert_eq!(card["coucou_bot"], "Diseño Bot");
        assert_eq!(card["tool_name"], "run_powershell");
        assert_eq!(card["tool_input"], json!({"command":"dir"}));
    }
}
