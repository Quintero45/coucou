// The assistant's tools. Each one declares its risk: reads run straight away,
// anything with a side effect goes through policy::approve first, showing the
// owner exactly what the click authorises. MCP servers, skills and the
// self-evolution tools plug in here too, under their own name prefixes.

pub mod files;
mod services;
pub mod system;
mod web;

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::policy::{self, Risk};
use crate::providers::{Part, ToolCall, ToolSpec};
use crate::{mcp, memory, selfmod};

const MAX_OUTPUT: usize = 24_000;

pub struct Outcome {
    pub text: String,
    pub is_error: bool,
    /// Images the tool returned (a screenshot from an MCP server), as Part::Image.
    pub images: Vec<Part>,
}

impl Outcome {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: cap(text.into()), is_error: false, images: Vec::new() }
    }
    pub fn err(text: impl Into<String>) -> Self {
        Self { text: cap(text.into()), is_error: true, images: Vec::new() }
    }
}

fn cap(mut text: String) -> String {
    if text.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (cut)");
    }
    text
}

/// A built-in tool.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub risk: Risk,
    pub schema: fn() -> Value,
}

pub fn schema(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn memory_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "remember",
            description: "Save a short note to your long-term memory (preferences, facts about the owner, things to keep in mind). It is shown to you in every future conversation.",
            risk: Risk::Read,
            schema: || schema(json!({ "note": { "type": "string" } }), &["note"]),
        },
        Tool {
            name: "forget",
            description: "Remove every memory note containing the given text.",
            risk: Risk::Read,
            schema: || schema(json!({ "text": { "type": "string" } }), &["text"]),
        },
    ]
}

fn builtins() -> Vec<Tool> {
    let mut all = Vec::new();
    all.extend(files::tools());
    all.extend(system::tools());
    all.extend(web::tools());
    all.extend(services::tools());
    all.extend(memory_tools());
    all
}

/// Everything the model may call right now.
pub fn specs(app: &AppHandle) -> Vec<ToolSpec> {
    let mut out: Vec<ToolSpec> = builtins()
        .into_iter()
        .filter(|t| available(t.name))
        .map(|t| ToolSpec { name: t.name.into(), description: t.description.into(), schema: (t.schema)() })
        .collect();
    out.extend(mcp::specs(app));
    out.extend(crate::grokbot::specs(app));
    out.extend(selfmod::specs());
    out
}

/// What a Grok Bot may call through `aria-hook tool` (pipe.rs): the built-in
/// tools only. Not memory (it is ARIA's), not MCP, skills, self-evolution or
/// send_to_grok_bot — those keep their own gates and stay with ARIA.
pub fn for_bots() -> Vec<Tool> {
    builtins()
        .into_iter()
        .filter(|t| !matches!(t.name, "remember" | "forget") && available(t.name))
        .collect()
}

/// One of `for_bots()`, by name.
pub fn for_bot(name: &str) -> Option<Tool> {
    for_bots().into_iter().find(|t| t.name == name)
}

/// Tools that need something configured first stay hidden until it is.
fn available(name: &str) -> bool {
    match name {
        "web_search" => crate::secrets::present("brave-api-key"),
        "github_api" => crate::secrets::present("github-token"),
        "n8n_webhook" => crate::secrets::present("n8n-url"),
        "send_email" => crate::secrets::present("resend-api-key"),
        _ => true,
    }
}

/// One line saying what a call does — the approval card and the audit log show it.
pub fn describe(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match name {
        "run_powershell" => {
            let cwd = s("cwd");
            if cwd.is_empty() { s("command") } else { format!("{}   (in {cwd})", s("command")) }
        }
        "write_file" => format!("{} ({} chars, {})", s("path"), s("content").chars().count(), if s("mode") == "append" { "append" } else { "overwrite" }),
        "open_app" => {
            let args = s("args");
            if args.is_empty() { s("target") } else { format!("{} {args}", s("target")) }
        }
        "open_url" | "web_fetch" => s("url"),
        "clipboard_write" => format!("{} chars", s("text").chars().count()),
        "send_email" => format!("to {} · {}", s("to"), s("subject")),
        "n8n_webhook" => s("path"),
        _ => {
            let raw = input.to_string();
            if raw == "{}" { String::new() } else { raw.chars().take(200).collect() }
        }
    }
}

pub async fn run(app: &AppHandle, call: &ToolCall) -> Outcome {
    if call.name.starts_with(mcp::PREFIX) {
        return mcp::call(app, call).await;
    }
    if let Some(outcome) = crate::grokbot::run(app, call).await {
        return outcome;
    }
    if let Some(outcome) = selfmod::run(app, call).await {
        return outcome;
    }
    let Some(tool) = builtins().into_iter().find(|t| t.name == call.name && available(t.name)) else {
        return Outcome::err(format!("Unknown tool {}", call.name));
    };
    let target = describe(&call.name, &call.input);
    match tool.risk {
        Risk::Read => policy::audit(&call.name, "read", &target),
        Risk::Act => {
            if !policy::approve(app, &call.name, &target).await {
                return Outcome::err("The owner declined this action. Do not retry it another way; ask what they want instead.");
            }
        }
    }
    execute(&call.name, &call.input).await
}

/// Runs a built-in tool once its gate has been passed: policy::audit for a
/// read, an owner's click for a side effect. `run` above and pipe.rs's Grok
/// Bot route are the only callers; never call it without that gate.
pub async fn execute(name: &str, input: &Value) -> Outcome {
    match name {
        "remember" => match memory::remember(input["note"].as_str().unwrap_or_default()) {
            Ok(()) => Outcome::ok("Saved to memory."),
            Err(e) => Outcome::err(e),
        },
        "forget" => match memory::forget(input["text"].as_str().unwrap_or_default()) {
            Ok(n) => Outcome::ok(format!("Removed {n} note(s).")),
            Err(e) => Outcome::err(e),
        },
        name if files::handles(name) => files::run(name, input).await,
        name if system::handles(name) => system::run(name, input).await,
        name if web::handles(name) => web::run(name, input).await,
        name if services::handles(name) => services::run(name, input).await,
        other => Outcome::err(format!("Unknown tool {other}")),
    }
}

/// `~` and relative paths are taken from the home folder.
pub fn expand_path(raw: &str) -> std::path::PathBuf {
    let raw = raw.trim();
    let home = crate::platform::home_dir();
    if raw == "~" {
        return home;
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return home.join(rest);
    }
    let p = std::path::PathBuf::from(raw);
    if p.is_absolute() { p } else { home.join(p) }
}
