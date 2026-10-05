//! `coucou-hook --bot <name> [--status <s>] <message…>` — how the owner's Grok
//! Bots report to the island. Grok Bot runs it on this computer through its
//! local execution; nothing is read from stdin.
//!
//! Statuses (English or Spanish): working/trabajando, done/listo, needs/necesito,
//! error, ask/pregunta. `ask` waits for Allow / Deny in the island and prints
//! `allow`, `deny` or `sin-respuesta` (exit codes 0, 1, 2) so the Bot knows
//! what the owner decided. Every other status is fire-and-forget.
//!
//! Cards on the pill:
//! - `coucou-hook --step "<text>" --bot <name>`: a progress step (fire and
//!   forget, exit 0). The app redacts it and keeps ~200 characters.
//! - `coucou-hook --attach <path> --bot <name> [--caption "<text>"]`: a file
//!   card. The app checks the file exists; prints `ok` (exit 0), or the reason
//!   on stderr (exit 1); exit 2 when Coucou is not running.

use std::sync::mpsc;

use serde_json::json;

use super::{talk, DECISION_BUDGET, FIRE_AND_FORGET_BUDGET};

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
}

fn parse(args: &[String]) -> Option<Result<BotArgs, String>> {
    let at = args.iter().position(|a| a == "--bot")?;
    let mut name = String::new();
    let mut raw_status = String::new();
    let mut words = Vec::new();
    let mut it = args.iter().enumerate();
    while let Some((i, arg)) = it.next() {
        if i == at {
            name = it.next().map(|(_, v)| v.clone()).unwrap_or_default();
        } else if arg == "--status" {
            raw_status = it.next().map(|(_, v)| v.clone()).unwrap_or_default();
        } else {
            words.push(arg.clone());
        }
    }
    let id = slug(&name);
    if id.is_empty() {
        return Some(Err("falta el nombre del Bot: --bot \"Nombre\"".into()));
    }
    let Some(status) = status(&raw_status) else {
        return Some(Err(format!("estado desconocido: {raw_status} (usa working, done, needs, error o ask)")));
    };
    let message: String = words.join(" ").trim().chars().take(4000).collect();
    if status == "ask" && message.is_empty() {
        return Some(Err("--status ask necesita la pregunta".into()));
    }
    Some(Ok(BotArgs { name: name.trim().to_string(), status, message }))
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
            // A status means nothing on a card; do not let it leak into the text.
            "--status" => {
                it.next();
            }
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
            "coucou_kind": "bot_step",
            "coucou_agent": format!("bot-{}", slug(name)),
            "coucou_bot": name,
            "text": text,
        }),
        Card::Attach { name, path, caption } => {
            // The app wants an absolute path; resolve a relative one here, where
            // the working directory is the Bot's.
            let abs = std::path::absolute(path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.clone());
            let mut v = json!({
                "coucou_kind": "bot_attach",
                "coucou_agent": format!("bot-{}", slug(name)),
                "coucou_bot": name,
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
        eprintln!("coucou-hook: Coucou no respondió (¿está abierto?)");
        return 2;
    };
    let v: serde_json::Value = serde_json::from_str(answer.trim()).unwrap_or_default();
    if v["ok"] == json!(true) {
        println!("ok");
        0
    } else {
        eprintln!("coucou-hook: {}", v["error"].as_str().unwrap_or("Coucou rechazó el archivo"));
        1
    }
}

/// None when this is not a `--bot` call; otherwise the process exit code.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_card(&args) {
        Some(Ok(card)) => return Some(run_card(card)),
        Some(Err(e)) => {
            eprintln!("coucou-hook: {e}");
            return Some(64);
        }
        None => {}
    }
    let bot = match parse(&args)? {
        Ok(b) => b,
        Err(e) => {
            eprintln!("coucou-hook: {e}");
            return Some(64);
        }
    };
    let asking = bot.status == "ask";
    let mut payload = json!({
        "coucou_agent": format!("bot-{}", slug(&bot.name)),
        "coucou_bot": bot.name,
        "message": bot.message,
        "cwd": "",
    });
    if asking {
        payload["hook_event_name"] = json!("PermissionRequest");
        payload["tool_name"] = json!("Pregunta");
        payload["tool_input"] = json!({ "command": bot.message });
    } else {
        payload["hook_event_name"] = json!("BotUpdate");
        payload["bot_status"] = json!(bot.status);
    }
    let mut line = payload.to_string();
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
    let code = match answer.trim() {
        "allow" | "always" => {
            println!("allow");
            0
        }
        "deny" => {
            println!("deny");
            1
        }
        _ => {
            println!("sin-respuesta");
            2
        }
    };
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
        assert_eq!(v["coucou_kind"], "bot_step");
        assert_eq!(v["coucou_agent"], "bot-ventas");
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
        assert_eq!(v["coucou_kind"], "bot_attach");
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
    fn rejects_bad_calls() {
        assert!(parse(&args(&["PreToolUse"])).is_none(), "not a bot call");
        assert!(parse(&args(&["--bot"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "raro"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "ask"])).unwrap().is_err());
    }
}
