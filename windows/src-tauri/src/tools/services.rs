// The integrations the owner configured, as tools: what the pollers last saw,
// read-only GitHub, n8n webhooks and Resend emails (both after a click).

use std::time::Duration;

use serde_json::{json, Value};

use super::{schema, Outcome, Tool};
use crate::policy::Risk;
use crate::{integrations, secrets};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "integration_status",
            description: "What ARIA's integrations (Stripe, GitHub, Vercel, n8n, Resend, Notion, Cal.com) last reported: balances, deployments, notifications, executions, bookings…",
            risk: Risk::Read,
            schema: || schema(json!({}), &[]),
        },
        Tool {
            name: "github_api",
            description: "Read-only GET on the GitHub REST API with the owner's token, e.g. \"/user/repos?sort=updated\", \"/repos/OWNER/REPO/pulls\", \"/notifications\".",
            risk: Risk::Read,
            schema: || schema(json!({ "path": { "type": "string" } }), &["path"]),
        },
        Tool {
            name: "n8n_webhook",
            description: "Trigger an n8n workflow through its webhook path (the part after /webhook/), with an optional JSON payload. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({
                "path": { "type": "string" },
                "payload": { "type": "object" },
            }), &["path"]),
        },
        Tool {
            name: "send_email",
            description: "Send an email through the owner's Resend account. `from` must be an address on a domain verified in Resend. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({
                "from": { "type": "string" },
                "to": { "type": "string" },
                "subject": { "type": "string" },
                "text": { "type": "string" },
            }), &["from", "to", "subject", "text"]),
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "integration_status" | "github_api" | "n8n_webhook" | "send_email")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Coucou")
        .build()
        .unwrap_or_default()
}

async fn body_of(response: reqwest::Response) -> Outcome {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_success() {
        Outcome::ok(if text.is_empty() { format!("HTTP {status}") } else { text })
    } else {
        Outcome::err(format!("HTTP {status}: {}", text.chars().take(1000).collect::<String>()))
    }
}

pub async fn run(name: &str, input: &Value) -> Outcome {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match name {
        "integration_status" => {
            let reports = integrations::last_reports();
            if reports.as_object().is_none_or(|m| m.is_empty()) {
                Outcome::ok("No integration has reported yet (none configured, or the first poll has not run).")
            } else {
                Outcome::ok(serde_json::to_string_pretty(&reports).unwrap_or_default())
            }
        }
        "github_api" => {
            let path = s("path");
            if !path.starts_with('/') || path.contains("://") {
                return Outcome::err("path must start with / (for example /user/repos)");
            }
            let Some(token) = secrets::get("github-token") else { return Outcome::err("No GitHub token saved.") };
            match client()
                .get(format!("https://api.github.com{path}"))
                .bearer_auth(token)
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
            {
                Ok(r) => body_of(r).await,
                Err(e) => Outcome::err(format!("Network error: {e}")),
            }
        }
        "n8n_webhook" => {
            let Some(base) = secrets::get("n8n-url") else { return Outcome::err("No n8n URL saved.") };
            let path = s("path");
            if path.contains("..") || path.contains("://") {
                return Outcome::err("invalid webhook path");
            }
            let url = format!("{}/webhook/{}", base.trim_end_matches('/'), path.trim_start_matches('/'));
            let payload = input.get("payload").cloned().unwrap_or_else(|| json!({}));
            match client().post(url).json(&payload).send().await {
                Ok(r) => body_of(r).await,
                Err(e) => Outcome::err(format!("Network error: {e}")),
            }
        }
        "send_email" => {
            let Some(key) = secrets::get("resend-api-key") else { return Outcome::err("No Resend key saved.") };
            let body = json!({ "from": s("from"), "to": [s("to")], "subject": s("subject"), "text": s("text") });
            match client().post("https://api.resend.com/emails").bearer_auth(key).json(&body).send().await {
                Ok(r) => body_of(r).await,
                Err(e) => Outcome::err(format!("Network error: {e}")),
            }
        }
        _ => Outcome::err("unknown service tool"),
    }
}
