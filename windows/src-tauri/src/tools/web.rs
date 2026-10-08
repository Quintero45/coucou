// The web: fetch a page (after a click — the URL itself can carry data out) and
// search through Brave when the owner saved a Brave Search key. Anthropic
// models also have the server-side web search.

use std::time::Duration;

use serde_json::{json, Value};

use super::{schema, Outcome, Tool};
use crate::policy::Risk;
use crate::secrets;

const MAX_PAGE: usize = 3_000_000;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "web_fetch",
            description: "Download a web page and return its readable text. Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({ "url": { "type": "string" } }), &["url"]),
        },
        Tool {
            name: "web_search",
            description: "Search the web (Brave Search). Returns titles, URLs and snippets.",
            risk: Risk::Read,
            schema: || schema(json!({ "query": { "type": "string" } }), &["query"]),
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "web_fetch" | "web_search")
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) ARIA")
        .build()
        .unwrap_or_default()
}

pub async fn run(name: &str, input: &Value) -> Outcome {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match name {
        "web_fetch" => fetch(&s("url")).await,
        "web_search" => search(&s("query")).await,
        _ => Outcome::err("unknown web tool"),
    }
}

async fn fetch(url: &str) -> Outcome {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Outcome::err("Only http:// and https:// URLs can be fetched.");
    }
    let response = match client().get(url).send().await {
        Ok(r) => r,
        Err(e) => return Outcome::err(format!("Network error: {e}")),
    };
    let status = response.status();
    let html = match response.text().await {
        Ok(t) => t,
        Err(e) => return Outcome::err(e.to_string()),
    };
    let html: String = html.chars().take(MAX_PAGE).collect();
    let text = html_to_text(&html);
    if status.is_success() {
        Outcome::ok(format!("{url}\n\n{text}"))
    } else {
        Outcome::err(format!("HTTP {status}\n\n{}", text.chars().take(2000).collect::<String>()))
    }
}

async fn search(query: &str) -> Outcome {
    let Some(key) = secrets::get("brave-api-key") else {
        return Outcome::err("No Brave Search key saved.");
    };
    let response = client()
        .get("https://api.search.brave.com/res/v1/web/search")
        .query(&[("q", query), ("count", "8")])
        .header("X-Subscription-Token", key)
        .header("Accept", "application/json")
        .send()
        .await;
    let value: Value = match response {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or_default(),
        Ok(r) => return Outcome::err(format!("Brave Search {}", r.status())),
        Err(e) => return Outcome::err(format!("Network error: {e}")),
    };
    let results: Vec<String> = value["web"]["results"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|r| {
                    format!(
                        "{}\n{}\n{}",
                        r["title"].as_str().unwrap_or_default(),
                        r["url"].as_str().unwrap_or_default(),
                        html_to_text(r["description"].as_str().unwrap_or_default()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if results.is_empty() {
        Outcome::ok("No result.")
    } else {
        Outcome::ok(results.join("\n\n"))
    }
}

/// Good-enough readable text: drop scripts, styles and tags, decode the common
/// entities, collapse blank space.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 3);
    // ASCII-only lowering keeps byte offsets identical to `html`.
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            for skip in ["script", "style", "noscript", "svg"] {
                if lower[i + 1..].starts_with(skip) {
                    let close = format!("</{skip}");
                    if let Some(end) = lower[i..].find(&close) {
                        i += end;
                    }
                    break;
                }
            }
            let block = ["<p", "<br", "<div", "<li", "<h1", "<h2", "<h3", "<tr", "</p", "</div"]
                .iter()
                .any(|t| lower[i..].starts_with(t));
            match html[i..].find('>') {
                Some(end) => i += end + 1,
                None => break,
            }
            out.push(if block { '\n' } else { ' ' });
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(collapsed);
        }
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::html_to_text;

    #[test]
    fn html_becomes_readable_text() {
        let html = "<html><head><style>p{}</style><script>var x=1;</script></head><body><h1>Título</h1><p>Hola &amp; adiós</p><div>dos</div></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Título"));
        assert!(text.contains("Hola & adiós"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("p{}"));
    }
}
