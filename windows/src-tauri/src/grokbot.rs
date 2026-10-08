// The owner's Grok Bots (Cursor's Grok Bot app).
//
// Grok Bot has no chat API. Two documented doors make it work with the island:
//   * ARIA → Bot: each Bot gets a routine with a webhook trigger. A POST with
//     the routine's Bearer key starts a run with our JSON body; the result shows
//     in the Bot's own chat (a 200 only means "started").
//   * Bot → ARIA: Grok Bot can run commands on this computer (its local
//     execution). The Bot runs `aria-hook --bot "<name>" --status … "<text>"`,
//     which lands in the island as that Bot's pill (hook/src/bot.rs).
//
// The webhook key lives in the Credential Manager (`grokbot-key:<id>`); only the
// name, colour and URL are in settings.json. Sending a task spends the owner's
// Grok Bot usage, so the assistant's send_to_grok_bot asks first.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::policy;
use crate::providers::{ToolCall, ToolSpec};
use crate::tools::Outcome;
use crate::{log, secrets, settings};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GrokBot {
    /// slug(name): the pill is `agent_bot-<id>`, the key `grokbot-key:<id>`.
    pub id: String,
    pub name: String,
    pub color: String,
    /// The routine's webhook "POST to" URL.
    pub url: String,
}

const PALETTE: &[&str] = &["#38BDF8", "#F472B6", "#34D399", "#FBBF24", "#A78BFA", "#FB7185", "#2DD4BF", "#F97316"];

/// Same rule as hook/src/bot.rs — the two must agree on every name.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.trim_end_matches('-').chars().take(20).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn key_name(id: &str) -> String {
    format!("{}{id}", secrets::GROKBOT_PREFIX)
}

fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())
}

pub fn list(app: &AppHandle) -> Vec<GrokBot> {
    app.state::<crate::Shared>().settings.lock().unwrap().grok_bots.clone()
}

fn find(app: &AppHandle, who: &str) -> Option<GrokBot> {
    let who = who.trim();
    let id = slug(who);
    list(app).into_iter().find(|b| b.id == who || b.id == id || b.name.eq_ignore_ascii_case(who))
}

/// Writes the bot list through the same path as the settings window.
fn store(app: &AppHandle, bots: Vec<GrokBot>) -> Result<(), String> {
    let snapshot = {
        let shared = app.state::<crate::Shared>();
        let mut current = shared.settings.lock().unwrap();
        current.grok_bots = bots;
        current.clone()
    };
    settings::save(&snapshot).map_err(|e| e.to_string())?;
    let _ = app.emit("settings-changed", snapshot);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotStatus {
    #[serde(flatten)]
    pub bot: GrokBot,
    pub has_key: bool,
}

pub fn statuses(app: &AppHandle) -> Vec<BotStatus> {
    list(app)
        .into_iter()
        .map(|bot| BotStatus { has_key: secrets::present(&key_name(&bot.id)), bot })
        .collect()
}

/// Adds or updates a Bot. `previous` is its old id when it was renamed; an
/// empty key keeps the stored one.
pub fn save(app: &AppHandle, previous: Option<&str>, name: &str, color: &str, url: &str, key: &str) -> Result<GrokBot, String> {
    let name = name.trim();
    let id = slug(name);
    if id.is_empty() {
        return Err("Ponle un nombre al Bot (letras o números).".into());
    }
    let url = url.trim();
    if !url.starts_with("https://") {
        return Err("La URL del webhook debe empezar por https://".into());
    }
    let mut bots = list(app);
    let previous = previous.filter(|p| !p.is_empty()).unwrap_or(&id).to_string();
    if previous != id && bots.iter().any(|b| b.id == id) {
        return Err(format!("Ya tienes un Bot llamado así ({id})."));
    }
    let color = if valid_color(color) {
        color.to_uppercase()
    } else {
        PALETTE[bots.len() % PALETTE.len()].to_string()
    };
    let bot = GrokBot { id: id.clone(), name: name.chars().take(40).collect(), color, url: url.to_string() };

    let key = key.trim();
    if !key.is_empty() {
        secrets::set(&key_name(&id), key)?;
    } else if previous != id {
        if let Some(old) = secrets::get(&key_name(&previous)) {
            secrets::set(&key_name(&id), &old)?;
        }
    }
    if previous != id {
        let _ = secrets::clear(&key_name(&previous));
    }
    if !secrets::present(&key_name(&id)) {
        return Err("Falta la clave (key) del webhook de la rutina.".into());
    }

    match bots.iter_mut().find(|b| b.id == previous) {
        Some(slot) => *slot = bot.clone(),
        None => bots.push(bot.clone()),
    }
    store(app, bots)?;
    log::line(format!("grokbot {id}: saved"));
    Ok(bot)
}

pub fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let bots: Vec<GrokBot> = list(app).into_iter().filter(|b| b.id != id).collect();
    let _ = secrets::clear(&key_name(id));
    store(app, bots)?;
    log::line(format!("grokbot {id}: removed"));
    Ok(())
}

// ── Attachments ──────────────────────────────────────────────────────────────
// Webhook bodies stay small: text is inlined up to TEXT_TOTAL across the
// request, images travel as base64 within IMAGE_EACH / IMAGE_TOTAL (measured on
// the encoded string, so the body stays under MAX_BODY), and anything else is
// described by name, type, size and its path on this PC.

/// Inline text budget across one request (bytes of UTF-8).
pub(crate) const TEXT_TOTAL: usize = 200 * 1024;
/// Largest single image, as base64 characters.
pub(crate) const IMAGE_EACH: usize = 1024 * 1024;
/// All images in one request, as base64 characters.
pub(crate) const IMAGE_TOTAL: usize = 1536 * 1024;
/// Hard ceiling for the serialized JSON body.
pub(crate) const MAX_BODY: usize = 2 * 1024 * 1024;

/// What the island may attach: an `ingestFiles` id, or pasted text / bytes.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum AttachmentIn {
    Inbox { id: String },
    Text { name: String, mime: String, text: String },
    Base64 { name: String, mime: String, base64: String },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Content {
    /// Text to inline; `partial` when only the start of the file was read.
    Text { text: String, partial: bool },
    /// An image, already base64.
    Image(String),
    /// Nothing inlinable (other type, unreadable, invalid or missing).
    None,
}

/// One attachment after reading, before the caps are applied.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Where the bytes are on this PC, when they are.
    pub path: Option<String>,
    pub content: Content,
    /// Pasted bytes not yet on disk: written to the inbox only if the item
    /// ends up described by path instead of inlined.
    pub pending: Option<Vec<u8>>,
}

const CODE_EXTS: &[&str] = &[
    "txt", "md", "csv", "json", "log", "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "html", "htm", "css", "xml", "yaml",
    "yml", "toml", "sql", "ini", "cfg", "conf", "sh", "ps1", "bat", "c", "h", "cpp", "hpp", "cs", "java", "kt", "go",
    "rb", "php", "swift", "vue", "svelte", "svg",
];

pub(crate) fn is_text_like(name: &str, mime: &str) -> bool {
    let mime = mime.to_ascii_lowercase();
    let mime = mime.split(';').next().unwrap_or("").trim();
    if mime.starts_with("text/") {
        return true;
    }
    let sub = mime.strip_prefix("application/").unwrap_or("");
    if ["json", "xml", "yaml", "x-yaml", "toml", "sql", "javascript", "typescript", "x-sh"].contains(&sub)
        || sub.ends_with("+json")
        || sub.ends_with("+xml")
    {
        return true;
    }
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    CODE_EXTS.contains(&ext.as_str())
}

pub(crate) fn is_image(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().trim(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

/// Pasted base64: drops a `data:…;base64,` prefix and whitespace, then checks
/// the alphabet and padding. Returns the clean string and the decoded size.
pub(crate) fn clean_base64(raw: &str) -> Option<(String, usize)> {
    let raw = match raw.find(";base64,") {
        Some(i) if raw.starts_with("data:") => &raw[i + 8..],
        _ => raw,
    };
    let s: String = raw.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if s.len() % 4 != 0 {
        return None;
    }
    let body = s.trim_end_matches('=');
    let pad = s.len() - body.len();
    if pad > 2 || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return None;
    }
    let size = s.len() / 4 * 3 - pad;
    Some((s, size))
}

/// Decodes a string that `clean_base64` accepted.
pub(crate) fn decode_base64(s: &str) -> Vec<u8> {
    fn val(b: u8) -> u32 {
        match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            _ => 63,
        }
    }
    let bytes = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            n |= val(b) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

/// The longest prefix of `s` that fits in `max` bytes without splitting a char.
fn cut_at(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Applies the caps. Returns the outgoing attachment objects and the lines to
/// append to the message. `meta_only` describes everything by path (413 retry).
pub(crate) fn plan(items: &[Item], meta_only: bool) -> (Vec<Value>, Vec<String>) {
    let mut text_used = 0usize;
    let mut image_used = 0usize;
    let mut out = Vec::new();
    let mut lines = Vec::new();
    for item in items {
        let mut entry = json!({ "name": item.name, "mime": item.mime, "size": item.size });
        let inlined = match (&item.content, meta_only) {
            (_, true) | (Content::None, _) => false,
            (Content::Text { text, partial }, false) => {
                let room = TEXT_TOTAL.saturating_sub(text_used);
                if room == 0 {
                    false
                } else {
                    let kept = cut_at(text, room);
                    text_used += kept.len();
                    entry["text"] = json!(kept);
                    if *partial || kept.len() < text.len() {
                        entry["truncated"] = json!(true);
                        if let Some(path) = &item.path {
                            entry["path"] = json!(path);
                        }
                    }
                    true
                }
            }
            (Content::Image(b64), false) => {
                if b64.len() <= IMAGE_EACH && image_used + b64.len() <= IMAGE_TOTAL {
                    image_used += b64.len();
                    entry["base64"] = json!(b64);
                    true
                } else {
                    false
                }
            }
        };
        if !inlined {
            match &item.path {
                Some(path) => {
                    entry["path"] = json!(path);
                    lines.push(format!("(Adjunto en el PC: {path})"));
                }
                None => lines.push(format!("(Adjunto no incluido: {})", item.name)),
            }
        }
        out.push(entry);
    }
    (out, lines)
}

/// The webhook body. `attachments` is only present when there are some, so a
/// plain task looks exactly as it always did.
pub(crate) fn build_body(message: &str, lines: &[String], bot: &str, sent_at: &str, attachments: Vec<Value>) -> Value {
    let message = match (message.trim(), lines.is_empty()) {
        (m, true) => m.to_string(),
        ("", false) => lines.join("\n"),
        (m, false) => format!("{m}\n\n{}", lines.join("\n")),
    };
    let mut body = json!({
        "message": message,
        "from": "ARIA",
        "bot": bot,
        "sentAt": sent_at,
    });
    if !attachments.is_empty() {
        body["attachments"] = Value::Array(attachments);
    }
    body
}

/// Reads what the island attached. Missing inbox ids and invalid base64 are
/// kept as "not included" lines rather than failing the whole send.
fn read_items(attachments: Vec<AttachmentIn>) -> Vec<Item> {
    attachments
        .into_iter()
        .map(|a| match a {
            AttachmentIn::Inbox { id } => {
                let name = crate::files::display_name(&id);
                let mime = crate::files::guess_mime(&name).to_string();
                let Some(path) = crate::files::inbox_file(&id) else {
                    return Item { name, mime, size: 0, path: None, content: Content::None, pending: None };
                };
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let content = if is_text_like(&name, &mime) {
                    read_prefix(&path, TEXT_TOTAL)
                        .map(|bytes| Content::Text {
                            text: String::from_utf8_lossy(&bytes).into_owned(),
                            partial: (bytes.len() as u64) < size,
                        })
                        .unwrap_or(Content::None)
                } else if is_image(&mime) && (size as usize).div_ceil(3) * 4 <= IMAGE_EACH {
                    std::fs::read(&path).map(|b| Content::Image(crate::claude::base64_for(&b))).unwrap_or(Content::None)
                } else {
                    Content::None
                };
                Item { name, mime, size, path: Some(display_path(&path)), content, pending: None }
            }
            AttachmentIn::Text { name, mime, text } => {
                let size = text.len() as u64;
                Item { name, mime, size, path: None, pending: Some(text.clone().into_bytes()), content: Content::Text { text, partial: false } }
            }
            AttachmentIn::Base64 { name, mime, base64 } => match clean_base64(&base64) {
                Some((clean, size)) => {
                    let content = if is_image(&mime) { Content::Image(clean.clone()) } else { Content::None };
                    Item { name, mime, size: size as u64, path: None, content, pending: Some(decode_base64(&clean)) }
                }
                None => Item { name, mime, size: 0, path: None, content: Content::None, pending: None },
            },
        })
        .collect()
}

fn read_prefix(path: &std::path::Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(max as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Canonical Windows paths start with `\\?\`; the Bot gets the familiar form.
fn display_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Pasted items that the plan describes by path are written to the inbox first.
fn persist_described(items: &mut [Item], meta_only: bool) {
    let (planned, _) = plan(items, meta_only);
    for (item, entry) in items.iter_mut().zip(planned) {
        let described = entry.get("text").is_none() && entry.get("base64").is_none();
        let truncated = entry.get("truncated").is_some();
        if item.path.is_none() && (described || truncated) {
            if let Some(bytes) = item.pending.take() {
                match crate::files::save_to_inbox(&item.name, &bytes) {
                    Ok(path) => item.path = Some(display_path(&path)),
                    Err(e) => log::line(format!("grokbot: could not keep pasted {}: {e}", item.name)),
                }
            }
        }
    }
}

/// Serialized body for these items, or None when even that is over MAX_BODY.
fn assemble(items: &mut [Item], meta_only: bool, message: &str, bot: &str, sent_at: &str) -> Option<String> {
    persist_described(items, meta_only);
    let (attachments, lines) = plan(items, meta_only);
    let body = build_body(message, &lines, bot, sent_at, attachments).to_string();
    (body.len() <= MAX_BODY).then_some(body)
}

/// Starts a run of the Bot's routine with `message` and any attachments. Ok
/// means Grok Bot accepted it; the answer arrives later, in the Bot's chat and
/// (if it follows its instructions) in the island.
///
/// Errors are codes for the island to word: `not_connected` (no such Bot, no
/// URL or no key), `too_large`, `empty_message`, `http_<status>` (`http_0`
/// when the webhook could not be reached at all).
pub async fn send(app: &AppHandle, who: &str, message: &str, attachments: Vec<AttachmentIn>) -> Result<String, String> {
    let bot = find(app, who).ok_or("not_connected")?;
    if bot.url.trim().is_empty() {
        return Err("not_connected".into());
    }
    let message = message.trim();
    if message.is_empty() && attachments.is_empty() {
        return Err("empty_message".into());
    }
    let key = secrets::get(&key_name(&bot.id)).ok_or("not_connected")?;
    let t = crate::platform::local_time();
    let sent_at = format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute);
    let count = attachments.len();
    let mut items = read_items(attachments);

    let mut meta_only = false;
    let body = match assemble(&mut items, false, message, &bot.name, &sent_at) {
        Some(body) => body,
        None => {
            meta_only = true;
            assemble(&mut items, true, message, &bot.name, &sent_at).ok_or("too_large")?
        }
    };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(if count == 0 { 30 } else { 60 }))
        .build()
        .map_err(|_| "http_0".to_string())?;
    let post = |body: String| {
        client
            .post(&bot.url)
            .bearer_auth(&key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
    };
    let unreachable = |e: reqwest::Error| {
        log::line(format!("grokbot {}: webhook unreachable: {e}", bot.id));
        "http_0".to_string()
    };
    let mut response = post(body).await.map_err(unreachable)?;
    if response.status().as_u16() == 413 && count > 0 && !meta_only {
        log::line(format!("grokbot {}: webhook answered 413, retrying without inline attachments", bot.id));
        let body = assemble(&mut items, true, message, &bot.name, &sent_at).ok_or("too_large")?;
        response = post(body).await.map_err(unreachable)?;
    }
    let status = response.status();
    if status.is_success() {
        log::line(format!(
            "grokbot {}: task sent ({} chars, {count} attachments)",
            bot.id,
            message.chars().count()
        ));
        return Ok(format!("Enviado a {}. Grok Bot inició la rutina; el resultado llegará a su chat y a la isla.", bot.name));
    }
    log::line(format!("grokbot {}: webhook answered {status}", bot.id));
    Err(match status.as_u16() {
        413 => "too_large".to_string(),
        code => format!("http_{code}"),
    })
}

/// Words `send`'s error codes for the assistant (the island words them itself).
fn describe_error(bot: &str, code: &str) -> String {
    match code {
        "not_connected" => format!("{bot} is not connected: no such Grok Bot, or its webhook URL or key is missing."),
        "too_large" => "The task is too large for the webhook.".into(),
        "empty_message" => "The message is empty.".into(),
        "http_0" => "Could not reach Grok Bot (network error or timeout).".into(),
        "http_401" | "http_403" => format!("{bot}: the webhook key is not valid (was it changed in Grok Bot?)."),
        "http_404" => format!("{bot}: the webhook URL does not exist; check that the routine is still active."),
        "http_429" => format!("{bot}: Grok Bot is rate limiting or the weekly usage is used up."),
        other => format!("{bot}: Grok Bot answered {other}."),
    }
}

/// What to paste into the Bot's description in Grok Bot so it reports here.
pub fn instructions(app: &AppHandle, id: &str) -> Result<String, String> {
    let bot = find(app, id).ok_or("Ese Bot no existe.")?;
    let exe = settings::hook_exe_path().to_string_lossy().to_string();
    let cmd = if exe.contains(' ') { format!("& \"{exe}\"") } else { exe };
    let n = bot.name.replace('"', "");
    Ok(format!(
        "Estás conectado a ARIA, la isla de mi computadora (Windows). Las tareas que te mande ARIA llegan por el webhook \
de tu rutina «Tareas de ARIA», en el campo \"message\" del cuerpo JSON.\n\n\
Para avisarme, ejecuta estos comandos en MI computadora local (no en tu computadora en la nube):\n\n\
  {cmd} --bot \"{n}\" --status working \"qué estás haciendo\"\n\
  {cmd} --bot \"{n}\" --status done \"resumen del resultado en 1 a 3 frases\"\n\
  {cmd} --bot \"{n}\" --status needs \"qué necesitas de mí\"\n\
  {cmd} --bot \"{n}\" --status error \"qué falló\"\n\n\
Antes de cualquier acción importante (enviar, publicar, comprar, borrar), pídeme permiso así:\n\n\
  {cmd} --bot \"{n}\" --status ask \"¿Puedo publicar el post?\"\n\n\
El comando imprime allow si acepto, deny si lo rechazo o sin-respuesta si no contesto en 2 minutos. Continúa solo con allow.\n\
Cuando termines cada tarea, avísame siempre con --status done."
    ))
}

// ── Tools for the assistant ───────────────────────────────────────────────────

pub fn specs(app: &AppHandle) -> Vec<ToolSpec> {
    let bots = list(app);
    if bots.is_empty() {
        return Vec::new();
    }
    let names: Vec<String> = bots.iter().map(|b| b.name.clone()).collect();
    vec![
        ToolSpec {
            name: "grok_bots_list".into(),
            description: "List the owner's Grok Bots (persistent AI teammates in Cursor's Grok Bot, each with its own cloud computer, memory and plugins).".into(),
            schema: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "send_to_grok_bot".into(),
            description: format!(
                "Hand a task to one of the owner's Grok Bots ({}). Use it for long or multi-step work in the cloud: research, \
browsing, documents, plugins (Gmail, Slack, Notion…). The Bot works asynchronously and reports in the island when done; \
you will not get its answer back here. Write a complete, self-contained instruction with a clear finish line. \
Spends the owner's Grok Bot usage, so the owner approves each send.",
                names.join(", ")
            ),
            schema: json!({
                "type": "object",
                "properties": {
                    "bot": { "type": "string", "description": "The Bot's name" },
                    "message": { "type": "string", "description": "The task, complete and self-contained" }
                },
                "required": ["bot", "message"]
            }),
        },
    ]
}

pub async fn run(app: &AppHandle, call: &ToolCall) -> Option<Outcome> {
    let s = |k: &str| call.input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    match call.name.as_str() {
        "grok_bots_list" => {
            policy::audit("grok_bots_list", "read", "");
            let lines: Vec<String> = statuses(app)
                .into_iter()
                .map(|b| format!("- {}{}", b.bot.name, if b.has_key { "" } else { " (sin clave de webhook)" }))
                .collect();
            Some(Outcome::ok(if lines.is_empty() { "No Grok Bots configured.".into() } else { lines.join("\n") }))
        }
        "send_to_grok_bot" => {
            let (bot, message) = (s("bot"), s("message"));
            let target = format!("Mandar a {bot}: {}", message.chars().take(300).collect::<String>());
            if !policy::approve(app, "send_to_grok_bot", &target).await {
                return Some(Outcome::err("The owner declined sending this task."));
            }
            Some(match send(app, &bot, &message, Vec::new()).await {
                Ok(text) => Outcome::ok(text),
                Err(e) => Outcome::err(describe_error(&bot, &e)),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_match_the_relay() {
        assert_eq!(slug("Investigador"), "investigador");
        assert_eq!(slug("  Asistente de Ventas  "), "asistente-de-ventas");
        assert_eq!(slug("Diseño & Código!"), "diseno-codigo");
        assert_eq!(slug("¿?"), "");
        assert!(slug("un nombre larguísimo para un bot de prueba").len() <= 20);
    }

    #[test]
    fn colours_are_checked() {
        assert!(valid_color("#38BDF8"));
        assert!(!valid_color("38BDF8"));
        assert!(!valid_color("#38BDFZ"));
    }

    fn item(name: &str, mime: &str, size: u64, path: Option<&str>, content: Content) -> Item {
        Item { name: name.into(), mime: mime.into(), size, path: path.map(Into::into), content, pending: None }
    }

    fn text(s: &str) -> Content {
        Content::Text { text: s.into(), partial: false }
    }

    #[test]
    fn attachments_deserialize_untagged() {
        let v: Vec<AttachmentIn> = serde_json::from_str(
            r#"[{"id":"u1-a.txt"},{"name":"n.txt","mime":"text/plain","text":"hi"},{"name":"p.png","mime":"image/png","base64":"AAAA"}]"#,
        )
        .unwrap();
        assert_eq!(v[0], AttachmentIn::Inbox { id: "u1-a.txt".into() });
        assert!(matches!(&v[1], AttachmentIn::Text { text, .. } if text == "hi"));
        assert!(matches!(&v[2], AttachmentIn::Base64 { base64, .. } if base64 == "AAAA"));
    }

    #[test]
    fn text_like_and_images_are_recognised() {
        assert!(is_text_like("a.bin", "text/plain; charset=utf-8"));
        assert!(is_text_like("a", "application/json"));
        assert!(is_text_like("a", "application/ld+json"));
        assert!(is_text_like("main.rs", "application/octet-stream"));
        assert!(!is_text_like("a.pdf", "application/pdf"));
        assert!(is_image("image/PNG"));
        assert!(is_image("image/bmp"));
        assert!(!is_image("image/svg+xml"));
    }

    #[test]
    fn base64_is_validated_and_decoded() {
        assert_eq!(clean_base64("Zm9vYmFy"), Some(("Zm9vYmFy".into(), 6)));
        assert_eq!(clean_base64("Zm9vYg=="), Some(("Zm9vYg==".into(), 4)));
        assert_eq!(clean_base64("data:image/png;base64,Zm9v\r\nYmE="), Some(("Zm9vYmE=".into(), 5)));
        assert_eq!(clean_base64("Zm9"), None);
        assert_eq!(clean_base64("Zm9*"), None);
        assert_eq!(clean_base64("Z==="), None);
        for raw in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0u8, 255, 128, 7]] {
            let enc = crate::claude::base64_for(raw);
            let (clean, size) = clean_base64(&enc).unwrap();
            assert_eq!(size, raw.len());
            assert_eq!(decode_base64(&clean), raw);
        }
    }

    #[test]
    fn text_is_capped_across_the_request() {
        let big = "a".repeat(TEXT_TOTAL - 10);
        let items = vec![
            item("one.txt", "text/plain", big.len() as u64, Some("C:\\in\\one.txt"), text(&big)),
            item("two.md", "text/markdown", 30, Some("C:\\in\\two.md"), text("ñ".repeat(15).as_str())),
            item("three.txt", "text/plain", 5, Some("C:\\in\\three.txt"), text("hello")),
        ];
        let (out, lines) = plan(&items, false);
        assert_eq!(out[0]["text"].as_str().unwrap().len(), TEXT_TOTAL - 10);
        assert!(out[0].get("truncated").is_none());
        // 10 bytes of room: five two-byte chars, never half of one.
        assert_eq!(out[1]["text"], "ñññññ");
        assert_eq!(out[1]["truncated"], true);
        assert_eq!(out[1]["path"], "C:\\in\\two.md");
        // No room left: described by path, and the message says where it is.
        assert!(out[2].get("text").is_none());
        assert_eq!(out[2]["path"], "C:\\in\\three.txt");
        assert_eq!(lines, vec!["(Adjunto en el PC: C:\\in\\three.txt)".to_string()]);

        let partial = vec![item("log.txt", "text/plain", 9_000_000, Some("p"), Content::Text { text: "x".into(), partial: true })];
        assert_eq!(plan(&partial, false).0[0]["truncated"], true);
    }

    #[test]
    fn images_are_capped_each_and_in_total() {
        let small = "A".repeat(600 * 1024);
        let huge = "A".repeat(IMAGE_EACH + 4);
        let items = vec![
            item("a.png", "image/png", 1, Some("pa"), Content::Image(small.clone())),
            item("b.png", "image/png", 1, Some("pb"), Content::Image(huge)),
            item("c.png", "image/png", 1, Some("pc"), Content::Image(small.clone())),
            item("d.png", "image/png", 1, None, Content::Image(small)),
            item("e.zip", "application/zip", 9, Some("pe"), Content::None),
        ];
        let (out, lines) = plan(&items, false);
        assert!(out[0].get("base64").is_some());
        assert!(out[1].get("base64").is_none(), "over the per-image cap");
        assert!(out[2].get("base64").is_some());
        assert!(out[3].get("base64").is_none(), "over the per-request cap");
        assert!(out[4].get("base64").is_none() && out[4]["path"] == "pe");
        assert_eq!(
            lines,
            vec![
                "(Adjunto en el PC: pb)".to_string(),
                "(Adjunto no incluido: d.png)".to_string(),
                "(Adjunto en el PC: pe)".to_string()
            ]
        );
    }

    #[test]
    fn meta_only_describes_everything() {
        let items = vec![
            item("a.txt", "text/plain", 2, Some("pa"), text("hi")),
            item("b.png", "image/png", 3, Some("pb"), Content::Image("AAAA".into())),
        ];
        let (out, lines) = plan(&items, true);
        for entry in &out {
            assert!(entry.get("text").is_none() && entry.get("base64").is_none());
        }
        assert_eq!(out[0], json!({ "name": "a.txt", "mime": "text/plain", "size": 2, "path": "pa" }));
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn body_is_assembled() {
        let plain = build_body("  hola  ", &[], "Investigador", "2026-10-04 20:00", Vec::new());
        assert_eq!(
            plain,
            json!({ "message": "hola", "from": "ARIA", "bot": "Investigador", "sentAt": "2026-10-04 20:00" })
        );
        let items = vec![
            item("a.txt", "text/plain", 2, Some("pa"), text("hi")),
            item("z.zip", "application/zip", 9, Some("C:\\in\\z.zip"), Content::None),
        ];
        let (att, lines) = plan(&items, false);
        let body = build_body("mira", &lines, "Bot", "t", att);
        assert_eq!(body["message"], "mira\n\n(Adjunto en el PC: C:\\in\\z.zip)");
        assert_eq!(body["attachments"][0], json!({ "name": "a.txt", "mime": "text/plain", "size": 2, "text": "hi" }));
        assert_eq!(body["attachments"][1]["path"], "C:\\in\\z.zip");
        let only = build_body("", &lines, "Bot", "t", Vec::new());
        assert_eq!(only["message"], "(Adjunto en el PC: C:\\in\\z.zip)");
        // The caps keep the worst case under the hard ceiling.
        assert!(TEXT_TOTAL * 6 / 5 + IMAGE_TOTAL < MAX_BODY);
    }

    #[test]
    fn errors_are_worded_for_the_assistant() {
        assert!(describe_error("Bot", "http_401").contains("key"));
        assert!(describe_error("Bot", "not_connected").contains("not connected"));
        assert!(describe_error("Bot", "http_500").contains("http_500"));
    }
}
