// Approval policy for the assistant's own tools. Part of the protected core.
//
// Reading is free; anything with a side effect waits for a click on the island's
// approval card. No click in time, a paused island or a card already in use all
// mean no. "Always" lasts until the chat is reset, and only for that one tool.
// Every call — free, approved, refused — is written to the audit log.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;

use crate::island::WINDOW_LABEL;
use crate::log;
use crate::pipe::{Pending, Reply};

/// The island only has to put the card up; it does that synchronously.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    /// Reads only: runs without asking.
    Read,
    /// Changes something, or sends something out: needs a click.
    Act,
}

static ALWAYS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
static COUNTER: AtomicU64 = AtomicU64::new(1);

/// Forgets every "Always" — called when the chat is reset.
pub fn reset_session() {
    ALWAYS.lock().unwrap().clear();
}

pub fn audit(tool: &str, verdict: &str, target: &str) {
    let target: String = target.chars().take(300).collect();
    log::line(format!("assistant tool {tool} {verdict} · {}", target.replace('\n', " ")));
}

/// Waits for the owner's click. `target` is exactly what the card shows: the
/// command, the path, the URL — what the click authorises.
pub async fn approve(app: &AppHandle, tool: &str, target: &str) -> bool {
    approve_with_detail(app, tool, target, None).await
}

/// Same, with the full text under review (a skill's code, a diff) shown in a
/// scrollable box on the card. "Always" is never offered for these.
pub async fn approve_with_detail(app: &AppHandle, tool: &str, target: &str, detail: Option<&str>) -> bool {
    if detail.is_none() && ALWAYS.lock().unwrap().contains(tool) {
        audit(tool, "allowed (always)", target);
        return true;
    }
    let id = format!("assistant-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
    let (tx, mut rx) = mpsc::channel::<Reply>(4);
    app.state::<Pending>().0.lock().unwrap().insert(id.clone(), tx);
    let _ = app.emit_to(
        WINDOW_LABEL,
        "assistant-approval",
        json!({
            "requestId": id,
            "tool": tool,
            "command": target,
            "detail": detail,
            "allowAlways": detail.is_none(),
        }),
    );

    // Reading a diff or a skill's code takes longer than reading a command.
    let timeout = if detail.is_some() { REVIEW_TIMEOUT } else { DECISION_TIMEOUT };
    let verdict = wait(&mut rx, timeout).await;
    app.state::<Pending>().0.lock().unwrap().remove(&id);

    match verdict.as_deref() {
        Some("allow") => {
            audit(tool, "allowed", target);
            true
        }
        Some("always") if detail.is_none() => {
            ALWAYS.lock().unwrap().insert(tool.to_string());
            audit(tool, "allowed (always from now on)", target);
            true
        }
        Some(_) => {
            audit(tool, "denied", target);
            false
        }
        None => {
            audit(tool, "not answered — denied", target);
            false
        }
    }
}

async fn wait(rx: &mut mpsc::Receiver<Reply>, timeout: Duration) -> Option<String> {
    match tokio::time::timeout(ACK_TIMEOUT, rx.recv()).await {
        Ok(Some(Reply::Ack)) => {}
        Ok(Some(Reply::Decision(d))) => return Some(d),
        _ => return None,
    }
    match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(Reply::Decision(d))) => Some(d),
        _ => None,
    }
}
