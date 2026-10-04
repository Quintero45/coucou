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
/// `deny`) or a JSON line `{"decision":"answer","answers":{…}}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub decision: String,
    pub answers: Value,
}

pub fn parse_reply(raw: &str) -> Option<Reply> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw.starts_with('{') {
        let v: Value = serde_json::from_str(raw).ok()?;
        let decision = v.get("decision").and_then(Value::as_str)?.to_string();
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
        Kind::Ask if agent == "cursor" => Some(cursor_answer(reply).to_string()),
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
/// answers. Anything else: allow, and Cursor shows its own card — an empty
/// reply would block the tool instead.
fn cursor_answer(reply: Option<&Reply>) -> Value {
    let answers: Vec<(String, String)> = reply
        .filter(|r| r.decision == "answer")
        .and_then(|r| r.answers.as_object())
        .map(|a| {
            a.iter()
                .map(|(q, v)| (q.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
                .collect()
        })
        .unwrap_or_default();
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
        normalize(&mut m, "cursor", "", false, false);
        assert_eq!(m["hook_event_name"], "StopFailure");
        let mut ok = obj(json!({ "hook_event_name": "stop", "status": "completed" }));
        normalize(&mut ok, "cursor", "", false, false);
        assert_eq!(ok["hook_event_name"], "Stop");
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
}
