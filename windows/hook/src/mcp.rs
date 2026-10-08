//! `coucou-hook --agent cursor --mcp` — a tiny MCP server (stdio, JSON-RPC 2.0,
//! one message per line) with one tool, `island_ask`: the agent's questions
//! with options, asked in the island instead of the agent's own window.
//!
//! Cursor runs no hook for its built-in AskQuestion, so the island never hears
//! of it. This tool is the way round: the agent calls it, the question goes to
//! the app over the pipe exactly like a Cursor AskQuestion through `--ask`
//! (cursor.rs builds the card), and the answer comes back as the tool's result.
//!
//! Never blocks the agent for long: Coucou closed, another card on screen, or
//! no answer within ASK_BUDGET all return at once with a result telling the
//! agent to ask in its own window instead.

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::choices;
use crate::cursor::{self, ASK_TOOL};

/// Asked for when the client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "ARIA is the island at the top of the user's screen. \
Whenever you would ask the user a question with options (one or several questions, single or multiple choice), \
call island_ask instead of your built-in AskQuestion tool: the user answers there. \
If it says nobody answered, ask with your own tool instead.";

const DESCRIPTION: &str = "Ask the user one or more questions with options in ARIA, the island at the top of \
their screen, and wait for the answers (up to two minutes). Use this INSTEAD of the built-in AskQuestion tool \
whenever you need the user to choose. A single single-choice question also lets them type their own answer. \
If nobody answers, the result says so: then ask with your own AskQuestion tool.";

fn tool_spec() -> Value {
    json!({
        "name": ASK_TOOL,
        "title": "Ask in the island",
        "description": DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "The question, as the user should read it." },
                            "header": { "type": "string", "description": "Optional short title." },
                            "options": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": choices::MAX_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiSelect": { "type": "boolean", "description": "Several options may be picked." }
                        },
                        "required": ["question", "options"]
                    }
                }
            },
            "required": ["questions"]
        }
    })
}

/// None unless `--mcp` is on the command line; otherwise serves until stdin
/// closes and returns the exit code.
pub fn run() -> Option<i32> {
    if !std::env::args().skip(1).any(|a| a == "--mcp") {
        return None;
    }
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // a notification
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        if method == "tools/call" {
            // Answered from its own thread: pings and other calls go on meanwhile.
            let out = Arc::clone(&out);
            std::thread::spawn(move || send(&out, &json!({ "jsonrpc": "2.0", "id": id, "result": call(&params) })));
            continue;
        }
        send(&out, &respond(id, &method, &params));
    }
    Some(0)
}

fn send(out: &Mutex<std::io::Stdout>, msg: &Value) {
    if let Ok(mut out) = out.lock() {
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }
}

/// Every request but `tools/call`.
fn respond(id: Value, method: &str, params: &Value) -> Value {
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "coucou", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [tool_spec()] }),
        _ => {
            return json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("Method not found: {method}") } });
        }
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn call(params: &Value) -> Value {
    if params.get("name").and_then(Value::as_str) != Some(ASK_TOOL) {
        return text_result("Unknown tool.", true);
    }
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let Some((line, ctx)) = question_line(&arguments, &current_dir()) else {
        return text_result("Give at least one question, each with a text and at least one option.", true);
    };
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let _ = tx.send(crate::talk(&line, true));
    });
    let reply = rx.recv_timeout(crate::ASK_BUDGET).ok().flatten();
    result_for(reply.as_deref(), &ctx)
}

fn current_dir() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

/// The line for the pipe — a Cursor AskQuestion, normalised as `--ask` does —
/// and what is needed to read the answer. None when there is nothing to ask.
fn question_line(arguments: &Value, cwd: &str) -> Option<(String, cursor::Context)> {
    let questions = arguments.get("questions").and_then(Value::as_array)?;
    let asks = |q: &Value| {
        let text = ["question", "prompt"].iter().find_map(|k| q.get(*k).and_then(Value::as_str)).unwrap_or_default();
        !text.trim().is_empty() && q.get("options").and_then(Value::as_array).is_some_and(|o| !o.is_empty())
    };
    if questions.is_empty() || !questions.iter().all(asks) {
        return None;
    }
    let mut map = Map::new();
    map.insert("tool_name".into(), json!("AskQuestion"));
    map.insert("tool_input".into(), json!({ "questions": questions }));
    let (name, _) = cursor::normalize(&mut map, "preToolUse", false, true);
    map.insert("hook_event_name".into(), json!(name));
    map.insert("source_event".into(), json!("mcp"));
    map.insert("coucou_agent".into(), json!("cursor"));
    if !cwd.is_empty() {
        map.insert("cwd".into(), json!(cwd));
    }
    let ctx = cursor::Context::from(&map);
    let mut payload = Value::Object(map);
    crate::truncate_strings(&mut payload);
    let mut line = payload.to_string();
    line.push('\n');
    Some((line, ctx))
}

/// The island's raw reply → the tool's result.
fn result_for(reply: Option<&str>, ctx: &cursor::Context) -> Value {
    let parsed = reply.and_then(choices::parse_reply);
    let answers = cursor::answers(parsed.as_ref(), ctx);
    if !answers.is_empty() {
        let lines: Vec<String> = answers.iter().map(|(q, a)| format!("- {q} → {a}")).collect();
        return text_result(&format!("The user answered in the island:\n{}", lines.join("\n")), false);
    }
    if parsed.is_some_and(|r| r.decision == "deny") {
        return text_result("The user dismissed the question in the island without answering.", false);
    }
    text_result(
        "Nobody answered in the island (ARIA closed, another card on screen, or no answer in time). \
Ask the user with your own AskQuestion tool instead.",
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(arguments: Value) -> (Value, cursor::Context) {
        let (line, ctx) = question_line(&arguments, "C:\\p").expect("asked");
        (serde_json::from_str(line.trim_end()).unwrap(), ctx)
    }

    #[test]
    fn the_server_introduces_itself_and_its_one_tool() {
        let init = respond(json!(1), "initialize", &json!({ "protocolVersion": "2025-03-26" }));
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "coucou");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("island_ask"));
        let list = respond(json!(2), "tools/list", &Value::Null);
        assert_eq!(list["result"]["tools"][0]["name"], ASK_TOOL);
        assert_eq!(respond(json!(3), "ping", &Value::Null)["result"], json!({}));
        assert_eq!(respond(json!(4), "resources/list", &Value::Null)["error"]["code"], -32601);
    }

    #[test]
    fn one_question_becomes_a_card_with_choices_on_cursors_pill() {
        let (v, ctx) = line_of(json!({ "questions": [{
            "question": "¿Qué motor?", "options": [{ "label": "Grok", "description": "rápido" }, { "label": "Claude" }],
        }] }));
        assert_eq!(v["hook_event_name"], "PermissionRequest");
        assert_eq!(v["tool_name"], "Pregunta");
        assert_eq!(v["coucou_agent"], "cursor");
        assert_eq!(v["source_event"], "mcp");
        assert_eq!(v["cwd"], "C:\\p");
        assert_eq!(v["allowCustom"], true);
        let r = result_for(Some(r#"{"decision":"allow","answer":"Grok"}"#), &ctx);
        assert_eq!(r["isError"], false);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("¿Qué motor? → Grok"));
    }

    #[test]
    fn several_or_multi_select_keep_the_step_by_step_card() {
        let (v, ctx) = line_of(json!({ "questions": [
            { "question": "¿Cuáles?", "options": [{ "label": "a" }, { "label": "b" }], "multiSelect": true },
        ] }));
        assert_eq!(v["hook_event_name"], "PermissionRequest");
        assert_eq!(v["tool_name"], "AskUserQuestion");
        assert_eq!(v["tool_input"]["questions"][0]["multiSelect"], true);
        let r = result_for(Some(r#"{"decision":"answer","answers":{"¿Cuáles?":["a","b"]}}"#), &ctx);
        assert!(r["content"][0]["text"].as_str().unwrap().contains("¿Cuáles? → a, b"));

        // Answers come back in the order they were asked.
        let (_, ctx) = line_of(json!({ "questions": [
            { "question": "¿Zeta?", "options": [{ "label": "z" }] },
            { "question": "¿Alfa?", "options": [{ "label": "a" }] },
        ] }));
        let r = result_for(Some(r#"{"decision":"answer","answers":{"¿Alfa?":"a","¿Zeta?":"z"}}"#), &ctx);
        assert!(r["content"][0]["text"].as_str().unwrap().ends_with("- ¿Zeta? → z\n- ¿Alfa? → a"));
    }

    #[test]
    fn no_answer_or_rechazar_say_so() {
        let (_, ctx) = line_of(json!({ "questions": [{ "question": "¿Sí?", "options": [{ "label": "Sí" }] }] }));
        let none = result_for(None, &ctx);
        assert_eq!(none["isError"], true);
        assert!(none["content"][0]["text"].as_str().unwrap().contains("AskQuestion"));
        let dismissed = result_for(Some("deny"), &ctx);
        assert_eq!(dismissed["isError"], false);
        assert!(dismissed["content"][0]["text"].as_str().unwrap().contains("dismissed"));
    }

    #[test]
    fn nothing_to_ask_is_refused() {
        for bad in [json!({}), json!({ "questions": [] }), json!({ "questions": [{ "question": "¿?", "options": [] }] }),
                    json!({ "questions": [{ "question": " ", "options": [{ "label": "a" }] }] })] {
            assert!(question_line(&bad, "").is_none(), "{bad}");
        }
        assert_eq!(call(&json!({ "name": "other" }))["isError"], true);
    }
}
