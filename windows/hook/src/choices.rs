//! Questions with buttons, shared by the Grok Bots (`--bot … --options`) and
//! Cursor's AskQuestion: the options the island shows, and the island's reply
//! as it comes back on the pipe.

use serde_json::{json, Value};

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_answer_only_counts_with_allow() {
        let r = parse_reply(r#"{"decision":"allow","answer":"B"}"#).unwrap();
        assert_eq!(r.answer(), Some("B"));
        assert_eq!(parse_reply("allow").unwrap().answer(), None);
        assert_eq!(parse_reply(r#"{"decision":"deny","answer":"B"}"#).unwrap().answer(), None);
    }

    #[test]
    fn replies_parse_words_and_json() {
        assert_eq!(parse_reply("allow\n").unwrap().decision, "allow");
        let r = parse_reply(r#"{"decision":"answer","answers":{"Q":"A"}}"#).unwrap();
        assert_eq!(r.decision, "answer");
        assert_eq!(r.answers["Q"], "A");
        assert!(parse_reply("  ").is_none());
    }
}
