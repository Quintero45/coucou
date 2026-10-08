//! Turns every agent's hook dialect into the canonical Claude Code events the
//! island understands, and the island's answer back into each agent's own
//! output format. Pure functions over JSON, so all of it is unit-tested.
//!
//! | Agent       | Event names                    | Answer format                         |
//! |-------------|--------------------------------|---------------------------------------|
//! | Claude Code | canonical                      | hookSpecificOutput.decision           |
//! | Codex       | canonical                      | same, never `updatedPermissions`      |
//! | Cursor      | camelCase (`beforeSubmitPrompt`)| `{"permission": "allow" | "deny" | "ask"}` |
//! | Gemini CLI  | `BeforeTool`, `AfterAgent`…    | `{}`                                  |
//! | Antigravity | `PreInvocation`…               | `{}`                                  |

use serde_json::{json, Map, Value};

/// What the relay has to do with an event once it is normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Forward and exit.
    Fire,
    /// Wait for Allow / Deny (/ Always) from the island.
    Permission,
    /// Claude Code's AskUserQuestion: wait for the chosen answers.
    Ask,
    /// Cursor's stop: the island may hand back orders as a `followup_message`.
    Followup,
}

/// Cursor events that can carry an approval. They only become a
/// `PermissionRequest` when the hook was installed with `--approve`; otherwise
/// they are plain tool steps.
const CURSOR_GATED: &[&str] = &["beforeShellExecution", "beforeMCPExecution"];

/// Gemini CLI, Antigravity and Cursor event names → canonical names.
pub fn canonical_event(raw: &str) -> &str {
    match raw {
        // Gemini CLI
        "BeforeTool" | "BeforeToolSelection" => "PreToolUse",
        "AfterTool" | "AfterModel" => "PostToolUse",
        "BeforeAgent" => "UserPromptSubmit",
        "AfterAgent" => "Stop",
        "startup" => "SessionStart",
        "exit" => "SessionEnd",
        // Antigravity
        "PreInvocation" => "UserPromptSubmit",
        "PostInvocation" => "PostToolUse",
        // Cursor
        "sessionStart" => "SessionStart",
        "sessionEnd" => "SessionEnd",
        "beforeSubmitPrompt" => "UserPromptSubmit",
        "preToolUse" | "beforeShellExecution" | "beforeMCPExecution" | "beforeReadFile" => "PreToolUse",
        "postToolUse" | "afterShellExecution" | "afterMCPExecution" | "afterFileEdit" => "PostToolUse",
        "postToolUseFailure" => "PostToolUseFailure",
        "stop" => "Stop",
        "subagentStart" => "SubagentStart",
        "subagentStop" => "SubagentStop",
        "afterAgentResponse" => "AgentResponse",
        "afterAgentThought" => "AgentThought",
        "preCompact" => "PreCompact",
        other => other,
    }
}

/// Normalises `map` in place and says what to do with it.
///
/// `approve` is the `--approve` flag: only then do Cursor's shell and MCP gates
/// wait for a human. `ask` is `--ask`, Claude Code's AskUserQuestion hook.
pub fn normalize(map: &mut Map<String, Value>, agent: &str, arg_event: &str, approve: bool, ask: bool) -> Kind {
    let raw = map
        .get("hook_event_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| arg_event.to_string());

    if ask {
        let tool = map.get("tool_name").and_then(Value::as_str).unwrap_or_default();
        let is_question = if agent == "cursor" { is_cursor_question(tool) } else { tool == "AskUserQuestion" };
        map.insert("hook_event_name".into(), json!(raw));
        if !is_question {
            return Kind::Fire;
        }
        if agent == "cursor" {
            cursor_questions_to_claude(map);
            normalize_tool_fields(map);
            // One single-choice question: a card with one button per option
            // and "Otra respuesta", answered through approval_decision.
            if cursor_question_to_choices(map) {
                map.insert("source_event".into(), json!(raw));
                return Kind::Ask;
            }
        }
        map.insert("coucou_kind".into(), json!("ask_user_question"));
        return Kind::Ask;
    }

    let mut event = canonical_event(&raw).to_string();
    if event != raw {
        map.insert("source_event".into(), json!(raw));
    }

    match raw.as_str() {
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
            let name = map.get("tool_name").and_then(Value::as_str).unwrap_or("tool").to_string();
            map.insert("tool_name".into(), json!(format!("MCP: {name}")));
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
        "stop" => {
            if map.get("status").and_then(Value::as_str) == Some("error") {
                event = "StopFailure".into();
            }
        }
        _ => {}
    }

    let gated = agent == "cursor" && approve && CURSOR_GATED.contains(&raw.as_str());
    if gated {
        event = "PermissionRequest".into();
    }

    // Cursor's postToolUse carries the whole tool output; the island never shows it.
    map.remove("tool_output");

    normalize_tool_fields(map);
    map.insert("hook_event_name".into(), json!(event));

    if agent == "cursor" && event == "Stop" {
        map.insert("coucou_kind".into(), json!("cursor_stop"));
        return Kind::Followup;
    }
    if event == "PermissionRequest" { Kind::Permission } else { Kind::Fire }
}

fn is_cursor_question(tool: &str) -> bool {
    matches!(tool.to_ascii_lowercase().replace('_', "").as_str(), "askquestion" | "askuserquestion")
}

/// Cursor's AskQuestion input (`title`, `questions[].prompt`, `options[].label`,
/// `allow_multiple`) → the AskUserQuestion shape the island reads (`question`,
/// `header`, `options[].label`, `multiSelect`).
fn cursor_questions_to_claude(map: &mut Map<String, Value>) {
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

/// At most this many buttons on a question card.
pub const MAX_OPTIONS: usize = 12;
/// Longest option label, in characters.
pub const MAX_LABEL_CHARS: usize = 60;
const MAX_DESCRIPTION_CHARS: usize = 200;

/// `(label, description)` pairs → the `options` the island reads
/// (`[{label, description?}]`). Labels are trimmed and cut to 60 characters,
/// descriptions to 200; a repeated label (ignoring case) is dropped. An empty
/// label, no options at all, or more than 12 is an error.
pub fn clean_options(raw: &[(String, String)]) -> Result<Vec<Value>, String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (i, (label, description)) in raw.iter().enumerate() {
        let label: String = label.trim().chars().take(MAX_LABEL_CHARS).collect();
        let label = label.trim_end();
        if label.is_empty() {
            return Err(format!("la opción {} está vacía", i + 1));
        }
        let key = label.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let description: String = description.trim().chars().take(MAX_DESCRIPTION_CHARS).collect();
        let description = description.trim_end();
        out.push(if description.is_empty() {
            json!({ "label": label })
        } else {
            json!({ "label": label, "description": description })
        });
    }
    if out.is_empty() {
        return Err("no hay ninguna opción".into());
    }
    if out.len() > MAX_OPTIONS {
        return Err(format!("como máximo {MAX_OPTIONS} opciones (hay {})", out.len()));
    }
    Ok(out)
}

/// A Cursor question already in the island's shape (`cursor_questions_to_claude`)
/// with exactly one single-choice question becomes a question card with
/// choices: `Pregunta` with the question in `tool_input.command`, `options` and
/// `allowCustom: true` at the top. The questions stay in `tool_input` for the
/// answer (`Context`). Several questions, multi-select, or options that do not
/// fit (empty, more than 12) keep the step-by-step question card.
fn cursor_question_to_choices(map: &mut Map<String, Value>) -> bool {
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
    let Ok(options) = clean_options(&raw) else { return false };
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

/// Gemini / Antigravity `toolCall`, Cursor `conversation_id` and
/// `workspace_roots` → `tool_name`, `tool_input`, `session_id`, `cwd`.
pub fn normalize_tool_fields(map: &mut Map<String, Value>) {
    if !map.contains_key("tool_name") {
        let tool = map.get("toolCall").and_then(Value::as_object).cloned().unwrap_or_default();
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| map.get("tool").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        if !name.is_empty() {
            map.insert("tool_name".into(), json!(name));
        }
        if !map.contains_key("tool_input") {
            if let Some(args) = tool.get("args").and_then(Value::as_object) {
                let mut flat = args.clone();
                for (src, dst) in [
                    ("CommandLine", "command"),
                    ("FilePath", "file_path"),
                    ("Path", "path"),
                    ("Url", "url"),
                    ("Query", "query"),
                    ("Pattern", "pattern"),
                ] {
                    if let Some(v) = flat.get(src).cloned() {
                        flat.insert(dst.into(), v);
                    }
                }
                map.insert("tool_input".into(), Value::Object(flat));
            }
        }
    }

    let has_session = map.get("session_id").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if !has_session {
        for k in ["conversation_id", "conversationId", "sessionId"] {
            if let Some(s) = map.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                let s = s.to_string();
                map.insert("session_id".into(), json!(s));
                break;
            }
        }
    }

    let has_cwd = map.get("cwd").and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if !has_cwd {
        let root = ["workspace_roots", "workspacePaths"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str))
            .map(str::to_string);
        if let Some(root) = root {
            map.insert("cwd".into(), json!(root));
        }
    }
}

/// The island's reply, as written on the pipe: a bare word (`allow`, `always`,
/// `deny`), a JSON line `{"decision":"answer","answers":{…}}`, or, for a
/// question card with choices, `{"decision":"allow","answer":"…"}` (kept as
/// `answers: {"answer": …}`; read it with `Reply::answer`).
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub decision: String,
    pub answers: Value,
}

impl Reply {
    /// The option picked (or the text typed) on a question card with choices.
    pub fn answer(&self) -> Option<&str> {
        if self.decision != "allow" {
            return None;
        }
        self.answers.get("answer").and_then(Value::as_str).filter(|a| !a.trim().is_empty())
    }
}

pub fn parse_reply(raw: &str) -> Option<Reply> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('{') {
        let v: Value = serde_json::from_str(raw).ok()?;
        let decision = v.get("decision").and_then(Value::as_str)?.to_string();
        if let Some(answer) = v.get("answer").and_then(Value::as_str) {
            return Some(Reply { decision, answers: json!({ "answer": answer }) });
        }
        return Some(Reply { decision, answers: v.get("answers").cloned().unwrap_or(json!({})) });
    }
    Some(Reply { decision: raw.to_string(), answers: json!({}) })
}

/// What the original payload carried that the answer needs: Claude Code's
/// suggested rules (for "Always") and the AskUserQuestion questions.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub suggestions: Value,
    pub questions: Value,
}

impl Context {
    pub fn from(map: &Map<String, Value>) -> Self {
        Self {
            suggestions: map.get("permission_suggestions").cloned().unwrap_or(Value::Null),
            questions: map
                .get("tool_input")
                .and_then(|i| i.get("questions"))
                .cloned()
                .unwrap_or(Value::Null),
        }
    }
}

/// The JSON to print on stdout, or None for silence. Silence always means
/// "as if Coucou were not installed", so anything unrecognised is silent —
/// except for Cursor's approval gates, where silence is replaced by `ask` so
/// Cursor puts the question in its own UI rather than proceeding.
pub fn output(agent: &str, kind: Kind, reply: Option<&Reply>, ctx: &Context) -> Option<String> {
    match kind {
        Kind::Fire => match agent {
            "gemini" | "antigravity" | "cursor" => Some("{}".into()),
            _ => None,
        },
        Kind::Followup => {
            let message = reply
                .filter(|r| r.decision == "followup")
                .and_then(|r| r.answers.get("message").and_then(Value::as_str))
                .filter(|m| !m.trim().is_empty());
            Some(match message {
                Some(m) => json!({ "followup_message": m }).to_string(),
                None => "{}".into(),
            })
        }
        Kind::Ask if agent == "cursor" => Some(cursor_answer(reply, ctx).to_string()),
        Kind::Ask => {
            let r = reply?;
            if r.decision != "answer" {
                return None;
            }
            Some(
                json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "allow",
                        "updatedInput": { "questions": ctx.questions, "answers": r.answers },
                    }
                })
                .to_string(),
            )
        }
        Kind::Permission if agent == "cursor" => {
            let permission = match reply.map(|r| r.decision.as_str()) {
                Some("allow" | "always") => json!({ "permission": "allow" }),
                Some("deny") => json!({
                    "permission": "deny",
                    "user_message": "Denied from Coucou",
                    "agent_message": "The user denied this action from Coucou.",
                }),
                _ => json!({ "permission": "ask" }),
            };
            Some(permission.to_string())
        }
        Kind::Permission => {
            let r = reply?;
            let decision = match r.decision.as_str() {
                "allow" => json!({ "behavior": "allow" }),
                "always" => {
                    let has_rules = ctx.suggestions.as_array().is_some_and(|a| !a.is_empty());
                    if agent == "codex" || !has_rules {
                        json!({ "behavior": "allow" })
                    } else {
                        json!({ "behavior": "allow", "updatedPermissions": ctx.suggestions })
                    }
                }
                "deny" => json!({ "behavior": "deny", "message": "Denied from Coucou" }),
                "answer" => json!({
                    "behavior": "allow",
                    "updatedInput": { "questions": ctx.questions, "answers": r.answers },
                }),
                _ => return None,
            };
            Some(
                json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
                    .to_string(),
            )
        }
    }
}

/// Cursor's preToolUse can't fill in a question's answers, only allow or deny
/// the tool. Answered in the island: deny the card and hand the agent the
/// answers (Cursor's hook output has no answer field: `agent_message` is the
/// only text that reaches the model). Anything else — including Rechazar on a
/// card with choices — allow, and Cursor shows its own card; an empty reply
/// would block the tool instead.
fn cursor_answer(reply: Option<&Reply>, ctx: &Context) -> Value {
    let picked = reply.and_then(Reply::answer).map(|a| {
        let question = ctx.questions.get(0).and_then(|q| q.get("question")).and_then(Value::as_str).unwrap_or("Pregunta");
        vec![(question.to_string(), a.to_string())]
    });
    let answers: Vec<(String, String)> = picked.unwrap_or_else(|| {
        reply
            .filter(|r| r.decision == "answer")
            .and_then(|r| r.answers.as_object())
            .map(|a| {
                a.iter()
                    .map(|(q, v)| (q.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    });
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

    #[test]
    fn cursor_events_become_canonical() {
        let mut m = obj(json!({
            "hook_event_name": "beforeSubmitPrompt",
            "conversation_id": "c1",
            "workspace_roots": ["C:/work/app"],
            "prompt": "fix it"
        }));
        assert_eq!(normalize(&mut m, "cursor", "", false, false), Kind::Fire);
        assert_eq!(m["hook_event_name"], "UserPromptSubmit");
        assert_eq!(m["source_event"], "beforeSubmitPrompt");
        assert_eq!(m["session_id"], "c1");
        assert_eq!(m["cwd"], "C:/work/app");
    }

    #[test]
    fn cursor_shell_gate_waits_only_with_approve() {
        let base = json!({ "hook_event_name": "beforeShellExecution", "command": "rm -rf build", "cwd": "C:/x" });
        let mut plain = obj(base.clone());
        assert_eq!(normalize(&mut plain, "cursor", "", false, false), Kind::Fire);
        assert_eq!(plain["hook_event_name"], "PreToolUse");
        assert_eq!(plain["tool_name"], "Shell");
        assert_eq!(plain["tool_input"]["command"], "rm -rf build");

        let mut gated = obj(base);
        assert_eq!(normalize(&mut gated, "cursor", "", true, false), Kind::Permission);
        assert_eq!(gated["hook_event_name"], "PermissionRequest");
    }

    #[test]
    fn cursor_mcp_input_string_is_parsed() {
        let mut m = obj(json!({
            "hook_event_name": "beforeMCPExecution",
            "tool_name": "create_issue",
            "tool_input": "{\"title\":\"bug\"}"
        }));
        normalize(&mut m, "cursor", "", true, false);
        assert_eq!(m["tool_name"], "MCP: create_issue");
        assert_eq!(m["tool_input"]["title"], "bug");
    }

    #[test]
    fn cursor_file_edit_keeps_edits_for_the_diff() {
        let mut m = obj(json!({
            "hook_event_name": "afterFileEdit",
            "file_path": "C:/a/b.ts",
            "edits": [{ "old_string": "a", "new_string": "b" }]
        }));
        normalize(&mut m, "cursor", "", false, false);
        assert_eq!(m["hook_event_name"], "PostToolUse");
        assert_eq!(m["tool_name"], "Edit");
        assert_eq!(m["tool_input"]["edits"][0]["new_string"], "b");
    }

    #[test]
    fn cursor_stop_with_error_is_a_failure() {
        let mut m = obj(json!({ "hook_event_name": "stop", "status": "error" }));
        assert_eq!(normalize(&mut m, "cursor", "", false, false), Kind::Fire);
        assert_eq!(m["hook_event_name"], "StopFailure");
        let mut ok = obj(json!({ "hook_event_name": "stop", "status": "completed" }));
        assert_eq!(normalize(&mut ok, "cursor", "", false, false), Kind::Followup);
        assert_eq!(ok["hook_event_name"], "Stop");
        assert_eq!(ok["coucou_kind"], "cursor_stop");
    }

    #[test]
    fn cursor_stop_hands_back_queued_orders() {
        let ctx = Context::default();
        let orders = parse_reply(r#"{"decision":"followup","answers":{"message":"run the tests"}}"#);
        let out: Value = serde_json::from_str(&output("cursor", Kind::Followup, orders.as_ref(), &ctx).unwrap()).unwrap();
        assert_eq!(out["followup_message"], "run the tests");
        assert_eq!(output("cursor", Kind::Followup, None, &ctx).unwrap(), "{}");
    }

    #[test]
    fn gemini_and_antigravity_fields_are_flattened() {
        let mut m = obj(json!({
            "toolCall": { "name": "run_shell", "args": { "CommandLine": "ls" } },
            "conversationId": "g1"
        }));
        normalize(&mut m, "antigravity", "PreInvocation", false, false);
        assert_eq!(m["hook_event_name"], "UserPromptSubmit");
        assert_eq!(m["tool_name"], "run_shell");
        assert_eq!(m["tool_input"]["command"], "ls");
        assert_eq!(m["session_id"], "g1");
        assert_eq!(canonical_event("AfterAgent"), "Stop");
    }

    #[test]
    fn claude_events_pass_through_unchanged() {
        let mut m = obj(json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" }));
        assert_eq!(normalize(&mut m, "", "", false, false), Kind::Permission);
        assert_eq!(m["hook_event_name"], "PermissionRequest");
        assert!(m.get("source_event").is_none());
    }

    #[test]
    fn ask_mode_only_waits_for_ask_user_question() {
        let mut q = obj(json!({ "hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion" }));
        assert_eq!(normalize(&mut q, "", "", false, true), Kind::Ask);
        assert_eq!(q["coucou_kind"], "ask_user_question");
        let mut other = obj(json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash" }));
        assert_eq!(normalize(&mut other, "", "", false, true), Kind::Fire);
    }

    #[test]
    fn cursor_questions_take_the_island_shape() {
        let mut q = obj(json!({
            "hook_event_name": "preToolUse",
            "tool_name": "AskQuestion",
            "conversation_id": "c1",
            "tool_input": {
                "title": "Motor",
                "questions": [{
                    "id": "engine",
                    "prompt": "¿Qué motor?",
                    "options": [{ "id": "a", "label": "Grok" }, { "id": "b", "label": "Claude" }],
                    "allow_multiple": true,
                }],
            },
        }));
        assert_eq!(normalize(&mut q, "cursor", "preToolUse", false, true), Kind::Ask);
        assert_eq!(q["coucou_kind"], "ask_user_question");
        assert_eq!(q["session_id"], "c1");
        let first = &q["tool_input"]["questions"][0];
        assert_eq!(first["question"], "¿Qué motor?");
        assert_eq!(first["header"], "Motor");
        assert_eq!(first["options"][1]["label"], "Claude");
        assert_eq!(first["multiSelect"], true);

        let mut shell = obj(json!({ "hook_event_name": "preToolUse", "tool_name": "Shell" }));
        assert_eq!(normalize(&mut shell, "cursor", "preToolUse", false, true), Kind::Fire);
    }

    #[test]
    fn options_are_trimmed_capped_and_deduped() {
        let p = |items: &[(&str, &str)]| {
            clean_options(&items.iter().map(|(l, d)| (l.to_string(), d.to_string())).collect::<Vec<_>>())
        };
        let out = p(&[(" Sí ", " la buena "), ("No", ""), ("sí", "repetida")]).unwrap();
        assert_eq!(out, vec![json!({ "label": "Sí", "description": "la buena" }), json!({ "label": "No" })]);
        let long = "x".repeat(100);
        assert_eq!(p(&[(long.as_str(), "")]).unwrap()[0]["label"].as_str().unwrap().chars().count(), MAX_LABEL_CHARS);
        assert!(p(&[("A", ""), ("  ", "")]).is_err(), "empty label");
        assert!(p(&[]).is_err());
        let thirteen: Vec<(String, String)> = (0..13).map(|i| (format!("o{i}"), String::new())).collect();
        assert!(clean_options(&thirteen).is_err());
        assert_eq!(clean_options(&thirteen[..12]).unwrap().len(), 12);
    }

    #[test]
    fn a_single_cursor_question_becomes_a_card_with_choices() {
        let mut q = obj(json!({
            "hook_event_name": "preToolUse",
            "tool_name": "AskQuestion",
            "tool_input": { "title": "Motor", "questions": [{
                "prompt": "¿Qué motor?",
                "options": [{ "id": "a", "label": "Grok", "description": "rápido" }, { "id": "b", "label": "Claude" }],
            }] },
        }));
        assert_eq!(normalize(&mut q, "cursor", "preToolUse", false, true), Kind::Ask);
        assert_eq!(q["hook_event_name"], "PermissionRequest");
        assert_eq!(q["tool_name"], "Pregunta");
        assert_eq!(q["source_tool"], "AskQuestion");
        assert!(q.get("coucou_kind").is_none(), "not the step-by-step question card");
        assert_eq!(q["tool_input"]["command"], "¿Qué motor?");
        assert_eq!(q["options"], json!([{ "label": "Grok", "description": "rápido" }, { "label": "Claude" }]));
        assert_eq!(q["allowCustom"], true);

        // The pick goes back as the deny reason, the only text Cursor relays.
        let ctx = Context::from(&q);
        let picked = parse_reply(r#"{"decision":"allow","answer":"Otro: Gemini"}"#);
        let out: Value = serde_json::from_str(&output("cursor", Kind::Ask, picked.as_ref(), &ctx).unwrap()).unwrap();
        assert_eq!(out["permission"], "deny");
        assert!(out["agent_message"].as_str().unwrap().contains("¿Qué motor? → Otro: Gemini"));
        // Rechazar, a bare allow, or nothing: Cursor shows its own card, as before.
        for reply in [None, parse_reply("deny"), parse_reply("allow")] {
            assert_eq!(output("cursor", Kind::Ask, reply.as_ref(), &ctx).unwrap(), r#"{"permission":"allow"}"#);
        }
    }

    #[test]
    fn multi_select_or_several_cursor_questions_keep_the_question_card() {
        let multi = json!({ "hook_event_name": "preToolUse", "tool_name": "AskQuestion", "tool_input": { "questions": [
            { "prompt": "¿Cuáles?", "options": ["a", "b"], "allow_multiple": true } ] } });
        let two = json!({ "hook_event_name": "preToolUse", "tool_name": "AskQuestion", "tool_input": { "questions": [
            { "prompt": "¿Uno?", "options": ["a"] }, { "prompt": "¿Dos?", "options": ["b"] } ] } });
        for v in [multi, two] {
            let mut m = obj(v);
            assert_eq!(normalize(&mut m, "cursor", "preToolUse", false, true), Kind::Ask);
            assert_eq!(m["coucou_kind"], "ask_user_question");
            assert!(m.get("options").is_none());
        }
    }

    #[test]
    fn an_answer_only_counts_with_allow() {
        let r = parse_reply(r#"{"decision":"allow","answer":"B"}"#).unwrap();
        assert_eq!(r.answer(), Some("B"));
        assert_eq!(parse_reply("allow").unwrap().answer(), None);
        assert_eq!(parse_reply(r#"{"decision":"deny","answer":"B"}"#).unwrap().answer(), None);
        // A Claude Code tool approved through a card with choices is a plain allow.
        let ctx = Context::default();
        let out: Value = serde_json::from_str(&output("", Kind::Permission, Some(&r), &ctx).unwrap()).unwrap();
        assert_eq!(out, json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}));
    }

    #[test]
    fn cursor_answers_deny_with_the_answers_and_otherwise_allow() {
        let ctx = Context::default();
        let answered = parse_reply(r#"{"decision":"answer","answers":{"¿Qué motor?":"Grok"}}"#);
        let out: Value = serde_json::from_str(&output("cursor", Kind::Ask, answered.as_ref(), &ctx).unwrap()).unwrap();
        assert_eq!(out["permission"], "deny");
        assert!(out["agent_message"].as_str().unwrap().contains("¿Qué motor? → Grok"));
        assert!(out["user_message"].as_str().unwrap().contains("Grok"));

        for reply in [None, parse_reply("deny"), parse_reply(r#"{"decision":"answer","answers":{}}"#)] {
            let out = output("cursor", Kind::Ask, reply.as_ref(), &ctx).unwrap();
            assert_eq!(out, r#"{"permission":"allow"}"#);
        }
    }

    #[test]
    fn replies_parse_words_and_json() {
        assert_eq!(parse_reply("allow\n").unwrap().decision, "allow");
        let r = parse_reply(r#"{"decision":"answer","answers":{"Q":"A"}}"#).unwrap();
        assert_eq!(r.decision, "answer");
        assert_eq!(r.answers["Q"], "A");
        assert!(parse_reply("  ").is_none());
    }

    #[test]
    fn claude_output_matches_the_documented_shape() {
        let ctx = Context::default();
        let allow = Reply { decision: "allow".into(), answers: json!({}) };
        let out: Value = serde_json::from_str(&output("", Kind::Permission, Some(&allow), &ctx).unwrap()).unwrap();
        assert_eq!(
            out,
            json!({"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}})
        );
        let deny = Reply { decision: "deny".into(), answers: json!({}) };
        assert!(output("", Kind::Permission, Some(&deny), &ctx).unwrap().contains(r#""behavior":"deny""#));
        assert!(output("", Kind::Permission, None, &ctx).is_none());
        let junk = Reply { decision: "maybe".into(), answers: json!({}) };
        assert!(output("", Kind::Permission, Some(&junk), &ctx).is_none());
    }

    #[test]
    fn always_writes_rules_for_claude_but_never_for_codex() {
        let ctx = Context { suggestions: json!([{ "type": "addRules" }]), questions: Value::Null };
        let always = Reply { decision: "always".into(), answers: json!({}) };
        assert!(output("", Kind::Permission, Some(&always), &ctx).unwrap().contains("updatedPermissions"));
        assert!(!output("codex", Kind::Permission, Some(&always), &ctx).unwrap().contains("updatedPermissions"));
        // No suggestions: a plain allow, never an empty rule list.
        let none = Context::default();
        assert!(!output("", Kind::Permission, Some(&always), &none).unwrap().contains("updatedPermissions"));
    }

    #[test]
    fn cursor_gate_falls_back_to_ask_never_allow() {
        let ctx = Context::default();
        assert_eq!(output("cursor", Kind::Permission, None, &ctx).unwrap(), r#"{"permission":"ask"}"#);
        let allow = Reply { decision: "allow".into(), answers: json!({}) };
        assert_eq!(output("cursor", Kind::Permission, Some(&allow), &ctx).unwrap(), r#"{"permission":"allow"}"#);
        let deny = Reply { decision: "deny".into(), answers: json!({}) };
        assert!(output("cursor", Kind::Permission, Some(&deny), &ctx).unwrap().contains(r#""permission":"deny""#));
        assert_eq!(output("cursor", Kind::Fire, None, &ctx).unwrap(), "{}");
    }

    #[test]
    fn ask_answer_rewrites_the_tool_input() {
        let ctx = Context { suggestions: Value::Null, questions: json!([{ "question": "Which?" }]) };
        let r = Reply { decision: "answer".into(), answers: json!({ "Which?": "This one" }) };
        let out: Value = serde_json::from_str(&output("", Kind::Ask, Some(&r), &ctx).unwrap()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(out["hookSpecificOutput"]["updatedInput"]["answers"]["Which?"], "This one");
        assert!(output("", Kind::Ask, None, &ctx).is_none());
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
        // Already repaired text stays repaired.
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
        let json = json!({ "followup_message": "Sí, añade 😀" }).to_string();
        let ascii = ascii_json(&json);
        assert!(ascii.is_ascii());
        assert_eq!(ascii, r#"{"followup_message":"S\u00ed, a\u00f1ade \ud83d\ude00"}"#);
        assert_eq!(serde_json::from_str::<Value>(&ascii).unwrap(), serde_json::from_str::<Value>(&json).unwrap());
    }
}
