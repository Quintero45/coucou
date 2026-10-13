// What is left of the original Claude client: the default model, the chat
// context the island sends, and files turned into message parts — the same
// rules as readFileAsBlock() in ClaudeService.swift. The conversation itself
// lives in agent.rs and talks to every provider through providers.rs.

use serde::Deserialize;

use crate::providers::Part;

/// Text and code files are inlined; anything larger is skipped, as on macOS.
const MAX_INLINE_TEXT: u64 = 200_000;

pub const DEFAULT_MODEL: &str = "claude-opus-5";

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatContext {
    File { name: String, path: String },
    Window {
        #[serde(rename = "appName", alias = "app_name")]
        app_name: String,
        title: String,
        #[serde(default)]
        url: Option<String>,
        /// Inbox id of a PNG of the window.
        #[serde(default)]
        screenshot: Option<String>,
    },
}

/// The parts the first message carries for its context.
pub fn context_parts(context: &ChatContext) -> Vec<Part> {
    match context {
        ChatContext::File { name, path } => {
            let mut parts = Vec::new();
            if let Some(part) = file_part(name, path) {
                parts.push(part);
            }
            parts.push(Part::Text(format!("File: {name} ({path})")));
            parts
        }
        ChatContext::Window { app_name, title, url, screenshot } => {
            let mut parts = Vec::new();
            let shot = screenshot.as_deref().and_then(crate::files::inbox_file);
            if let Some(part) = shot.and_then(|p| file_part("window.png", &p.to_string_lossy())) {
                parts.push(part);
            }
            let mut text = format!("Context — App: {app_name}, Window: {title}");
            if let Some(url) = url {
                text.push_str(&format!(", URL: {url}"));
            }
            if parts.is_empty() {
                text.push_str(" (no screenshot)");
            } else {
                text.push_str(" (screenshot attached)");
            }
            parts.push(Part::Text(text));
            parts
        }
    }
}

/// PDF → document, image → image, text/code → inline text.
fn file_part(name: &str, path: &str) -> Option<Part> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media = match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    if let Some(media) = media {
        let bytes = std::fs::read(path).ok()?;
        return Some(Part::Image { media: media.into(), data: base64(&bytes) });
    }
    if ext == "pdf" {
        let bytes = std::fs::read(path).ok()?;
        return Some(Part::Pdf { name: name.into(), data: base64(&bytes) });
    }

    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_INLINE_TEXT {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(Part::Text(format!("File contents:\n{text}")))
}

/// Small standalone base64 encoder — not worth another dependency.
/// Also used for Stripe's basic auth.
pub(crate) fn base64_for(bytes: &[u8]) -> String {
    base64(bytes)
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64, context_parts, ChatContext};
    use crate::providers::Part;

    #[test]
    fn window_context_reads_the_island_shape() {
        let c: ChatContext = serde_json::from_value(serde_json::json!({
            "kind": "window", "appName": "chrome", "title": "Inbox", "screenshot": "nope/../x.png"
        }))
        .unwrap();
        let parts = context_parts(&c);
        assert_eq!(parts.len(), 1, "an id outside the inbox is not attached");
        match &parts[0] {
            Part::Text(t) => assert!(t.contains("App: chrome, Window: Inbox") && t.contains("no screenshot"), "{t}"),
            _ => panic!("text expected"),
        }
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
