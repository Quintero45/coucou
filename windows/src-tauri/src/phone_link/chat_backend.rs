// The real chat behind the phone link (chat.rs holds the rules): ARIA's own
// provider code (providers.rs), with the phone's own conversation and no tools —
// anything with side effects stays on the computer, behind a click. Keys are read
// from the Credential Manager by `providers::Endpoint`; nothing here holds one.
//
// Failures are sorted into a few kinds here and only the kind goes on: a provider's message may
// contain part of a key, so it is neither forwarded nor logged.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::chat::{BoxFuture, ChatBackend, ChatConfig, ChatFail, ModelOption};
use crate::providers::{self, Endpoint, Msg, Part, Provider, Request};
use crate::settings::Settings;
use crate::{log, secrets, Shared};

/// Most models the user can allow, and the longest model name taken.
pub const MAX_ALLOWED: usize = 50;
const MAX_MODEL_CHARS: usize = 200;
/// The phone's conversation keeps this many messages.
const KEPT: usize = 40;

const PROVIDERS: [&str; 6] = ["anthropic", "xai", "openai", "google", "ollama", "lmstudio"];

/// ARIA herself: the message goes to the island's chat, with her provider (Cursor included),
/// her tools and her approval cards, and her answer comes back. Offered whenever chat is on.
pub const ARIA_MODEL: &str = "aria/aria";
/// Tools and approval cards can take a while; past this the phone is told it failed.
const ARIA_TIMEOUT: Duration = Duration::from_secs(10 * 60);

type Waiters = Mutex<HashMap<u64, oneshot::Sender<String>>>;

fn waiters() -> &'static Waiters {
    static W: OnceLock<Waiters> = OnceLock::new();
    W.get_or_init(Waiters::default)
}

/// The island's answer to a message from the phone ("" when it failed).
#[tauri::command]
pub fn phone_chat_reply(id: u64, text: String) {
    if let Some(tx) = waiters().lock().unwrap().remove(&id) {
        let _ = tx.send(text);
    }
}

/// Removes the waiter when the turn ends, answered or not (the phone left, a timeout).
struct Waiting(u64);

impl Drop for Waiting {
    fn drop(&mut self) {
        waiters().lock().unwrap().remove(&self.0);
    }
}

async fn ask_aria(app: &AppHandle, text: String) -> Result<String, ChatFail> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    waiters().lock().unwrap().insert(id, tx);
    let _guard = Waiting(id);
    log::line("phone link: message for ARIA sent to the island chat");
    app.emit("phone-ask", serde_json::json!({ "id": id, "text": text })).map_err(|_| ChatFail::Internal)?;
    match tokio::time::timeout(ARIA_TIMEOUT, rx).await {
        Ok(Ok(answer)) if !answer.trim().is_empty() => Ok(answer),
        _ => Err(ChatFail::Provider),
    }
}

fn aria_option() -> ModelOption {
    ModelOption { id: ARIA_MODEL.into(), provider: "aria".into(), label: "ARIA".into() }
}

const SYSTEM: &str = "You are ARIA, the owner's assistant, answering from their phone. \
Answer in the language they write in, briefly. You have no tools here: if they ask for \
something that needs one, tell them to ask you on the computer.";

pub struct AppChat {
    app: AppHandle,
    history: Arc<Mutex<Vec<Msg>>>,
}

impl AppChat {
    pub fn new(app: AppHandle) -> Self {
        Self { app, history: Arc::default() }
    }

    fn settings(&self) -> Settings {
        self.app.state::<Shared>().settings.lock().unwrap().clone()
    }
}

fn provider_of(id: &str) -> Option<Provider> {
    Some(match id {
        "anthropic" => Provider::Anthropic,
        "xai" => Provider::Xai,
        "openai" => Provider::Openai,
        "google" => Provider::Google,
        "ollama" => Provider::Ollama,
        "lmstudio" => Provider::Lmstudio,
        _ => return None,
    })
}

impl ChatBackend for AppChat {
    fn config(&self) -> ChatConfig {
        let s = self.settings();
        let mut allowed = vec![aria_option()];
        allowed.extend(options(&s.phone_chat_models));
        ChatConfig { enabled: s.phone_chat, allowed }
    }

    fn send(&self, model_id: &str, text: String, deltas: UnboundedSender<String>) -> BoxFuture<Result<String, ChatFail>> {
        if model_id == ARIA_MODEL {
            let app = self.app.clone();
            return Box::pin(async move { ask_aria(&app, text).await });
        }
        let settings = self.settings();
        let history = self.history.clone();
        let id = model_id.to_string();
        Box::pin(async move {
            let Some((provider_id, model)) = parse_model_id(&id) else { return Err(ChatFail::Internal) };
            let Some(provider) = provider_of(provider_id) else { return Err(ChatFail::Internal) };
            if let Some(key) = key_name(provider_id) {
                if secrets::get(key).is_none() {
                    return Err(ChatFail::NoKey);
                }
            }
            let mut ep = Endpoint::new(&settings, provider).map_err(|_| ChatFail::NoKey)?;
            ep.model = model.to_string();
            let mut messages = history.lock().unwrap().clone();
            messages.push(Msg::User(vec![Part::Text(text.clone())]));
            let mut so_far = String::new();
            let mut on_text = |piece: &str| {
                so_far.push_str(piece);
                let _ = deltas.send(so_far.clone());
            };
            let cancel = AtomicBool::new(false);
            let request = Request { system: SYSTEM, messages: &messages, tools: &[], web_search: false };
            match providers::complete(&ep, request, &mut on_text, &cancel).await {
                Ok(turn) => {
                    let mut h = history.lock().unwrap();
                    h.push(Msg::User(vec![Part::Text(text)]));
                    h.push(Msg::Assistant { text: turn.text.clone(), calls: Vec::new() });
                    let excess = h.len().saturating_sub(KEPT);
                    h.drain(..excess);
                    Ok(turn.text)
                }
                Err(err) => {
                    let kind = classify(is_local(provider_id), &err);
                    // The kind only: the text may echo a key.
                    log::line(format!("phone link: chat via {provider_id} failed ({kind:?})"));
                    Err(kind)
                }
            }
        })
    }

    fn reset(&self) {
        self.history.lock().unwrap().clear();
    }
}

// ── Pure helpers (tested below) ───────────────────────────────────────────────

fn is_local(provider: &str) -> bool {
    matches!(provider, "ollama" | "lmstudio")
}

/// The credential store entry a provider's key lives in; None for the model servers.
fn key_name(provider: &str) -> Option<&'static str> {
    provider_of(provider).and_then(|p| p.info().key)
}

pub fn provider_name(provider: &str) -> String {
    provider_of(provider).map_or_else(|| "Custom server".into(), |p| p.info().name.to_string())
}

/// "provider/model" with a provider this build knows and a plain model name; the model may contain
/// slashes ("meta/llama" is the model of the "ollama" provider written "ollama/meta/llama").
pub fn parse_model_id(id: &str) -> Option<(&str, &str)> {
    let (provider, model) = id.split_once('/')?;
    let model = model.trim();
    if !PROVIDERS.contains(&provider) || model.is_empty() || model.chars().count() > MAX_MODEL_CHARS || model.chars().any(char::is_control) {
        return None;
    }
    Some((provider, model))
}

/// The user's list, made safe: valid ids only, no duplicates, at most MAX_ALLOWED.
pub fn clean_models(ids: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if parse_model_id(id).is_some() && !out.contains(id) && out.len() < MAX_ALLOWED {
            out.push(id.clone());
        }
    }
    out
}

pub fn options(ids: &[String]) -> Vec<ModelOption> {
    clean_models(ids)
        .into_iter()
        .filter_map(|id| {
            let (provider, model) = parse_model_id(&id)?;
            let label = format!("{} · {}", provider_name(provider), model);
            Some(ModelOption { provider: provider.to_string(), label, id })
        })
        .collect()
}

/// Which kind of failure an error message from providers.rs stands for. Only what does not change
/// with the wording counts: an HTTP status in the text. A model server that answers with no status
/// at all is one that did not answer.
pub fn classify(local: bool, err: &str) -> ChatFail {
    let has = |code: &str| err.split(|c: char| !c.is_ascii_digit()).any(|w| w == code);
    if has("401") || has("403") {
        return ChatFail::Auth;
    }
    let has_status = err.split(|c: char| !c.is_ascii_digit()).any(|w| w.len() == 3 && w.starts_with(['4', '5']));
    if local && !has_status {
        return ChatFail::Unreachable;
    }
    if !has_status && (err.contains("error sending request") || err.contains("connect")) {
        return ChatFail::Unreachable;
    }
    ChatFail::Provider
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn model_ids_need_a_known_provider_and_a_plain_model() {
        assert_eq!(parse_model_id("openai/gpt-4o"), Some(("openai", "gpt-4o")));
        assert_eq!(parse_model_id("xai/grok-4"), Some(("xai", "grok-4")));
        assert_eq!(parse_model_id("ollama/llama3.2:latest"), Some(("ollama", "llama3.2:latest")));
        for bad in ["", "openai", "openai/", "openai/ ", "nope/gpt", "OpenAI/gpt", "cursor/auto", "openai/a\nb", &format!("openai/{}", "x".repeat(201))] {
            assert_eq!(parse_model_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_allowed_list_is_cleaned_and_capped() {
        assert_eq!(clean_models(&ids(&["openai/gpt-4o", "bad", "openai/gpt-4o", "google/gemini"])), ids(&["openai/gpt-4o", "google/gemini"]));
        let many: Vec<String> = (0..80).map(|i| format!("openai/m{i}")).collect();
        assert_eq!(clean_models(&many).len(), MAX_ALLOWED);
        assert!(clean_models(&[]).is_empty());
    }

    #[test]
    fn what_the_phone_is_told_about_a_model_is_its_id_provider_and_label_only() {
        let o = options(&ids(&["anthropic/claude-x", "lmstudio/qwen"]));
        assert_eq!(o[0], ModelOption { id: "anthropic/claude-x".into(), provider: "anthropic".into(), label: "Anthropic · claude-x".into() });
        assert_eq!(o[1].label, "LM Studio · qwen");
    }

    #[test]
    fn failures_are_sorted_by_status_and_never_by_wording() {
        assert_eq!(classify(false, "Anthropic 401 Unauthorized: invalid x-api-key"), ChatFail::Auth);
        assert_eq!(classify(false, "OpenAI 403 Forbidden: nope"), ChatFail::Auth);
        assert_eq!(classify(false, "OpenAI 429 Too Many Requests: slow down"), ChatFail::Provider);
        assert_eq!(classify(false, "error sending request for url"), ChatFail::Unreachable);
        assert_eq!(classify(true, "Cannot reach http://127.0.0.1:11434"), ChatFail::Unreachable);
        assert_eq!(classify(true, "Ollama 500: model crashed"), ChatFail::Provider);
        assert_eq!(classify(false, "something 14012 happened"), ChatFail::Provider);
    }

    #[test]
    fn aria_is_offered_by_name_and_never_stored_as_a_provider_model() {
        assert_eq!(aria_option(), ModelOption { id: ARIA_MODEL.into(), provider: "aria".into(), label: "ARIA".into() });
        assert_eq!(parse_model_id(ARIA_MODEL), None);
        assert!(clean_models(&ids(&[ARIA_MODEL])).is_empty());
    }

    #[tokio::test]
    async fn a_reply_reaches_only_the_message_it_answers() {
        let (tx, rx) = oneshot::channel();
        waiters().lock().unwrap().insert(9_001, tx);
        phone_chat_reply(9_002, "wrong".into());
        phone_chat_reply(9_001, "hola".into());
        assert_eq!(rx.await.unwrap(), "hola");
        assert!(!waiters().lock().unwrap().contains_key(&9_001));
    }

    #[test]
    fn keys_are_found_by_provider() {
        assert_eq!(key_name("anthropic"), Some("anthropic-api-key"));
        assert_eq!(key_name("xai"), Some("xai-api-key"));
        assert_eq!(key_name("ollama"), None);
        assert!(is_local("lmstudio") && !is_local("openai"));
    }
}
