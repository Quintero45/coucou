// The machine: run commands, open apps and URLs, the clipboard, what is running.
// Every one with a side effect needs the owner's click (see tools/mod.rs).

use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use super::{expand_path, schema, Outcome, Tool};
use crate::platform;
use crate::policy::Risk;

const DEFAULT_TIMEOUT: u64 = 60;
const MAX_TIMEOUT: u64 = 600;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "run_powershell",
            description: "Run a PowerShell command on the owner's Windows PC and get its output (stdout, stderr, exit code). Optional working folder and timeout in seconds (default 60, max 600). Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({
                "command": { "type": "string" },
                "cwd": { "type": "string" },
                "timeout_secs": { "type": "integer" },
            }), &["command"]),
        },
        Tool {
            name: "open_app",
            description: "Open an app (by name like \"notepad\", \"chrome\", \"code\", or full path), a file or a folder, with optional arguments. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({
                "target": { "type": "string" },
                "args": { "type": "string" },
            }), &["target"]),
        },
        Tool {
            name: "open_url",
            description: "Open a web page in the default browser. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({ "url": { "type": "string" } }), &["url"]),
        },
        Tool {
            name: "clipboard_read",
            description: "Read the text currently on the clipboard. Needs the owner's click (it may hold private data).",
            risk: Risk::Act,
            schema: || schema(json!({}), &[]),
        },
        Tool {
            name: "clipboard_write",
            description: "Put text on the clipboard. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({ "text": { "type": "string" } }), &["text"]),
        },
        Tool {
            name: "system_info",
            description: "Basic facts about this PC: OS, user, machine name, CPU count, home folder, local time.",
            risk: Risk::Read,
            schema: || schema(json!({}), &[]),
        },
        Tool {
            name: "list_processes",
            description: "The running processes using the most CPU, with memory use.",
            risk: Risk::Read,
            schema: || schema(json!({}), &[]),
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "run_powershell" | "open_app" | "open_url" | "clipboard_read" | "clipboard_write" | "system_info" | "list_processes"
    )
}

pub async fn run(name: &str, input: &Value) -> Outcome {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match name {
        "run_powershell" => {
            let timeout = input
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT)
                .clamp(1, MAX_TIMEOUT);
            let cwd = s("cwd");
            run_shell(&s("command"), (!cwd.is_empty()).then(|| expand_path(&cwd)), timeout, None).await
        }
        "open_app" => {
            let target = s("target");
            let args = s("args");
            let resolved = if target.contains(['\\', '/']) || target.starts_with('~') {
                expand_path(&target).display().to_string()
            } else {
                target.clone()
            };
            match platform::shell_open(&resolved, (!args.is_empty()).then_some(args.as_str())) {
                Ok(()) => Outcome::ok(format!("Opened {resolved}.")),
                Err(e) => Outcome::err(e),
            }
        }
        "open_url" => {
            let url = s("url");
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Outcome::err("Only http:// and https:// URLs can be opened.");
            }
            platform::open_url(&url);
            Outcome::ok(format!("Opened {url}."))
        }
        "clipboard_read" => {
            #[cfg(windows)]
            let script = "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-Clipboard -Raw";
            #[cfg(not(windows))]
            let script = "wl-paste 2>/dev/null || xclip -o -selection clipboard";
            run_shell(script, None, 15, None).await
        }
        "clipboard_write" => {
            #[cfg(windows)]
            let script = "[Console]::InputEncoding=[Text.Encoding]::UTF8; Set-Clipboard -Value ([Console]::In.ReadToEnd())";
            #[cfg(not(windows))]
            let script = "wl-copy 2>/dev/null || xclip -selection clipboard";
            let out = run_shell(script, None, 15, Some(s("text"))).await;
            if out.is_error { out } else { Outcome::ok("Copied to the clipboard.") }
        }
        "system_info" => {
            let t = platform::local_time();
            let env = |k: &str| std::env::var(k).unwrap_or_default();
            Outcome::ok(format!(
                "OS: {} ({})\nUser: {}\nMachine: {}\nCPUs: {}\nHome: {}\nLocal time: {:04}-{:02}-{:02} {:02}:{:02}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                if cfg!(windows) { env("USERNAME") } else { env("USER") },
                if cfg!(windows) { env("COMPUTERNAME") } else { env("HOSTNAME") },
                std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
                platform::home_dir().display(),
                t.year, t.month, t.day, t.hour, t.minute,
            ))
        }
        "list_processes" => {
            #[cfg(windows)]
            let script = "Get-Process | Sort-Object CPU -Descending | Select-Object -First 40 Name,Id,@{n='CPU(s)';e={[math]::Round($_.CPU,1)}},@{n='MB';e={[math]::Round($_.WorkingSet64/1MB)}} | Format-Table -AutoSize | Out-String -Width 160";
            #[cfg(not(windows))]
            let script = "ps -eo comm,pid,%cpu,rss --sort=-%cpu | head -40";
            run_shell(script, None, 20, None).await
        }
        _ => Outcome::err("unknown system tool"),
    }
}

/// Runs a shell script with a timeout; the process dies with the timeout.
pub async fn run_shell(
    script: &str,
    cwd: Option<std::path::PathBuf>,
    timeout_secs: u64,
    stdin: Option<String>,
) -> Outcome {
    if script.trim().is_empty() {
        return Outcome::err("empty command");
    }
    let mut cmd = platform::shell_command(script);
    if let Some(dir) = cwd {
        if !dir.is_dir() {
            return Outcome::err(format!("{} is not a folder", dir.display()));
        }
        cmd.current_dir(dir);
    }
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::err(format!("could not start {}: {e}", platform::SHELL_NAME)),
    };
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes()).await;
        drop(pipe);
    }
    match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);
            let mut text = String::new();
            if !stdout.trim().is_empty() {
                text.push_str(stdout.trim_end());
            }
            if !stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str("stderr:\n");
                text.push_str(stderr.trim_end());
            }
            if text.is_empty() {
                text.push_str("(no output)");
            }
            text.push_str(&format!("\n\nexit code {code}"));
            if out.status.success() { Outcome::ok(text) } else { Outcome::err(text) }
        }
        Ok(Err(e)) => Outcome::err(e.to_string()),
        Err(_) => Outcome::err(format!("timed out after {timeout_secs} s — the command was stopped")),
    }
}
