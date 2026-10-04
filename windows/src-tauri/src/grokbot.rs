// The owner's Grok Bots (Cursor's Grok Bot app).
//
// Grok Bot has no chat API. Two documented doors make it work with the island:
//   * Coucou → Bot: each Bot gets a routine with a webhook trigger. A POST with
//     the routine's Bearer key starts a run with our JSON body; the result shows
//     in the Bot's own chat (a 200 only means "started").
//   * Bot → Coucou: Grok Bot can run commands on this computer (its local
//     execution). The Bot runs `coucou-hook --bot "<name>" --status … "<text>"`,
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

/// Starts a run of the Bot's routine with `message`. Ok means Grok Bot accepted
/// it; the answer arrives later, in the Bot's chat and (if it follows its
/// instructions) in the island.
pub async fn send(app: &AppHandle, who: &str, message: &str) -> Result<String, String> {
    let bot = find(app, who).ok_or_else(|| format!("No tienes ningún Bot llamado «{who}»."))?;
    let message = message.trim();
    if message.is_empty() {
        return Err("El mensaje está vacío.".into());
    }
    let key = secrets::get(&key_name(&bot.id)).ok_or("Falta la clave del webhook de este Bot. Ábrelo en Ajustes → Mis Bots de Grok.")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let t = crate::platform::local_time();
    let body = json!({
        "message": message,
        "from": "Coucou",
        "bot": bot.name,
        "sentAt": format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute),
    });
    let response = client
        .post(&bot.url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("No se pudo contactar a Grok Bot: {e}"))?;
    let status = response.status();
    if status.is_success() {
        log::line(format!("grokbot {}: task sent ({} chars)", bot.id, message.chars().count()));
        return Ok(format!("Enviado a {}. Grok Bot inició la rutina; el resultado llegará a su chat y a la isla.", bot.name));
    }
    let text: String = response.text().await.unwrap_or_default().chars().take(300).collect();
    log::line(format!("grokbot {}: webhook answered {status}", bot.id));
    Err(match status.as_u16() {
        401 | 403 => format!("{}: la clave del webhook no es válida (¿la cambiaste en Grok Bot?).", bot.name),
        404 => format!("{}: la URL del webhook no existe. Revisa que la rutina siga activa.", bot.name),
        429 => format!("{}: Grok Bot está limitando las peticiones o se acabó el uso semanal.", bot.name),
        _ => format!("{}: Grok Bot respondió {status}. {text}", bot.name),
    })
}

/// What to paste into the Bot's description in Grok Bot so it reports here.
pub fn instructions(app: &AppHandle, id: &str) -> Result<String, String> {
    let bot = find(app, id).ok_or("Ese Bot no existe.")?;
    let exe = settings::hook_exe_path().to_string_lossy().to_string();
    let cmd = if exe.contains(' ') { format!("& \"{exe}\"") } else { exe };
    let n = bot.name.replace('"', "");
    Ok(format!(
        "Estás conectado a Coucou, la isla de mi computadora (Windows). Las tareas que te mande Coucou llegan por el webhook \
de tu rutina «Tareas de Coucou», en el campo \"message\" del cuerpo JSON.\n\n\
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
            Some(match send(app, &bot, &message).await {
                Ok(text) => Outcome::ok(text),
                Err(e) => Outcome::err(e),
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
}
