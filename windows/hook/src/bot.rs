//! `coucou-hook --bot <name> [--status <s>] <message…>` — how the owner's Grok
//! Bots report to the island. Grok Bot runs it on this computer through its
//! local execution; nothing is read from stdin.
//!
//! Statuses (English or Spanish): working/trabajando, done/listo, needs/necesito,
//! error, ask/pregunta. `ask` waits for Allow / Deny in the island and prints
//! `allow`, `deny` or `sin-respuesta` (exit codes 0, 1, 2) so the Bot knows
//! what the owner decided. Every other status is fire-and-forget.

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

/// None when this is not a `--bot` call; otherwise the process exit code.
pub fn run() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    fn rejects_bad_calls() {
        assert!(parse(&args(&["PreToolUse"])).is_none(), "not a bot call");
        assert!(parse(&args(&["--bot"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "raro"])).unwrap().is_err());
        assert!(parse(&args(&["--bot", "X", "--status", "ask"])).unwrap().is_err());
    }
}
