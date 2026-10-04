// Skills — reusable tools Mochi writes for itself, installed only after the
// owner has read the code on the approval card.
//
// A skill is a folder in %APPDATA%\Coucou\skills\<name>\ with a skill.json and
// its files. Two kinds:
//   * "script": each tool runs one script (PowerShell .ps1, Python .py, Node
//     .js/.mjs) with the call's arguments as JSON on stdin; stdout is the result.
//   * "mcp": the skill is an MCP server (command + args), connected like any
//     other connection under the name `skill-<name>`.
// Skills are read from disk on every turn, so a new one is usable at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::arg;
use crate::policy::{self, Risk};
use crate::providers::{ToolCall, ToolSpec};
use crate::tools::Outcome;
use crate::{log, mcp, platform, settings};

pub const PREFIX: &str = "skill__";
const MAX_FILE: usize = 200_000;
const RUN_TIMEOUT: u64 = 180;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    #[serde(default = "empty_schema")]
    pub parameters: Value,
    /// Script file inside the skill folder (kind "script").
    #[serde(default)]
    pub script: String,
    /// Declared at install time, which the owner approved: runs without asking.
    #[serde(default)]
    pub read_only: bool,
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default = "script_kind")]
    pub kind: String,
    #[serde(default)]
    pub tools: Vec<SkillTool>,
    /// kind "mcp": how to start the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn script_kind() -> String {
    "script".into()
}

fn yes() -> bool {
    true
}

pub fn dir() -> PathBuf {
    settings::config_dir().join("skills")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_file(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub fn list() -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else { return out };
    for entry in entries.flatten() {
        let path = entry.path().join("skill.json");
        if let Some(skill) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Skill>(&b).ok()) {
            if valid_name(&skill.name) {
                out.push(skill);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// MCP-kind skills, for mcp.rs to connect alongside the owner's servers.
pub fn mcp_servers() -> Vec<(String, mcp::ServerConfig)> {
    list()
        .into_iter()
        .filter(|s| s.enabled && s.kind == "mcp" && s.command.is_some())
        .map(|s| {
            let cfg = mcp::ServerConfig {
                command: s.command.clone(),
                args: s.args.clone(),
                cwd: Some(dir().join(&s.name).display().to_string()),
                ..Default::default()
            };
            (format!("skill-{}", s.name), cfg)
        })
        .collect()
}

fn exposed(skill: &str, tool: &str) -> String {
    let mut name = format!("{PREFIX}{}__{}", skill.replace('-', "_"), tool);
    name.truncate(64);
    name
}

pub fn specs() -> Vec<ToolSpec> {
    let mut out = vec![ToolSpec {
        name: "create_skill".into(),
        description: "Write and install a reusable skill (a new tool for yourself). The owner reads the code and \
must approve. kind \"script\": every tool runs one script file from `files` (.ps1, .py, .js) that reads its JSON \
arguments from stdin and prints the result. kind \"mcp\": give `command` and `args` to start an MCP server \
from the files. Names: lowercase letters, digits and dashes."
            .into(),
        schema: json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "description": { "type": "string" },
                "kind": { "type": "string", "enum": ["script", "mcp"] },
                "tools": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "description": { "type": "string" },
                            "parameters": { "type": "object" },
                            "script": { "type": "string" },
                            "read_only": { "type": "boolean" }
                        },
                        "required": ["name", "description", "script"]
                    }
                },
                "command": { "type": "string" },
                "args": { "type": "array", "items": { "type": "string" } },
                "files": { "type": "object", "additionalProperties": { "type": "string" } }
            },
            "required": ["name", "description", "kind", "files"]
        }),
    }];
    for skill in list().into_iter().filter(|s| s.enabled && s.kind == "script") {
        for tool in &skill.tools {
            out.push(ToolSpec {
                name: exposed(&skill.name, &tool.name),
                description: format!("[skill {}] {}", skill.name, tool.description),
                schema: if tool.parameters.is_object() { tool.parameters.clone() } else { empty_schema() },
            });
        }
    }
    out
}

pub async fn run(app: &AppHandle, call: &ToolCall) -> Option<Outcome> {
    if call.name == "create_skill" {
        return Some(create(app, &call.input).await);
    }
    if !call.name.starts_with(PREFIX) {
        return None;
    }
    let found = list().into_iter().filter(|s| s.enabled && s.kind == "script").find_map(|s| {
        let tool = s.tools.iter().find(|t| exposed(&s.name, &t.name) == call.name)?.clone();
        Some((s, tool))
    });
    let Some((skill, tool)) = found else {
        return Some(Outcome::err(format!("Unknown skill tool {}", call.name)));
    };
    let label = format!("skill {} · {}", skill.name, tool.name);
    let args = call.input.to_string();
    let risk = if tool.read_only { Risk::Read } else { Risk::Act };
    match risk {
        Risk::Read => policy::audit(&label, "read", &args),
        Risk::Act => {
            if !policy::approve(app, &label, &args).await {
                return Some(Outcome::err("The owner declined this action. Do not retry it another way."));
            }
        }
    }
    Some(run_script(&dir().join(&skill.name), &tool.script, &args).await)
}

fn interpreter(script: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let ext = Path::new(script).extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "ps1" => Some(("powershell.exe", vec!["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])),
        "py" => Some(("python", vec![])),
        "js" | "mjs" | "cjs" => Some(("node", vec![])),
        _ => None,
    }
}

async fn run_script(folder: &Path, script: &str, input: &str) -> Outcome {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    if !valid_file(script) || !folder.join(script).is_file() {
        return Outcome::err(format!("script {script} not found in the skill"));
    }
    let Some((program, pre)) = interpreter(script) else {
        return Outcome::err(format!("no interpreter for {script} (use .ps1, .py or .js)"));
    };
    let Some(exe) = platform::find_on_path(program.trim_end_matches(".exe")) else {
        return Outcome::err(format!("{program} is not installed"));
    };
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(pre)
        .arg(folder.join(script))
        .current_dir(folder)
        .env("COUCOU_SKILL_DIR", folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::err(e.to_string()),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes()).await;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(RUN_TIMEOUT), child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let text = match (stdout.is_empty(), stderr.is_empty()) {
                (false, true) => stdout,
                (true, false) => format!("stderr:\n{stderr}"),
                (false, false) => format!("{stdout}\n\nstderr:\n{stderr}"),
                (true, true) => "(no output)".into(),
            };
            if out.status.success() { Outcome::ok(text) } else { Outcome::err(text) }
        }
        Ok(Err(e)) => Outcome::err(e.to_string()),
        Err(_) => Outcome::err(format!("the skill ran longer than {RUN_TIMEOUT} s and was stopped")),
    }
}

/// Validates, shows the full code on the approval card, and writes the skill
/// only after the click.
async fn create(app: &AppHandle, input: &Value) -> Outcome {
    let name = arg(input, "name").trim().to_string();
    if !valid_name(&name) {
        return Outcome::err("name: lowercase letters, digits and dashes, up to 32 characters");
    }
    let kind = match arg(input, "kind") {
        "mcp" => "mcp",
        _ => "script",
    };
    let files: BTreeMap<String, String> = input
        .get("files")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default();
    if files.is_empty() {
        return Outcome::err("files is empty");
    }
    for (file, content) in &files {
        if !valid_file(file) || file == "skill.json" {
            return Outcome::err(format!("invalid file name {file}"));
        }
        if content.len() > MAX_FILE {
            return Outcome::err(format!("{file} is too large"));
        }
    }
    let tools: Vec<SkillTool> = input
        .get("tools")
        .cloned()
        .map(|v| {
            serde_json::from_value::<Vec<Value>>(v)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| {
                    Some(SkillTool {
                        name: t["name"].as_str()?.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect(),
                        description: t["description"].as_str().unwrap_or_default().to_string(),
                        parameters: if t["parameters"].is_object() { t["parameters"].clone() } else { empty_schema() },
                        script: t["script"].as_str().unwrap_or_default().to_string(),
                        read_only: t["read_only"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if kind == "script" {
        if tools.is_empty() {
            return Outcome::err("a script skill needs at least one tool");
        }
        for t in &tools {
            if t.name.is_empty() || !files.contains_key(&t.script) || interpreter(&t.script).is_none() {
                return Outcome::err(format!("tool {}: its script must be one of the files and end in .ps1, .py or .js", t.name));
            }
        }
    }
    let command = (kind == "mcp").then(|| arg(input, "command").to_string()).filter(|c| !c.is_empty());
    if kind == "mcp" && command.is_none() {
        return Outcome::err("an mcp skill needs a command");
    }
    let args: Vec<String> = input
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let skill = Skill {
        name: name.clone(),
        description: arg(input, "description").to_string(),
        kind: kind.into(),
        tools,
        command,
        args,
        enabled: true,
    };
    let manifest = serde_json::to_string_pretty(&skill).unwrap_or_default();
    let exists = dir().join(&name).exists();

    let mut review = format!("skill.json\n{manifest}\n");
    for (file, content) in &files {
        review.push_str(&format!("\n──── {file} ────\n{content}\n"));
    }
    let target = format!(
        "{} skill \"{name}\" ({kind}) — {} file(s){}",
        if exists { "Replace" } else { "Install" },
        files.len(),
        if skill.tools.iter().any(|t| t.read_only) { ", some tools run without asking" } else { "" }
    );
    if !policy::approve_with_detail(app, "create_skill", &target, Some(&review)).await {
        return Outcome::err("The owner declined this skill.");
    }

    let folder = dir().join(&name);
    let staging = dir().join(format!(".{name}.new"));
    let _ = std::fs::remove_dir_all(&staging);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&staging)?;
        for (file, content) in &files {
            std::fs::write(staging.join(file), content)?;
        }
        std::fs::write(staging.join("skill.json"), &manifest)?;
        if folder.exists() {
            std::fs::remove_dir_all(&folder)?;
        }
        std::fs::rename(&staging, &folder)
    };
    if let Err(e) = write() {
        return Outcome::err(format!("could not install the skill: {e}"));
    }
    log::line(format!("skill {name} installed ({kind})"));
    if kind == "mcp" {
        mcp::start(app.clone());
    }
    Outcome::ok(format!(
        "Skill {name} installed in {}. {}",
        folder.display(),
        if kind == "mcp" { "Its MCP server is connecting." } else { "Its tools are available from your next step." }
    ))
}

// ── Settings window ───────────────────────────────────────────────────────────

pub fn set_enabled(app: &AppHandle, name: &str, enabled: bool) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid skill".into());
    }
    let path = dir().join(name).join("skill.json");
    let mut skill: Skill = serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    skill.enabled = enabled;
    std::fs::write(&path, serde_json::to_vec_pretty(&skill).unwrap_or_default()).map_err(|e| e.to_string())?;
    log::line(format!("skill {name} {}", if enabled { "enabled" } else { "disabled" }));
    if skill.kind == "mcp" {
        mcp::start(app.clone());
    }
    Ok(())
}

pub fn remove(app: &AppHandle, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err("invalid skill".into());
    }
    std::fs::remove_dir_all(dir().join(name)).map_err(|e| e.to_string())?;
    log::line(format!("skill {name} removed"));
    mcp::start(app.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_files_are_strict() {
        assert!(valid_name("resize-images"));
        assert!(!valid_name("Resize"));
        assert!(!valid_name("../x"));
        assert!(valid_file("run.ps1"));
        assert!(!valid_file("..\\evil.ps1"));
        assert!(!valid_file(".hidden"));
    }

    #[test]
    fn interpreters_by_extension() {
        assert_eq!(interpreter("a.ps1").unwrap().0, "powershell.exe");
        assert_eq!(interpreter("a.py").unwrap().0, "python");
        assert!(interpreter("a.exe").is_none());
    }

    #[test]
    fn exposed_names_fit() {
        assert_eq!(exposed("resize-images", "run"), "skill__resize_images__run");
        assert!(exposed(&"a".repeat(32), &"b".repeat(60)).len() <= 64);
    }
}
