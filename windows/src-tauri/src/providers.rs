// AI providers for the assistant — one neutral conversation, several APIs.
//
// Anthropic speaks the Messages API. xAI (Grok), OpenAI, Google (Gemini through
// its OpenAI-compatible endpoint), Ollama and LM Studio all speak Chat
// Completions. Every reply is streamed: text deltas reach `on_text` as they
// arrive, tool calls come back whole at the end of the turn.
//
// Keys are read from the Credential Manager here and never cross the IPC
// boundary. Local servers get no key and only talk to the URL the user typed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::secrets;

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Server-side fallback: on a policy decline the API retries on a fallback
/// model inside the same call, so the island never shows a dead end.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 8192;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Grok and the other models of the owner's Cursor plan, through cursor.rs.
    #[default]
    Cursor,
    Anthropic,
    Xai,
    Openai,
    Google,
    Ollama,
    Lmstudio,
}

pub struct Info {
    pub name: &'static str,
    /// Credential Manager key; None for local servers.
    pub key: Option<&'static str>,
    pub base: &'static str,
    pub model: &'static str,
    /// Local servers take a user-supplied base URL.
    pub local: bool,
}

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Provider::Cursor => "cursor",
            Provider::Anthropic => "anthropic",
            Provider::Xai => "xai",
            Provider::Openai => "openai",
            Provider::Google => "google",
            Provider::Ollama => "ollama",
            Provider::Lmstudio => "lmstudio",
        }
    }

    pub fn info(self) -> Info {
        match self {
            Provider::Cursor => Info {
                name: "Cursor (Grok)",
                key: Some("cursor-api-key"),
                base: "https://api.cursor.com",
                model: crate::cursor::AUTO_MODEL,
                local: false,
            },
            Provider::Anthropic => Info {
                name: "Anthropic",
                key: Some("anthropic-api-key"),
                base: "https://api.anthropic.com/v1",
                model: crate::claude::DEFAULT_MODEL,
                local: false,
            },
            Provider::Xai => Info {
                name: "xAI Grok",
                key: Some("xai-api-key"),
                base: "https://api.x.ai/v1",
                model: "grok-4",
                local: false,
            },
            Provider::Openai => Info {
                name: "OpenAI",
                key: Some("openai-api-key"),
                base: "https://api.openai.com/v1",
                model: "gpt-5",
                local: false,
            },
            Provider::Google => Info {
                name: "Google AI",
                key: Some("google-api-key"),
                base: "https://generativelanguage.googleapis.com/v1beta/openai",
                model: "gemini-2.5-pro",
                local: false,
            },
            Provider::Ollama => Info {
                name: "Ollama",
                key: None,
                base: "http://localhost:11434/v1",
                model: "llama3.2",
                local: true,
            },
            Provider::Lmstudio => Info {
                name: "LM Studio",
                key: None,
                base: "http://localhost:1234/v1",
                model: "local-model",
                local: true,
            },
        }
    }
}

/// Everything one call needs: where, which model, with which key.
#[derive(Clone)]
pub struct Endpoint {
    pub provider: Provider,
    pub base: String,
    pub model: String,
    key: Option<String>,
}

impl Endpoint {
    pub fn new(settings: &crate::settings::Settings, provider: Provider) -> Result<Self, String> {
        let info = provider.info();
        let key = match info.key {
            Some(name) => Some(
                secrets::get(name)
                    .ok_or_else(|| format!("Falta la clave de {}. Ponla en Ajustes → Asistente.", info.name))?,
            ),
            None => None,
        };
        let base = settings.base_for(provider);
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(format!("{}: la URL del servidor debe empezar por http:// o https://", info.name));
        }
        Ok(Self { provider, base, model: settings.model_for(provider), key })
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

// ── Neutral conversation ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Part {
    Text(String),
    Image { media: String, data: String },
    Pdf { name: String, data: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Clone, Debug)]
pub enum Msg {
    User(Vec<Part>),
    Assistant { text: String, calls: Vec<ToolCall> },
    Tool { id: String, output: String, is_error: bool },
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

pub struct Request<'a> {
    pub system: &'a str,
    pub messages: &'a [Msg],
    pub tools: &'a [ToolSpec],
    /// Anthropic's server-side web search.
    pub web_search: bool,
}

#[derive(Debug, Default)]
pub struct Turn {
    pub text: String,
    pub calls: Vec<ToolCall>,
}

pub async fn complete(
    ep: &Endpoint,
    req: Request<'_>,
    on_text: &mut (dyn FnMut(&str) + Send),
    cancel: &AtomicBool,
) -> Result<Turn, String> {
    match ep.provider {
        Provider::Anthropic => anthropic(ep, &req, on_text, cancel).await,
        Provider::Cursor => Err("Cursor runs its own loop (cursor.rs).".into()),
        _ => {
            let result = openai(ep, &req, req.tools, on_text, cancel).await;
            // Plenty of local models refuse a request that carries tools. Ask
            // again without them rather than leaving the chat dead.
            match result {
                Err(err) if !req.tools.is_empty() && err.contains("tool") => {
                    openai(ep, &req, &[], on_text, cancel).await
                }
                other => other,
            }
        }
    }
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())
}

/// The API's own error message, which is what makes a bad key obvious.
fn api_error(name: &str, status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let err = v.get("error")?;
            err.get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| err.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    format!("{name} {status}: {detail}")
}

// ── Server-sent events ────────────────────────────────────────────────────────

#[derive(Default)]
struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    /// Feeds a chunk and returns the complete `data:` payloads it closed.
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(data) = line.strip_prefix("data:") {
                out.push(data.trim_start().to_string());
            }
        }
        out
    }
}

async fn stream(
    mut response: reqwest::Response,
    cancel: &AtomicBool,
    mut on_data: impl FnMut(&str) -> Result<bool, String>,
) -> Result<(), String> {
    let mut sse = Sse::default();
    while let Some(chunk) = response.chunk().await.map_err(|e| format!("Error de red: {e}"))? {
        if cancel.load(Ordering::Relaxed) {
            return Err("Stopped.".into());
        }
        for data in sse.push(&chunk) {
            if !on_data(&data)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

// ── Anthropic Messages API ────────────────────────────────────────────────────

fn anthropic_part(part: &Part) -> Value {
    match part {
        Part::Text(t) => json!({ "type": "text", "text": t }),
        Part::Image { media, data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media, "data": data },
        }),
        Part::Pdf { data, .. } => json!({
            "type": "document",
            "source": { "type": "base64", "media_type": "application/pdf", "data": data },
        }),
    }
}

/// Consecutive messages of one role are merged: tool results ride in a single
/// user turn, as the API requires.
fn push_role(out: &mut Vec<Value>, role: &str, content: Vec<Value>) {
    if let Some(last) = out.last_mut() {
        if last["role"] == role {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.extend(content);
                return;
            }
        }
    }
    out.push(json!({ "role": role, "content": content }));
}

fn anthropic_messages(msgs: &[Msg]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in msgs {
        match m {
            Msg::User(parts) => push_role(&mut out, "user", parts.iter().map(anthropic_part).collect()),
            Msg::Assistant { text, calls } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
                for c in calls {
                    content.push(json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.input }));
                }
                if content.is_empty() {
                    content.push(json!({ "type": "text", "text": "…" }));
                }
                push_role(&mut out, "assistant", content);
            }
            Msg::Tool { id, output, is_error } => push_role(
                &mut out,
                "user",
                vec![json!({ "type": "tool_result", "tool_use_id": id, "content": output, "is_error": is_error })],
            ),
        }
    }
    out
}

#[derive(Default)]
struct Block {
    kind: String,
    text: String,
    id: String,
    name: String,
    json: String,
}

async fn anthropic(
    ep: &Endpoint,
    req: &Request<'_>,
    on_text: &mut (dyn FnMut(&str) + Send),
    cancel: &AtomicBool,
) -> Result<Turn, String> {
    let mut tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema }))
        .collect();
    if req.web_search {
        tools.push(json!({ "type": "web_search_20260209", "name": "web_search", "max_uses": 5 }));
    }
    let mut body = json!({
        "model": ep.model,
        "max_tokens": MAX_TOKENS,
        "system": req.system,
        "messages": anthropic_messages(req.messages),
        "stream": true,
        "fallbacks": "default",
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }

    let response = client()?
        .post(format!("{}/messages", ep.base))
        .header("x-api-key", ep.key.as_deref().unwrap_or_default())
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Error de red: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(api_error("Claude API", status, &text));
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut refusal: Option<String> = None;
    let mut wrote_text = false;
    stream(response, cancel, |data| {
        let Ok(ev) = serde_json::from_str::<Value>(data) else { return Ok(true) };
        match ev["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                let cb = &ev["content_block"];
                let kind = cb["type"].as_str().unwrap_or_default().to_string();
                let mut block = Block { kind: kind.clone(), ..Default::default() };
                if kind == "text" {
                    if wrote_text {
                        on_text("\n\n");
                    }
                    block.text = cb["text"].as_str().unwrap_or_default().to_string();
                    if !block.text.is_empty() {
                        on_text(&block.text);
                        wrote_text = true;
                    }
                } else if kind == "tool_use" {
                    block.id = cb["id"].as_str().unwrap_or_default().to_string();
                    block.name = cb["name"].as_str().unwrap_or_default().to_string();
                }
                blocks.push(block);
            }
            "content_block_delta" => {
                let delta = &ev["delta"];
                let Some(block) = blocks.last_mut() else { return Ok(true) };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = delta["text"].as_str().unwrap_or_default();
                        block.text.push_str(t);
                        if !t.is_empty() {
                            on_text(t);
                            wrote_text = true;
                        }
                    }
                    "input_json_delta" => block.json.push_str(delta["partial_json"].as_str().unwrap_or_default()),
                    _ => {}
                }
            }
            "message_delta" => {
                if ev["delta"]["stop_reason"].as_str() == Some("refusal") {
                    let why = ev["delta"]["stop_details"]["explanation"]
                        .as_str()
                        .unwrap_or("Claude declined this one.");
                    refusal = Some(why.to_string());
                }
            }
            "message_stop" => return Ok(false),
            "error" => {
                let msg = ev["error"]["message"].as_str().unwrap_or("stream error");
                return Err(format!("Claude API: {msg}"));
            }
            _ => {}
        }
        Ok(true)
    })
    .await?;

    if let Some(why) = refusal {
        return Err(why);
    }
    let mut turn = Turn::default();
    let texts: Vec<&str> = blocks.iter().filter(|b| b.kind == "text").map(|b| b.text.as_str()).collect();
    turn.text = texts.join("\n\n").trim().to_string();
    for b in blocks.into_iter().filter(|b| b.kind == "tool_use") {
        let input = if b.json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&b.json).unwrap_or_else(|_| json!({}))
        };
        turn.calls.push(ToolCall { id: b.id, name: b.name, input });
    }
    Ok(turn)
}

// ── Chat Completions (xAI, OpenAI, Google, Ollama, LM Studio) ─────────────────

fn openai_user(provider: Provider, parts: &[Part]) -> Value {
    if parts.iter().all(|p| matches!(p, Part::Text(_))) {
        let text = parts
            .iter()
            .filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None })
            .collect::<Vec<_>>()
            .join("\n\n");
        return json!({ "role": "user", "content": text });
    }
    let content: Vec<Value> = parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => json!({ "type": "text", "text": t }),
            Part::Image { media, data } => json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{media};base64,{data}") },
            }),
            Part::Pdf { name, data } if provider == Provider::Openai => json!({
                "type": "file",
                "file": { "filename": name, "file_data": format!("data:application/pdf;base64,{data}") },
            }),
            Part::Pdf { name, .. } => json!({
                "type": "text",
                "text": format!("(The PDF {name} was attached, but this provider cannot read PDFs.)"),
            }),
        })
        .collect();
    json!({ "role": "user", "content": content })
}

fn openai_messages(provider: Provider, system: &str, msgs: &[Msg]) -> Vec<Value> {
    let mut out = vec![json!({ "role": "system", "content": system })];
    for m in msgs {
        match m {
            Msg::User(parts) => out.push(openai_user(provider, parts)),
            Msg::Assistant { text, calls } => {
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                msg.insert("content".into(), if text.is_empty() { Value::Null } else { json!(text) });
                if !calls.is_empty() {
                    let calls: Vec<Value> = calls
                        .iter()
                        .map(|c| json!({
                            "id": c.id,
                            "type": "function",
                            "function": { "name": c.name, "arguments": c.input.to_string() },
                        }))
                        .collect();
                    msg.insert("tool_calls".into(), Value::Array(calls));
                }
                out.push(Value::Object(msg));
            }
            Msg::Tool { id, output, .. } => {
                out.push(json!({ "role": "tool", "tool_call_id": id, "content": output }));
            }
        }
    }
    out
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

async fn openai(
    ep: &Endpoint,
    req: &Request<'_>,
    tools: &[ToolSpec],
    on_text: &mut (dyn FnMut(&str) + Send),
    cancel: &AtomicBool,
) -> Result<Turn, String> {
    let info = ep.provider.info();
    let mut body = json!({
        "model": ep.model,
        "messages": openai_messages(ep.provider, req.system, req.messages),
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|t| json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.schema },
                }))
                .collect(),
        );
    }

    let mut request = client()?
        .post(format!("{}/chat/completions", ep.base))
        .header("content-type", "application/json")
        .json(&body);
    if let Some(key) = &ep.key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|e| {
        if info.local {
            format!("{} is not answering at {}. Is it running? ({e})", info.name, ep.base)
        } else {
            format!("Error de red: {e}")
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(api_error(info.name, status, &text));
    }

    let mut text = String::new();
    let mut calls: Vec<PartialCall> = Vec::new();
    stream(response, cancel, |data| {
        if data == "[DONE]" {
            return Ok(false);
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else { return Ok(true) };
        if let Some(err) = ev.get("error") {
            let msg = err["message"].as_str().map(str::to_string).unwrap_or_else(|| err.to_string());
            return Err(format!("{}: {msg}", info.name));
        }
        let delta = &ev["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str() {
            if !t.is_empty() {
                text.push_str(t);
                on_text(t);
            }
        }
        if let Some(list) = delta["tool_calls"].as_array() {
            for tc in list {
                let id = tc["id"].as_str().unwrap_or_default();
                // Some servers omit `index`; a new id then means a new call.
                let index = match tc["index"].as_u64() {
                    Some(i) => i as usize,
                    None if !id.is_empty() && !calls.iter().any(|c| c.id == id) => calls.len(),
                    None => calls.len().saturating_sub(1),
                };
                while calls.len() <= index {
                    calls.push(PartialCall::default());
                }
                let call = &mut calls[index];
                if !id.is_empty() {
                    call.id = id.to_string();
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    call.name.push_str(n);
                }
                match &tc["function"]["arguments"] {
                    Value::String(a) => call.args.push_str(a),
                    Value::Object(_) => call.args = tc["function"]["arguments"].to_string(),
                    _ => {}
                }
            }
        }
        Ok(true)
    })
    .await?;

    let calls = calls
        .into_iter()
        .filter(|c| !c.name.is_empty())
        .enumerate()
        .map(|(i, c)| ToolCall {
            id: if c.id.is_empty() { format!("call_{i}") } else { c.id },
            name: c.name,
            input: serde_json::from_str(&c.args).unwrap_or_else(|_| json!({})),
        })
        .collect();
    Ok(Turn { text: text.trim().to_string(), calls })
}

// ── Model list ────────────────────────────────────────────────────────────────

/// The models the provider offers, for the picker in settings.
pub async fn list_models(ep: &Endpoint) -> Result<Vec<String>, String> {
    if ep.provider == Provider::Cursor {
        return crate::cursor::list_models(ep).await;
    }
    let mut request = client()?.get(format!("{}/models", ep.base));
    request = match (ep.provider, &ep.key) {
        (Provider::Anthropic, Some(key)) => request
            .header("x-api-key", key)
            .header("anthropic-version", ANTHROPIC_VERSION),
        (_, Some(key)) => request.bearer_auth(key),
        (_, None) => request,
    };
    let response = request.send().await.map_err(|e| format!("Error de red: {e}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(api_error(ep.provider.info().name, status, &text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = value["data"]
        .as_array()
        .or_else(|| value["models"].as_array())
        .map(|list| {
            list.iter()
                .filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()))
                .map(|id| id.trim_start_matches("models/").to_string())
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_splits_data_lines_across_chunks() {
        let mut sse = Sse::default();
        assert!(sse.push(b"event: x\ndata: {\"a\"").is_empty());
        assert_eq!(sse.push(b":1}\r\n\r\ndata: [DONE]\n"), vec!["{\"a\":1}", "[DONE]"]);
    }

    #[test]
    fn anthropic_merges_tool_results_into_one_user_turn() {
        let msgs = vec![
            Msg::User(vec![Part::Text("hi".into())]),
            Msg::Assistant {
                text: String::new(),
                calls: vec![
                    ToolCall { id: "a".into(), name: "read_file".into(), input: json!({}) },
                    ToolCall { id: "b".into(), name: "list_dir".into(), input: json!({}) },
                ],
            },
            Msg::Tool { id: "a".into(), output: "x".into(), is_error: false },
            Msg::Tool { id: "b".into(), output: "y".into(), is_error: true },
        ];
        let out = anthropic_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
    }

    #[test]
    fn openai_messages_carry_tool_calls_and_results() {
        let msgs = vec![
            Msg::User(vec![Part::Text("hi".into())]),
            Msg::Assistant {
                text: "ok".into(),
                calls: vec![ToolCall { id: "c1".into(), name: "open_url".into(), input: json!({"url": "https://x"}) }],
            },
            Msg::Tool { id: "c1".into(), output: "done".into(), is_error: false },
        ];
        let out = openai_messages(Provider::Xai, "sys", &msgs);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["content"], "hi");
        assert_eq!(out[2]["tool_calls"][0]["function"]["name"], "open_url");
        assert_eq!(out[2]["tool_calls"][0]["function"]["arguments"], "{\"url\":\"https://x\"}");
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "c1");
    }

    #[test]
    fn images_become_data_urls_for_chat_completions() {
        let v = openai_user(
            Provider::Google,
            &[Part::Text("look".into()), Part::Image { media: "image/png".into(), data: "AAA".into() }],
        );
        assert_eq!(v["content"][1]["image_url"]["url"], "data:image/png;base64,AAA");
    }

    /// Needs a running Ollama: `COUCOU_TEST_OLLAMA_MODEL=qwen3.5:9b cargo test ollama -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn ollama_streams_text_and_calls_tools() {
        let model = std::env::var("COUCOU_TEST_OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".into());
        let ep = Endpoint {
            provider: Provider::Ollama,
            base: Provider::Ollama.info().base.into(),
            model,
            key: None,
        };
        let tools = vec![ToolSpec {
            name: "system_info".into(),
            description: "Returns the computer's OS, CPU and memory.".into(),
            schema: json!({"type": "object", "properties": {}}),
        }];
        let messages = vec![Msg::User(vec![Part::Text(
            "Call the system_info tool now. Do not answer in text.".into(),
        )])];
        let req = Request { system: "You are a test assistant.", messages: &messages, tools: &tools, web_search: false };
        let cancel = AtomicBool::new(false);
        let mut streamed = String::new();
        let turn = complete(&ep, req, &mut |t| streamed.push_str(t), &cancel).await.unwrap();
        assert_eq!(turn.calls.first().map(|c| c.name.as_str()), Some("system_info"), "text: {}", turn.text);

        let messages = vec![
            messages[0].clone(),
            Msg::Assistant { text: turn.text, calls: turn.calls.clone() },
            Msg::Tool { id: turn.calls[0].id.clone(), output: "Windows 11, 16 GB RAM".into(), is_error: false },
        ];
        let req = Request { system: "You are a test assistant.", messages: &messages, tools: &tools, web_search: false };
        let mut streamed = String::new();
        let turn = complete(&ep, req, &mut |t| streamed.push_str(t), &cancel).await.unwrap();
        assert!(turn.calls.is_empty() || !turn.text.is_empty());
        assert_eq!(streamed, turn.text);
        assert!(!turn.text.trim().is_empty());
    }
}
