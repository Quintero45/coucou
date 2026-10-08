// Self-modification — the supervised path by which ARIA improves itself.
//
// Three pieces, all under the owner's eye:
//   * guard   — the protected core: files ARIA may never change, verified at
//               startup against hashes baked in at build time.
//   * skills  — small reusable tools ARIA writes, each an approved MCP server.
//   * evolve  — changes to ARIA's own source, in a git worktree, checked and
//               shown as a diff before anything is applied.
//
// Everything with a side effect here goes through policy::approve, and the core
// guard refuses writes into protected paths no matter who asks.

pub mod evolve;
pub mod guard;
pub mod skills;

use serde_json::Value;
use tauri::AppHandle;

use crate::providers::{ToolCall, ToolSpec};
use crate::tools::Outcome;

/// Tools these modules add to the assistant.
pub fn specs() -> Vec<ToolSpec> {
    let mut out = skills::specs();
    out.extend(evolve::specs());
    out
}

/// Handles a call if it belongs to one of these modules.
pub async fn run(app: &AppHandle, call: &ToolCall) -> Option<Outcome> {
    if let Some(outcome) = skills::run(app, call).await {
        return Some(outcome);
    }
    evolve::run(app, call).await
}

/// `{ "x": "..." }` shorthand used by the tool handlers.
pub(crate) fn arg<'a>(input: &'a Value, key: &str) -> &'a str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}
