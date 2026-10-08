//! Cursor's own hook dialect, on top of the shared table in normalize.rs:
//! its shell / MCP / file events, the approval gates (`--approve`), its
//! AskQuestion tool answered from the island (`--ask`), the orders the island
//! hands back on `stop` (`followup_message`), and its replies
//! (`{"permission": "allow" | "deny" | "ask"}`), always as ASCII-only JSON.
//!
//! | Cursor event                                    | Canonical event                    |
//! |-------------------------------------------------|------------------------------------|
//! | `beforeShellExecution`, `beforeMCPExecution`    | `PreToolUse`, or `PermissionRequest` with `--approve` |
//! | `beforeReadFile`                                | `PreToolUse` (tool `Read`)         |
//! | `afterShellExecution`, `afterMCPExecution`      | `PostToolUse`                      |
//! | `afterFileEdit`                                 | `PostToolUse` (tool `Edit`, feeds the live diff) |
//! | `afterAgentResponse` / `afterAgentThought`      | `AgentResponse` / `AgentThought`   |
//! | `stop`                                          | `Stop` (may get orders back), `StopFailure` on error |

use serde_json::{json, Map, Value};

use crate::choices::{self, Reply};
use crate::normalize;

/// What the relay waits for once a Cursor event is normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Forward and exit.
    Nothing,
    /// An approval gate: Allow / Deny from the island.
    Decision,
    /// AskQuestion: the chosen answers.
    Ask,
    /// `stop`: the island may hand back queued orders.
    Followup,
    /// The gate, for Coucou's own question tool (mcp.rs): allowed at once, the
    /// question itself being the card.
    Pass,
}

/// Coucou's MCP question tool, as Cursor's beforeMCPExecution names it.
pub const ASK_TOOL: &str = "island_ask";

/// Our own question tool, served by this relay (`--mcp`): asking has no side
/// effect, so its gate needs no second card. Cursor names the server's command.
fn is_our_ask(map: &Map<String, Value>) -> bool {
    let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or_default();
    let command = map.get("command").and_then(Value::as_str).unwrap_or_default();
    tool.rsplit([':', '-', '/']).next() == Some(ASK_TOOL) && command.contains("coucou-hook") && command.contains("--mcp")
}

/// Events that can carry an approval. They only become a `PermissionRequest`
/// when the hook was installed with `--approve`; otherwise they are plain steps.
const GATED: &[&str] = &["beforeShellExecution", "beforeMCPExecution"];

/// Cursor's names → canonical names; the shared table covers the rest.
pub fn event(raw: &str) -> &str {
    match raw {
        "beforeShellExecution" | "beforeMCPExecution" | "beforeReadFile" => "PreToolUse",
        "afterShellExecution" | "afterMCPExecution" | "afterFileEdit" => "PostToolUse",
        "afterAgentResponse" => "AgentResponse",
        "afterAgentThought" => "AgentThought",
        "preCompact" => "PreCompact",
        other => normalize::event(other),
    }
}

/// Normalises a Cursor payload in place (after `normalize::fields`) and returns
/// the canonical event name and what to wait for. `approve` is `--approve`,
/// `ask` is `--ask` (the AskQuestion hook).
pub fn normalize(map: &mut Map<String, Value>, raw: &str, approve: bool, ask: bool) -> (String, Wait) {
    if ask {
        let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_string();
        if is_mode_switch(&tool) {
            mode_switch_card(map);
            map.insert("source_event".into(), json!(raw));
            return ("PermissionRequest".into(), Wait::Ask);
        }
        if !is_question(&tool) {
            return (raw.to_string(), Wait::Nothing);
        }
        questions_to_claude(map);
        // One single-choice question: a card with one button per option and
        // "Otra respuesta", answered through approval_decision.
        if question_to_choices(map) {
            map.insert("source_event".into(), json!(raw));
            return ("PermissionRequest".into(), Wait::Ask);
        }
        // Several questions or multi-select: Claude Code's AskUserQuestion
        // card, answered through approval_answer.
        map.insert("source_tool".into(), json!(tool));
        map.insert("source_event".into(), json!(raw));
        map.insert("tool_name".into(), json!("AskUserQuestion"));
        return ("PermissionRequest".into(), Wait::Ask);
    }

    let ours = raw == "beforeMCPExecution" && is_our_ask(map);
    let mut name = event(raw).to_string();
    if name != raw {
        map.insert("source_event".into(), json!(raw));
    }

    match raw {
        "beforeShellExecution" | "afterShellExecution" => {
            let mut input = Map::new();
            if let Some(c) = map.get("command").cloned() {
                input.insert("command".into(), c);
            }
            map.insert("tool_name".into(), json!("Shell"));
            map.insert("tool_input".into(), Value::Object(input));
            map.remove("output");
        }
        "beforeMCPExecution" | "afterMCPExecution" => {
            let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or("tool").to_string();
            map.insert("tool_name".into(), json!(format!("MCP: {tool}")));
            // Cursor sends tool_input as a JSON string.
            if let Some(Value::String(s)) = map.get("tool_input") {
                let parsed = serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!({ "input": s }));
                map.insert("tool_input".into(), parsed);
            }
            map.remove("result_json");
        }
        "afterFileEdit" => {
            let mut input = Map::new();
            for k in ["file_path", "edits"] {
                if let Some(v) = map.get(k).cloned() {
                    input.insert(k.into(), v);
                }
            }
            map.insert("tool_name".into(), json!("Edit"));
            map.insert("tool_input".into(), Value::Object(input));
        }
        "beforeReadFile" => {
            let mut input = Map::new();
            if let Some(v) = map.get("file_path").cloned() {
                input.insert("file_path".into(), v);
            }
            map.insert("tool_name".into(), json!("Read"));
            map.insert("tool_input".into(), Value::Object(input));
            map.remove("content");
        }
        "afterAgentResponse" => {
            if let Some(t) = map.get("text").cloned() {
                map.insert("message".into(), t);
            }
        }
        _ => {}
    }

    name = normalize::refine(&name, map);
    if approve && ours {
        return (name, Wait::Pass);
    }
    if approve && GATED.contains(&raw) {
        return ("PermissionRequest".into(), Wait::Decision);
    }
    if name == "Stop" {
        map.insert("coucou_kind".into(), json!("cursor_stop"));
        return (name, Wait::Followup);
    }
    (name, Wait::Nothing)
}

fn is_question(tool: &str) -> bool {
    matches!(tool.to_ascii_lowercase().replace('_', "").as_str(), "askquestion" | "askuserquestion")
}

fn is_mode_switch(tool: &str) -> bool {
    tool.to_ascii_lowercase().replace('_', "") == "switchmode"
}

const STAY_LABEL: &str = "Seguir en el modo actual";

/// `plan` → `Plan`.
fn mode_name(mode: &str) -> String {
    let mut chars = mode.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// SwitchMode (`target_mode_id`, `explanation`) → a card with two choices:
/// switch, or stay. Cursor asks for consent before switching modes, so the
/// island asks instead; no answer leaves the choice to Cursor's own prompt.
fn mode_switch_card(map: &mut Map<String, Value>) {
    let input = match map.get("tool_input") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let text = |keys: &[&str]| {
        keys.iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or_default().trim().to_string()
    };
    let mode = text(&["target_mode_id", "targetModeId", "mode", "target"]);
    let why = text(&["explanation", "reason"]);
    let (question, switch) = match mode_name(&mode) {
        name if name.is_empty() => ("Cursor quiere cambiar de modo".to_string(), "Cambiar de modo".to_string()),
        name => (format!("Cursor quiere cambiar a modo {name}"), format!("Cambiar a modo {name}")),
    };
    let options = choices::clean_options(&[(switch, why), (STAY_LABEL.into(), String::new())]).unwrap_or_default();
    let tool = map.get("tool_name").cloned().unwrap_or(Value::Null);
    map.insert("source_tool".into(), tool);
    map.insert("hook_event_name".into(), json!("PermissionRequest"));
    map.insert("tool_name".into(), json!("Pregunta"));
    map.insert("tool_input".into(), json!({ "command": question, "mode": mode }));
    map.insert("options".into(), Value::Array(options));
    map.insert("allowCustom".into(), json!(false));
}

/// AskQuestion's input (`title`, `questions[].prompt`, `options[].label`,
/// `allow_multiple`) → the AskUserQuestion shape the island reads (`question`,
/// `header`, `options[].label`, `multiSelect`).
fn questions_to_claude(map: &mut Map<String, Value>) {
    let input = match map.get("tool_input") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let title = input.get("title").and_then(Value::as_str).unwrap_or_default();
    let text = |v: &Value, keys: &[&str]| {
        keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or_default().to_string()
    };
    let questions: Vec<Value> = input
        .get("questions")
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .map(|q| {
                    let options: Vec<Value> = q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|os| {
                            os.iter()
                                .map(|o| match o {
                                    Value::String(s) => json!({ "label": s }),
                                    _ => json!({ "label": text(o, &["label", "id"]), "description": text(o, &["description"]) }),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let header = text(q, &["header"]);
                    let header = if header.is_empty() { title.to_string() } else { header };
                    let multi = ["multiSelect", "allow_multiple", "allowMultiple"]
                        .iter()
                        .any(|k| q.get(*k).and_then(Value::as_bool) == Some(true));
                    json!({
                        "question": text(q, &["question", "prompt"]),
                        "header": header,
                        "options": options,
                        "multiSelect": multi,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    map.insert("tool_input".into(), json!({ "questions": questions }));
}

/// A question already in the island's shape with exactly one single-choice
/// question becomes a question card with choices: `Pregunta` with the question
/// in `tool_input.command`, `options` and `allowCustom: true` at the top. The
/// questions stay in `tool_input` for the answer (`Context`). Several
/// questions, multi-select, or options that do not fit (empty, more than 12)
/// keep the step-by-step question card.
fn question_to_choices(map: &mut Map<String, Value>) -> bool {
    let Some(questions) = map.get("tool_input").and_then(|i| i.get("questions")).and_then(Value::as_array).cloned() else {
        return false;
    };
    let [q] = questions.as_slice() else { return false };
    if q.get("multiSelect").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    let raw: Vec<(String, String)> = q
        .get("options")
        .and_then(Value::as_array)
        .map(|os| {
            os.iter()
                .map(|o| {
                    let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    (s("label"), s("description"))
                })
                .collect()
        })
        .unwrap_or_default();
    let Ok(options) = choices::clean_options(&raw) else { return false };
    let text = q.get("question").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let text = text.or_else(|| q.get("header").and_then(Value::as_str)).unwrap_or("¿Qué prefieres?").to_string();
    let tool = map.get("tool_name").cloned().unwrap_or(Value::Null);
    map.insert("source_tool".into(), tool);
    map.insert("hook_event_name".into(), json!("PermissionRequest"));
    map.insert("tool_name".into(), json!("Pregunta"));
    map.insert("tool_input".into(), json!({ "command": text, "questions": questions }));
    map.insert("options".into(), Value::Array(options));
    map.insert("allowCustom".into(), json!(true));
    true
}

/// The questions asked, captured before the payload is truncated: the answer
/// has to name them verbatim.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub questions: Value,
    /// A SwitchMode card rather than a question.
    pub mode_switch: bool,
}

impl Context {
    pub fn from(map: &Map<String, Value>) -> Self {
        Self {
            questions: map.get("tool_input").and_then(|i| i.get("questions")).cloned().unwrap_or(Value::Null),
            mode_switch: map.get("source_tool").and_then(Value::as_str).is_some_and(is_mode_switch),
        }
    }
}

/// What to print for Cursor, given the island's raw reply (`None`: nobody
/// clicked). Always JSON, always ASCII-only. An approval gate with no decision
/// is `ask`, never an implicit allow, so Cursor asks in its own UI.
pub fn stdout(wait: Wait, decision: Option<&str>, ctx: &Context) -> Option<String> {
    let reply = decision.and_then(choices::parse_reply);
    let reply = reply.as_ref();
    let out = match wait {
        Wait::Nothing => json!({}),
        Wait::Followup => {
            let message = reply
                .filter(|r| r.decision == "followup")
                .and_then(|r| r.answers.get("message").and_then(Value::as_str))
                .filter(|m| !m.trim().is_empty());
            match message {
                Some(m) => json!({ "followup_message": m }),
                None => json!({}),
            }
        }
        Wait::Ask => answer(reply, ctx),
        Wait::Pass => json!({ "permission": "allow" }),
        Wait::Decision => match reply.map(|r| r.decision.as_str()) {
            Some("allow" | "always") => json!({ "permission": "allow" }),
            Some("deny") => json!({
                "permission": "deny",
                "user_message": "Denied from Coucou",
                "agent_message": "The user denied this action from Coucou.",
            }),
            _ => json!({ "permission": "ask" }),
        },
    };
    Some(ascii_json(&out.to_string()))
}

/// `(question, answer)` pairs from the island's reply to a question card: the
/// pick on a card with choices, or every answer of the step-by-step card.
/// Empty for anything else (Rechazar, no reply).
pub fn answers(reply: Option<&Reply>, ctx: &Context) -> Vec<(String, String)> {
    if let Some(a) = reply.and_then(Reply::answer) {
        let question = ctx.questions.get(0).and_then(|q| q.get("question")).and_then(Value::as_str).unwrap_or("Pregunta");
        return vec![(question.to_string(), a.to_string())];
    }
    reply
        .filter(|r| r.decision == "answer")
        .and_then(|r| r.answers.as_object())
        .map(|a| {
            a.iter()
                .map(|(q, v)| (q.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Cursor's preToolUse can't fill in a question's answers, only allow or deny
/// the tool. Answered in the island: deny the card and hand the agent the
/// answers (Cursor's hook output has no answer field: `agent_message` is the
/// only text that reaches the model). Anything else — including Rechazar on a
/// card with choices — allow, and Cursor shows its own card; an empty reply
/// would block the tool instead.
fn answer(reply: Option<&Reply>, ctx: &Context) -> Value {
    if ctx.mode_switch {
        let stay = reply.is_some_and(|r| r.decision == "deny") || reply.and_then(Reply::answer) == Some(STAY_LABEL);
        return match reply.and_then(Reply::answer) {
            _ if stay => json!({
                "permission": "deny",
                "user_message": "Seguir en el modo actual (desde Coucou)",
                "agent_message": "The user chose, in Coucou (the island at the top of their screen), to stay in the current mode. \
Do not switch modes; carry on in this one.",
            }),
            Some(_) => json!({ "permission": "allow" }),
            None => json!({ "permission": "ask" }),
        };
    }
    let answers = answers(reply, ctx);
    if answers.is_empty() {
        return json!({ "permission": "allow" });
    }
    let lines: Vec<String> = answers.iter().map(|(q, a)| format!("- {q} → {a}")).collect();
    let short: Vec<&str> = answers.iter().map(|(_, a)| a.as_str()).collect();
    json!({
        "permission": "deny",
        "user_message": format!("Respondido desde Coucou: {}", short.join(" · ")),
        "agent_message": format!(
            "The user already answered in Coucou (the island at the top of their screen). \
Do not ask again; continue with these answers:\n{}",
            lines.join("\n")
        ),
    })
}

// ── Cursor's text, read as ANSI on Windows ────────────────────────────────────
// Cursor runs a hook through a PowerShell 5.1 wrapper that reads the payload
// file without `-Encoding`, so as the ANSI code page (Windows-1252 here), and
// writes it to our stdin as UTF-8: "í" (c3 ad) arrives as "Ã\u{ad}". By then
// stdin is valid UTF-8, so the text can only be repaired. Conservatively: a
// string goes back to its 1252 bytes only when it carries the telltale pairs
// (a UTF-8 lead byte, then a continuation byte) and those bytes are valid
// UTF-8. Anything else is left exactly as it came.

/// Windows-1252 0x80..=0x9F; the five holes are what .NET decodes them to.
const CP1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

fn cp1252_byte(c: char) -> Option<u8> {
    match c as u32 {
        0..=0x7F | 0xA0..=0xFF => Some(c as u8),
        _ => CP1252_HIGH.iter().position(|&h| h == c).map(|i| 0x80 + i as u8),
    }
}

/// The original text of a string garbled as above, or None to leave it alone.
pub fn repair_mojibake(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let telltale = chars
        .windows(2)
        .any(|w| matches!(cp1252_byte(w[0]), Some(0xC2..=0xF4)) && matches!(cp1252_byte(w[1]), Some(0x80..=0xBF)));
    if !telltale {
        return None;
    }
    let bytes = chars.iter().map(|&c| cp1252_byte(c)).collect::<Option<Vec<u8>>>()?;
    let fixed = String::from_utf8(bytes).ok()?;
    (fixed != s).then_some(fixed)
}

/// `repair_mojibake` on every string in a payload.
pub fn repair_mojibake_in(value: &mut Value) {
    match value {
        Value::String(s) => {
            if let Some(fixed) = repair_mojibake(s) {
                *s = fixed;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(repair_mojibake_in),
        Value::Object(map) => map.values_mut().for_each(repair_mojibake_in),
        _ => {}
    }
}

/// The same JSON with every non-ASCII character as `\uXXXX`: no code page on
/// the way back to Cursor (the same wrapper reads our stdout) can garble it.
pub fn ascii_json(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn out(wait: Wait, reply: Option<&str>, ctx: &Context) -> Value {
        serde_json::from_str(&stdout(wait, reply, ctx).unwrap()).unwrap()
    }

    #[test]
    fn cursor_names_become_canonical() {
        assert_eq!(event("beforeShellExecution"), "PreToolUse");
        assert_eq!(event("afterFileEdit"), "PostToolUse");
        assert_eq!(event("afterAgentResponse"), "AgentResponse");
        assert_eq!(event("beforeSubmitPrompt"), "UserPromptSubmit");
        let mut m = obj(json!({ "hook_event_name": "beforeSubmitPrompt", "prompt": "fix it" }));
        assert_eq!(normalize(&mut m, "beforeSubmitPrompt", false, false), ("UserPromptSubmit".into(), Wait::Nothing));
        assert_eq!(m["source_event"], "beforeSubmitPrompt");
    }

    #[test]
    fn shell_gate_waits_only_with_approve() {
        let base = json!({ "hook_event_name": "beforeShellExecution", "command": "rm -rf build" });
        let mut plain = obj(base.clone());
        assert_eq!(normalize(&mut plain, "beforeShellExecution", false, false), ("PreToolUse".into(), Wait::Nothing));
        assert_eq!(plain["tool_name"], "Shell");
        assert_eq!(plain["tool_input"]["command"], "rm -rf build");

        let mut gated = obj(base);
        assert_eq!(normalize(&mut gated, "beforeShellExecution", true, false), ("PermissionRequest".into(), Wait::Decision));
    }

    #[test]
    fn our_own_question_tool_passes_the_gate() {
        let ours = json!({ "tool_name": "island_ask", "tool_input": "{}",
            "command": r"C:\Users\me\AppData\Local\Coucou\bin\coucou-hook.exe --agent cursor --mcp" });
        let mut m = obj(ours.clone());
        assert_eq!(normalize(&mut m, "beforeMCPExecution", true, false), ("PreToolUse".into(), Wait::Pass));
        assert_eq!(stdout(Wait::Pass, None, &Context::default()).unwrap(), r#"{"permission":"allow"}"#);
        // Same name from another server, or another tool of ours: the gate asks.
        let mut theirs = obj(json!({ "tool_name": "island_ask", "tool_input": "{}", "command": "npx other-server" }));
        assert_eq!(normalize(&mut theirs, "beforeMCPExecution", true, false).1, Wait::Decision);
        let mut other = ours;
        other["tool_name"] = json!("delete_everything");
        assert_eq!(normalize(&mut obj(other), "beforeMCPExecution", true, false).1, Wait::Decision);
    }

    #[test]
    fn mcp_input_string_is_parsed() {
        let mut m = obj(json!({ "tool_name": "create_issue", "tool_input": "{\"title\":\"bug\"}" }));
        normalize(&mut m, "beforeMCPExecution", true, false);
        assert_eq!(m["tool_name"], "MCP: create_issue");
        assert_eq!(m["tool_input"]["title"], "bug");
    }

    #[test]
    fn file_edit_keeps_edits_for_the_diff() {
        let mut m = obj(json!({ "file_path": "C:/a/b.ts", "edits": [{ "old_string": "a", "new_string": "b" }] }));
        assert_eq!(normalize(&mut m, "afterFileEdit", false, false).0, "PostToolUse");
        assert_eq!(m["tool_name"], "Edit");
        assert_eq!(m["tool_input"]["edits"][0]["new_string"], "b");
    }

    #[test]
    fn stop_may_bring_orders_back_and_an_error_is_a_failure() {
        let mut m = obj(json!({ "status": "error" }));
        assert_eq!(normalize(&mut m, "stop", false, false), ("StopFailure".into(), Wait::Nothing));
        let mut ok = obj(json!({ "status": "completed" }));
        assert_eq!(normalize(&mut ok, "stop", false, false), ("Stop".into(), Wait::Followup));
        assert_eq!(ok["coucou_kind"], "cursor_stop");

        let ctx = Context::default();
        let orders = r#"{"decision":"followup","answers":{"message":"run the tests"}}"#;
        assert_eq!(out(Wait::Followup, Some(orders), &ctx)["followup_message"], "run the tests");
        assert_eq!(stdout(Wait::Followup, None, &ctx).unwrap(), "{}");
    }

    #[test]
    fn a_gate_falls_back_to_ask_never_allow() {
        let ctx = Context::default();
        assert_eq!(stdout(Wait::Decision, None, &ctx).unwrap(), r#"{"permission":"ask"}"#);
        assert_eq!(stdout(Wait::Decision, Some("maybe"), &ctx).unwrap(), r#"{"permission":"ask"}"#);
        assert_eq!(stdout(Wait::Decision, Some("allow"), &ctx).unwrap(), r#"{"permission":"allow"}"#);
        assert_eq!(out(Wait::Decision, Some("deny"), &ctx)["permission"], "deny");
        assert_eq!(stdout(Wait::Nothing, Some("allow"), &ctx).unwrap(), "{}");
    }

    #[test]
    fn questions_take_the_island_shape() {
        let mut q = obj(json!({
            "tool_name": "AskQuestion",
            "tool_input": { "title": "Motor", "questions": [{
                "prompt": "¿Qué motor?",
                "options": [{ "id": "a", "label": "Grok" }, { "id": "b", "label": "Claude" }],
                "allow_multiple": true,
            }] },
        }));
        assert_eq!(normalize(&mut q, "preToolUse", false, true), ("PermissionRequest".into(), Wait::Ask));
        assert_eq!(q["tool_name"], "AskUserQuestion");
        let first = &q["tool_input"]["questions"][0];
        assert_eq!(first["question"], "¿Qué motor?");
        assert_eq!(first["header"], "Motor");
        assert_eq!(first["options"][1]["label"], "Claude");
        assert_eq!(first["multiSelect"], true);

        let mut shell = obj(json!({ "tool_name": "Shell" }));
        assert_eq!(normalize(&mut shell, "preToolUse", false, true).1, Wait::Nothing);
    }

    #[test]
    fn a_single_question_becomes_a_card_with_choices() {
        let mut q = obj(json!({
            "tool_name": "AskQuestion",
            "tool_input": { "title": "Motor", "questions": [{
                "prompt": "¿Qué motor?",
                "options": [{ "id": "a", "label": "Grok", "description": "rápido" }, { "id": "b", "label": "Claude" }],
            }] },
        }));
        assert_eq!(normalize(&mut q, "preToolUse", false, true), ("PermissionRequest".into(), Wait::Ask));
        assert_eq!(q["tool_name"], "Pregunta");
        assert_eq!(q["source_tool"], "AskQuestion");
        assert!(q.get("coucou_kind").is_none(), "not the step-by-step question card");
        assert_eq!(q["tool_input"]["command"], "¿Qué motor?");
        assert_eq!(q["options"], json!([{ "label": "Grok", "description": "rápido" }, { "label": "Claude" }]));
        assert_eq!(q["allowCustom"], true);

        // The pick goes back as the deny reason, the only text Cursor relays.
        let ctx = Context::from(&q);
        let o = out(Wait::Ask, Some(r#"{"decision":"allow","answer":"Otro: Gemini"}"#), &ctx);
        assert_eq!(o["permission"], "deny");
        assert!(o["agent_message"].as_str().unwrap().contains("¿Qué motor? → Otro: Gemini"));
        // Rechazar, a bare allow, or nothing: Cursor shows its own card.
        for reply in [None, Some("deny"), Some("allow")] {
            assert_eq!(stdout(Wait::Ask, reply, &ctx).unwrap(), r#"{"permission":"allow"}"#);
        }
    }

    #[test]
    fn multi_select_or_several_questions_keep_the_question_card() {
        let multi = json!({ "tool_name": "AskQuestion", "tool_input": { "questions": [
            { "prompt": "¿Cuáles?", "options": ["a", "b"], "allow_multiple": true } ] } });
        let two = json!({ "tool_name": "AskQuestion", "tool_input": { "questions": [
            { "prompt": "¿Uno?", "options": ["a"] }, { "prompt": "¿Dos?", "options": ["b"] } ] } });
        for v in [multi, two] {
            let mut m = obj(v);
            assert_eq!(normalize(&mut m, "preToolUse", false, true), ("PermissionRequest".into(), Wait::Ask));
            assert_eq!(m["tool_name"], "AskUserQuestion");
            assert!(m.get("options").is_none());
        }
    }

    #[test]
    fn answers_deny_with_the_answers_and_otherwise_allow() {
        let ctx = Context::default();
        let o = out(Wait::Ask, Some(r#"{"decision":"answer","answers":{"¿Qué motor?":"Grok"}}"#), &ctx);
        assert_eq!(o["permission"], "deny");
        assert!(o["agent_message"].as_str().unwrap().contains("¿Qué motor? → Grok"));
        assert!(o["user_message"].as_str().unwrap().contains("Grok"));
        for reply in [None, Some("deny"), Some(r#"{"decision":"answer","answers":{}}"#)] {
            assert_eq!(stdout(Wait::Ask, reply, &ctx).unwrap(), r#"{"permission":"allow"}"#);
        }
    }

    #[test]
    fn a_mode_switch_is_asked_in_the_island() {
        let mut m = obj(json!({
            "tool_name": "SwitchMode",
            "tool_input": { "target_mode_id": "plan", "explanation": "Conviene planear antes de tocar nada" },
        }));
        assert_eq!(normalize(&mut m, "preToolUse", false, true), ("PermissionRequest".into(), Wait::Ask));
        assert_eq!(m["tool_name"], "Pregunta");
        assert_eq!(m["source_tool"], "SwitchMode");
        assert_eq!(m["tool_input"]["command"], "Cursor quiere cambiar a modo Plan");
        assert_eq!(m["options"], json!([
            { "label": "Cambiar a modo Plan", "description": "Conviene planear antes de tocar nada" },
            { "label": "Seguir en el modo actual" },
        ]));
        assert_eq!(m["allowCustom"], false);

        let ctx = Context::from(&m);
        assert!(ctx.mode_switch);
        let switch = Some(r#"{"decision":"allow","answer":"Cambiar a modo Plan"}"#);
        assert_eq!(stdout(Wait::Ask, switch, &ctx).unwrap(), r#"{"permission":"allow"}"#);
        for stay in [Some(r#"{"decision":"allow","answer":"Seguir en el modo actual"}"#), Some("deny")] {
            let o = out(Wait::Ask, stay, &ctx);
            assert_eq!(o["permission"], "deny");
            assert!(o["agent_message"].as_str().unwrap().contains("stay in the current mode"));
        }
        // No answer: Cursor asks in its own window.
        assert_eq!(stdout(Wait::Ask, None, &ctx).unwrap(), r#"{"permission":"ask"}"#);

        let mut bare = obj(json!({ "tool_name": "switch_mode", "tool_input": "{}" }));
        normalize(&mut bare, "preToolUse", false, true);
        assert_eq!(bare["tool_input"]["command"], "Cursor quiere cambiar de modo");
        assert!(!Context::from(&obj(json!({ "source_tool": "AskQuestion" }))).mode_switch);
    }

    /// What Cursor's wrapper does to a text: its UTF-8 bytes read as Windows-1252.
    fn garble(s: &str) -> String {
        s.bytes().map(|b| if (0x80..=0x9F).contains(&b) { CP1252_HIGH[(b - 0x80) as usize] } else { b as char }).collect()
    }

    #[test]
    fn mojibake_from_cursor_is_repaired() {
        let seen = "S\u{c3}\u{ad}, te entiendo, y ya lo agregu\u{c3}\u{a9} al plan";
        assert_eq!(garble("Sí, te entiendo, y ya lo agregué al plan"), seen);
        assert_eq!(repair_mojibake(seen).as_deref(), Some("Sí, te entiendo, y ya lo agregué al plan"));
        for text in ["¿Qué tal? año, pingüino, Ángel, “comillas” — … 😀 €", "Á Í Ï Ð Ý ß ÿ", "東京 ok"] {
            assert_eq!(repair_mojibake(&garble(text)).as_deref(), Some(text), "{text}");
        }
    }

    #[test]
    fn good_text_is_left_alone() {
        for text in [
            "plain ascii",
            "café, Sí, agregué",       // real accents: their 1252 bytes are not UTF-8
            "precio 5Ã",                // no continuation after the lead
            "Ã©Ã",                      // round trip is not valid UTF-8
            "Ã© → listo",               // → is not in Windows-1252: not from the wrapper
            "😀 €",
            "",
        ] {
            assert_eq!(repair_mojibake(text), None, "{text}");
        }
        assert_eq!(repair_mojibake("Sí, agregué"), None);
    }

    #[test]
    fn every_string_of_a_payload_is_repaired() {
        let mut v = json!({ "text": garble("Sí"), "n": 1, "a": [garble("¿Qué?"), "ok"], "o": { "k": garble("año") } });
        repair_mojibake_in(&mut v);
        assert_eq!(v, json!({ "text": "Sí", "n": 1, "a": ["¿Qué?", "ok"], "o": { "k": "año" } }));
    }

    #[test]
    fn replies_to_cursor_are_ascii_json() {
        let ctx = Context::default();
        let reply = r#"{"decision":"followup","answers":{"message":"Sí, añade 😀"}}"#;
        let ascii = stdout(Wait::Followup, Some(reply), &ctx).unwrap();
        assert!(ascii.is_ascii());
        assert_eq!(ascii, r#"{"followup_message":"S\u00ed, a\u00f1ade \ud83d\ude00"}"#);
    }
}
