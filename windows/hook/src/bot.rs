//! `aria-hook --bot <name> [--status <s>] <message…>` — how the owner's Grok
//! Bots report to the island. Grok Bot runs it on this computer through its
//! local execution; nothing is read from stdin.
//!
//! Statuses (English or Spanish): working/trabajando, done/listo, needs/necesito,
//! error, ask/pregunta. `ask` waits for Allow / Deny in the island and prints
//! `allow`, `deny` or `sin-respuesta` (exit codes 0, 1, 2) so the Bot knows
//! what the owner decided. Every other status is fire-and-forget.
//!
//! Questions with buttons: `aria-hook --bot <name> [--status ask]
//! --options "A|B::description|C" [--allow-custom] "<question>"`. `|` separates
//! options, `::` a label from its description, `\|` is a literal pipe; at most
//! 12 options, labels cut to 60 characters, repeats dropped, an empty label is
//! refused (exit 64). `--options` alone means `--status ask`. The owner's pick
//! (or the text typed in "Otra respuesta", only with `--allow-custom`) is
//! printed with exit 0; Denegar prints `deny` (exit 1); no answer prints
//! `sin-respuesta` (exit 2). With `--status needs` the buttons are shown in the
//! Bot's conversation and nothing is waited for.
//!
//! Cards on the pill:
//! - `aria-hook --step "<text>" --bot <name>`: a progress step (fire and
//!   forget, exit 0). The app redacts it and keeps ~200 characters.
//! - `aria-hook --attach <path> --bot <name> [--caption "<text>"]`: a file
//!   card. The app checks the file exists; prints `ok` (exit 0), or the reason
//!   on stderr (exit 1); exit 2 when ARIA is not running.

use std::sync::mpsc;

use serde_json::json;

use super::{talk, DECISION_BUDGET, FIRE_AND_FORGET_BUDGET};
use crate::choices;

/// The pill id the app gives a Bot of this name: same rule as grokbot.rs.
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

fn status(raw: &str) -> Option<&'static str> {
    Some(match raw.trim().to_lowercase().as_str() {
        "" | "working" | "trabajando" | "progress" | "progreso" => "working",
        "done" | "listo" | "terminado" | "finished" => "done",
        "needs" | "necesito" | "ayuda" | "attention" => "needs",
        "error" | "fallo" | "failed" => "error",
        "ask" | "pregunta" | "aprobar" | "approve" => "ask",
        _ => return None,
    })
}

struct BotArgs {
    name: String,
    status: &'static str,
    message: String,
    /// `[{label, description?}]`, already checked; empty without `--options`.
    options: Vec<serde_json::Value>,
    allow_custom: bool,
}

/// `--options "A|B::description|C"`: `|` separates options, `::` a label from
/// its description, `\|` is a literal `|`. Limits in choices::clean_options.
fn parse_options(raw: &str) -> Result<Vec<serde_json::Value>, String> {
    if raw.trim().is_empty() {
        return Err("--options necesita al menos una opción: --options \"Sí|No\"".into());
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                chars.next();
                current.push('|');
            }
            '|' => parts.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    parts.push(current);
    let pairs: Vec<(String, String)> = parts
        .iter()
        .map(|p| match p.split_once("::") {
            Some((label, description)) => (label.to_string(), description.to_string()),
            None => (p.clone(), String::new()),
        })
        .collect();
    choices::clean_options(&pairs).map_err(|e| format!("--options: {e}"))
}

fn parse(args: &[String]) -> Option<Result<BotArgs, String>> {
    let at = args.iter().position(|a| a == "--bot")?;
    let mut name = String::new();
    let mut raw_status = String::new();
    let mut raw_options: Option<String> = None;
    let mut allow_custom = false;
    let mut words = Vec::new();
    let mut it = args.iter().enumerate();
    while let Some((i, arg)) = it.next() {
        if i == at {
            name = it.next().map(|(_, v)| v.clone()).unwrap_or_default();
        } else if arg == "--status" {
            raw_status = it.next().map(|(_, v)| v.clone()).unwrap_or_default();
        } else if arg == "--options" {
            raw_options = Some(it.next().map(|(_, v)| v.clone()).unwrap_or_default());
        } else if arg == "--allow-custom" {
            allow_custom = true;
        } else {
            words.push(arg.clone());
        }
    }
    let id = slug(&name);
    if id.is_empty() {
        return Some(Err("falta el nombre del Bot: --bot \"Nombre\"".into()));
    }
    // Buttons are a question: --options alone means --status ask.
    let status = if raw_options.is_some() && raw_status.trim().is_empty() { Some("ask") } else { status(&raw_status) };
    let Some(status) = status else {
        return Some(Err(format!("estado desconocido: {raw_status} (usa working, done, needs, error o ask)")));
    };
    let options = match raw_options.as_deref().map(parse_options) {
        None => Vec::new(),
        Some(Ok(o)) => o,
        Some(Err(e)) => return Some(Err(e)),
    };
    if !options.is_empty() && status != "ask" && status != "needs" {
        return Some(Err("--options solo va con --status ask (o needs)".into()));
    }
    if allow_custom && options.is_empty() {
        return Some(Err("--allow-custom necesita --options".into()));
    }
    let message: String = words.join(" ").trim().chars().take(4000).collect();
    if status == "ask" && message.is_empty() {
        return Some(Err("--status ask necesita la pregunta".into()));
    }
    Some(Ok(BotArgs { name: name.trim().to_string(), status, message, options, allow_custom }))
}

/// The line sent to the app for a status call.
fn status_payload(bot: &BotArgs) -> serde_json::Value {
    let mut payload = json!({
        "aria_agent": format!("bot-{}", slug(&bot.name)),
        "aria_bot": bot.name,
        "message": bot.message,
        "cwd": "",
    });
    if bot.status == "ask" {
        payload["hook_event_name"] = json!("PermissionRequest");
        payload["tool_name"] = json!("Pregunta");
        payload["tool_input"] = json!({ "command": bot.message });
    } else {
        payload["hook_event_name"] = json!("BotUpdate");
        payload["bot_status"] = json!(bot.status);
    }
    if !bot.options.is_empty() {
        payload["options"] = json!(bot.options);
        payload["allowCustom"] = json!(bot.allow_custom);
    }
    payload
}

/// The island's reply to `--status ask` → what is printed and the exit code.
/// A pick from the options (or the typed text) is printed as is, exit 0.
fn verdict(raw: &str) -> (String, i32) {
    match choices::parse_reply(raw) {
        Some(r) if r.decision == "allow" || r.decision == "always" => {
            (r.answer().map(str::to_string).unwrap_or_else(|| "allow".into()), 0)
        }
        Some(r) if r.decision == "deny" => ("deny".into(), 1),
        _ => ("sin-respuesta".into(), 2),
    }
}

#[derive(Debug, PartialEq)]
enum Card {
    Step { name: String, text: String },
    Attach { name: String, path: String, caption: Option<String> },
}

/// `--step` / `--attach` calls. None when neither flag is present.
fn parse_card(args: &[String]) -> Option<Result<Card, String>> {
    let has = |f: &str| args.iter().any(|a| a == f);
    if !has("--step") && !has("--attach") {
        return None;
    }
    if has("--step") && has("--attach") {
        return Some(Err("usa --step o --attach, no los dos".into()));
    }
    let mut name = None;
    let mut step = None;
    let mut path = None;
    let mut caption = None;
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--bot" => name = it.next().cloned(),
            "--step" => step = it.next().cloned(),
            "--attach" => path = it.next().cloned(),
            "--caption" => caption = it.next().cloned(),
            // A status or options mean nothing on a card; keep them out of the text.
            "--status" | "--options" => {
                it.next();
            }
            "--allow-custom" => {}
            _ => words.push(arg.clone()),
        }
    }
    let name = name.unwrap_or_default().trim().to_string();
    if slug(&name).is_empty() {
        return Some(Err("falta el nombre del Bot: --bot \"Nombre\"".into()));
    }
    if let Some(first) = step {
        // An unquoted step arrives as several words: keep them all.
        let mut all = vec![first];
        all.extend(words);
        let text: String = all.join(" ").trim().chars().take(2000).collect();
        if text.is_empty() {
            return Some(Err("--step necesita un texto".into()));
        }
        return Some(Ok(Card::Step { name, text }));
    }
    let path = path.unwrap_or_default().trim().to_string();
    if path.is_empty() {
        return Some(Err("--attach necesita la ruta del archivo".into()));
    }
    let caption = caption.map(|c| c.trim().chars().take(2000).collect::<String>()).filter(|c| !c.is_empty());
    Some(Ok(Card::Attach { name, path, caption }))
}

fn card_payload(card: &Card) -> serde_json::Value {
    match card {
        Card::Step { name, text } => json!({
            "aria_kind": "bot_step",
            "aria_agent": format!("bot-{}", slug(name)),
            "aria_bot": name,
            "text": text,
        }),
        Card::Attach { name, path, caption } => {
            // The app wants an absolute path; resolve a relative one here, where
            // the working directory is the Bot's.
            let abs = std::path::absolute(path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.clone());
            let mut v = json!({
                "aria_kind": "bot_attach",
                "aria_agent": format!("bot-{}", slug(name)),
                "aria_bot": name,
                "path": abs,
            });
            if let Some(c) = caption {
                v["caption"] = json!(c);
            }
            v
        }
    }
}

fn run_card(card: Card) -> i32 {
    let waits = matches!(card, Card::Attach { .. });
    let mut line = card_payload(&card).to_string();
    line.push('\n');
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let _ = tx.send(talk(&line, waits));
    });
    let answer = rx.recv_timeout(FIRE_AND_FORGET_BUDGET + std::time::Duration::from_secs(1)).ok().flatten();
    if !waits {
        return 0;
    }
    let Some(answer) = answer else {
        eprintln!("aria-hook: ARIA no respondió (¿está abierta?)");
        return 2;
    };
    let v: serde_json::Value = serde_json::from_str(answer.trim()).unwrap_or_default();
    if v["ok"] == json!(true) {
        println!("ok");
        0
    } else {
        eprintln!("aria-hook: {}", v["error"].as_str().unwrap_or("ARIA rechazó el archivo"));
        1
    }
}

/// None when this is not a `--bot` call; otherwise the process exit code.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_card(&args) {
        Some(Ok(card)) => return Some(run_card(card)),
        Some(Err(e)) => {
            eprintln!("aria-hook: {e}");
            return Some(64);
        }
        None => {}
    }
    let bot = match parse(&args)? {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aria-hook: {e}");
            return Some(64);
        }
    };
    let asking = bot.status == "ask";
    let mut line = status_payload(&bot).to_string();
    line.push('\n');

    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let _ = tx.send(talk(&line, asking));
    });
    let budget = if asking { DECISION_BUDGET } else { FIRE_AND_FORGET_BUDGET };
    let answer = rx.recv_timeout(budget).ok().flatten().unwrap_or_default();
    if !asking {
        return Some(0);
    }
    let (printed, code) = verdict(&answer);
    println!("{printed}");
    Some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn slugs_match_the_app() {
        assert_eq!(slug("Investigador"), "investigador");
        assert_eq!(slug("  Asistente de Ventas  "), "asistente-de-ventas");
        assert_eq!(slug("Diseño & Código!"), "diseno-codigo");
        assert_eq!(slug("¿?"), "");
        assert!(slug("un nombre larguísimo para un bot de prueba").len() <= 20);
    }

    #[test]
    fn parses_status_and_message_in_any_order() {
        let b = parse(&args(&["--status", "listo", "--bot", "Investigador", "Terminé", "el", "informe"])).unwrap().unwrap();
        assert_eq!(b.name, "Investigador");
        assert_eq!(b.status, "done");
        assert_eq!(b.message, "Terminé el informe");
        let b = parse(&args(&["--bot", "Ventas", "Revisando", "correos"])).unwrap().unwrap();
        assert_eq!(b.status, "working");
    }

    #[test]
    fn parses_steps() {
        let c = parse_card(&args(&["--step", "Leyendo correos", "--bot", "Ventas"])).unwrap().unwrap();
        assert_eq!(c, Card::Step { name: "Ventas".into(), text: "Leyendo correos".into() });
        let c = parse_card(&args(&["--bot", "Ventas", "--step", "Leyendo", "correos"])).unwrap().unwrap();
        assert_eq!(c, Card::Step { name: "Ventas".into(), text: "Leyendo correos".into() });
        let v = card_payload(&c);
        assert_eq!(v["aria_kind"], "bot_step");
        assert_eq!(v["aria_agent"], "bot-ventas");
        assert_eq!(v["text"], "Leyendo correos");
        assert!(parse_card(&args(&["--bot", "Ventas", "hola"])).is_none(), "a plain status call");
    }

    #[test]
    fn parses_attachments() {
        let c = parse_card(&args(&["--attach", "C:\\x\\informe.pdf", "--bot", "Ventas", "--caption", "El informe"]))
            .unwrap()
            .unwrap();
        assert_eq!(
            c,
            Card::Attach { name: "Ventas".into(), path: "C:\\x\\informe.pdf".into(), caption: Some("El informe".into()) }
        );
        let v = card_payload(&c);
        assert_eq!(v["aria_kind"], "bot_attach");
        assert_eq!(v["caption"], "El informe");
        let c = parse_card(&args(&["--bot", "V", "--attach", "informe.pdf"])).unwrap().unwrap();
        let v = card_payload(&c);
        assert!(std::path::Path::new(v["path"].as_str().unwrap()).is_absolute(), "relative paths are resolved");
        assert!(v.get("caption").is_none());
    }

    #[test]
    fn rejects_bad_cards() {
        assert!(parse_card(&args(&["--step", "x"])).unwrap().is_err(), "no bot");
        assert!(parse_card(&args(&["--bot", "V", "--step"])).unwrap().is_err(), "no text");
        assert!(parse_card(&args(&["--bot", "V", "--attach"])).unwrap().is_err(), "no path");
        assert!(parse_card(&args(&["--bot", "V", "--step", "a", "--attach", "b"])).unwrap().is_err());
    }

    #[test]
    fn parses_options() {
        let b = parse(&args(&["--bot", "Ventas", "--options", r"Sí::la buena|No|A\|B|sí", "--allow-custom", "¿Mando", "el", "correo?"]))
            .unwrap()
            .unwrap();
        assert_eq!(b.status, "ask", "--options alone is a question");
        assert_eq!(b.message, "¿Mando el correo?");
        assert!(b.allow_custom);
        assert_eq!(
            b.options,
            vec![json!({ "label": "Sí", "description": "la buena" }), json!({ "label": "No" }), json!({ "label": "A|B" })]
        );
        let v = status_payload(&b);
        assert_eq!(v["hook_event_name"], "PermissionRequest");
        assert_eq!(v["tool_name"], "Pregunta");
        assert_eq!(v["tool_input"]["command"], "¿Mando el correo?");
        assert_eq!(v["options"][2]["label"], "A|B");
        assert_eq!(v["allowCustom"], true);

        let n = parse(&args(&["--bot", "V", "--status", "needs", "--options", "x|y", "¿Cuál?"])).unwrap().unwrap();
        let v = status_payload(&n);
        assert_eq!(v["hook_event_name"], "BotUpdate");
        assert_eq!(v["allowCustom"], false);
        let plain = status_payload(&parse(&args(&["--bot", "V", "--status", "ask", "¿Sigo?"])).unwrap().unwrap());
        assert!(plain.get("options").is_none() && plain.get("allowCustom").is_none(), "no options, same card as before");
        let long = "x".repeat(80);
        let l = parse(&args(&["--bot", "V", "--options", &long, "¿?"])).unwrap().unwrap();
        assert_eq!(l.options[0]["label"].as_str().unwrap().chars().count(), 60);
    }

    #[test]
    fn rejects_bad_options() {
        let err = |list: &[&str]| parse(&args(list)).unwrap().is_err();
        assert!(err(&["--bot", "V", "--options", "A||B", "¿?"]), "empty label");
        assert!(err(&["--bot", "V", "--options", "A|", "¿?"]), "trailing pipe");
        assert!(err(&["--bot", "V", "--options", " ", "¿?"]));
        assert!(err(&["--bot", "V", "--options"]), "no value");
        assert!(err(&["--bot", "V", "--options", "1|2|3|4|5|6|7|8|9|10|11|12|13", "¿?"]), "more than 12");
        assert!(!err(&["--bot", "V", "--options", "1|2|3|4|5|6|7|8|9|10|11|12|1", "¿?"]), "12 once repeats are gone");
        assert!(err(&["--bot", "V", "--options", "A|B"]), "a question is needed");
        assert!(err(&["--bot", "V", "--status", "done", "--options", "A|B", "x"]));
        assert!(err(&["--bot", "V", "--allow-custom", "--status", "ask", "¿?"]), "--allow-custom without options");
        let c = parse_card(&args(&["--step", "Leyendo", "--bot", "V", "--options", "A|B", "--allow-custom"])).unwrap().unwrap();
        assert_eq!(c, Card::Step { name: "V".into(), text: "Leyendo".into() });
    }

    #[test]
    fn exit_contract() {
        assert_eq!(verdict("allow\n"), ("allow".to_string(), 0));
        assert_eq!(verdict("always"), ("allow".to_string(), 0));
        assert_eq!(verdict(r#"{"decision":"allow","answer":"Opción B"}"#), ("Opción B".to_string(), 0));
        assert_eq!(verdict(r#"{"decision":"allow","answer":"texto libre: mañana"}"#), ("texto libre: mañana".to_string(), 0));
        assert_eq!(verdict("deny"), ("deny".to_string(), 1));
        assert_eq!(verdict(r#"{"decision":"deny","answer":"B"}"#), ("deny".to_string(), 1));
        // Timeout, island closed, ARIA not running: nothing came back.
        assert_eq!(verdict(""), ("sin-respuesta".to_string(), 2));
        assert_eq!(verdict("maybe"), ("sin-respuesta".to_string(), 2));
        assert_eq!(verdict("{not json"), ("sin-respuesta".to_string(), 2));
    }

    #[test]
    fn rejects_bad_calls() {
        assert!(parse(&args(&["PreToolUse"])).is_none(), "not a bot call");
        assert!(parse(&args(&["--bot"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "raro"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "ask"])).unwrap().is_err());
    }
}
