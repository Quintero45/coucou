// Sending a dropped file by email, from the island's mail card. Only the
// owner's click on "Send" gets here: the assistant can fill the card in
// (compose_email) but never send through it, "Without asking" or not.
//
// Outlook (classic) does the sending through its automation interface, with
// the account already set up in it: nothing is stored here and no other
// connection is made. Without Outlook (the new Outlook has no automation),
// the owner's Resend integration is used if it is configured.
//
// aria.log keeps who it went to, the subject and the file's name — never the text.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::{log, secrets};

/// Larger attachments are refused before anything is sent.
pub const MAX_ATTACHMENT: u64 = 20 * 1024 * 1024;
/// Outlook can take a while to start and push the message out.
const OUTLOOK_TIMEOUT: Duration = Duration::from_secs(90);
const CONTACTS_KEPT: usize = 30;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailRequest {
    pub to: String,
    pub subject: String,
    #[serde(default)]
    pub body: String,
    /// The dropped file (its inbox copy), if any.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MailSent {
    /// "outlook" or "resend".
    pub via: &'static str,
    pub to: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailStatus {
    pub outlook: bool,
    pub resend: bool,
    /// The sender address Resend needs (Settings), "" if none yet.
    pub resend_from: String,
    /// The attachment's size in bytes, when it is there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contact {
    #[serde(default)]
    pub name: String,
    pub address: String,
}

// ── Checks ────────────────────────────────────────────────────────────────────

/// One address, plain or as `Name <address>`; `None` if it isn't one.
fn address(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let inner = match (raw.rfind('<'), raw.rfind('>')) {
        (Some(a), Some(b)) if a < b => &raw[a + 1..b],
        _ => raw,
    };
    let a = inner.trim();
    let (local, domain) = a.split_once('@')?;
    let ok_char = |c: char| !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | ',' | ';' | '@' | '(' | ')' | '[' | ']' | '\\');
    let labels_ok = domain.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-'));
    if local.is_empty() || local.len() > 64 || !local.chars().all(ok_char) || local.starts_with('.') || local.ends_with('.') {
        return None;
    }
    if !domain.contains('.') || !labels_ok || !domain.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-') {
        return None;
    }
    Some(a.to_string())
}

/// "a@x.com; Ana <b@y.com>, c@z.com" → the addresses, or the first one that isn't valid.
pub fn recipients(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split([',', ';', '\n']).map(str::trim).filter(|p| !p.is_empty()) {
        let Some(a) = address(part) else { return Err(format!("bad_address:{part}")) };
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&a)) {
            out.push(a);
        }
    }
    if out.is_empty() {
        return Err("no_recipient".into());
    }
    Ok(out)
}

/// The attachment, if there is one: it must exist and stay under 20 MB.
pub fn attachment(path: Option<&str>) -> Result<Option<(PathBuf, u64)>, String> {
    let Some(raw) = path.map(str::trim).filter(|p| !p.is_empty()) else { return Ok(None) };
    let p = PathBuf::from(raw);
    let meta = std::fs::metadata(&p).map_err(|_| "file_missing".to_string())?;
    if !meta.is_file() {
        return Err("file_missing".into());
    }
    if meta.len() > MAX_ATTACHMENT {
        return Err("file_too_big".into());
    }
    Ok(Some((p, meta.len())))
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ── Commands ──────────────────────────────────────────────────────────────────

fn resend_from(app: &AppHandle) -> String {
    app.try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().mail_from.trim().to_string())
        .unwrap_or_default()
}

#[tauri::command]
pub async fn mail_status(app: AppHandle, path: Option<String>) -> MailStatus {
    MailStatus {
        outlook: tauri::async_runtime::spawn_blocking(outlook::installed).await.unwrap_or(false),
        resend: secrets::present("resend-api-key"),
        resend_from: resend_from(&app),
        size: path.and_then(|p| std::fs::metadata(p).ok()).filter(|m| m.is_file()).map(|m| m.len()),
    }
}

/// People the owner wrote to lately, from Outlook's Sent Items (kept for the session).
#[tauri::command]
pub async fn mail_contacts() -> Vec<Contact> {
    static CACHE: std::sync::Mutex<Option<Vec<Contact>>> = std::sync::Mutex::new(None);
    if let Some(c) = CACHE.lock().unwrap().clone() {
        return c;
    }
    let found = outlook::recent_contacts().await;
    if !found.is_empty() {
        *CACHE.lock().unwrap() = Some(found.clone());
    }
    found
}

/// The card's "Send" button. Errors are codes the card turns into words.
#[tauri::command]
pub async fn mail_send(app: AppHandle, request: MailRequest) -> Result<MailSent, String> {
    let to = recipients(&request.to)?;
    let file = attachment(request.path.as_deref())?;
    let subject = request.subject.trim().to_string();
    let subject = if subject.is_empty() {
        file.as_ref().map(|(p, _)| file_name(p)).unwrap_or_default()
    } else {
        subject
    };
    let what = format!(
        "to={} subject={:?} file={}",
        to.join(","),
        subject.chars().take(120).collect::<String>(),
        file.as_ref().map(|(p, n)| format!("{} ({n} bytes)", file_name(p))).unwrap_or_else(|| "none".into()),
    );

    let via = if tauri::async_runtime::spawn_blocking(outlook::installed).await.unwrap_or(false) {
        outlook::send(&to, &subject, &request.body, file.as_ref().map(|(p, _)| p.as_path())).await.map(|_| "outlook")
    } else if secrets::present("resend-api-key") {
        let from = resend_from(&app);
        if address(&from).is_none() {
            Err("resend_no_from".to_string())
        } else {
            resend(&from, &to, &subject, &request.body, file.as_ref().map(|(p, _)| p.as_path())).await.map(|_| "resend")
        }
    } else {
        Err("no_mail".to_string())
    };

    match via {
        Ok(via) => {
            log::line(format!("mail sent via {via} {what} (owner's click)"));
            let _ = app.emit("mail-sent", json!({ "via": via }));
            Ok(MailSent { via, to })
        }
        Err(e) => {
            log::line(format!("mail not sent ({}) {what}", e.chars().take(200).collect::<String>()));
            Err(e)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Draft {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
}

/// `{"subject": …, "body": …}` out of a model's answer, code fences and all.
fn parse_draft(text: &str) -> Option<Draft> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let d: Draft = serde_json::from_str(text.get(start..=end)?).ok()?;
    (!d.subject.trim().is_empty() || !d.body.trim().is_empty()).then_some(d)
}

/// The card's "Write it" button: a subject and a short text for the file, in
/// one call to the owner's AI provider (no tools, nothing else read).
#[tauri::command]
pub async fn mail_draft(app: AppHandle, file_name: String, to: String, hint: String) -> Result<Draft, String> {
    use crate::providers::{self, Endpoint, Msg, Part, Provider, Request};
    let settings = app.state::<crate::Shared>().settings.lock().unwrap().clone();
    let mut ep = Endpoint::new(&settings, settings.provider)?;
    if ep.provider == Provider::Cursor {
        ep = Endpoint::new(&settings, Provider::Anthropic)?;
    }
    let system = "You write short, friendly, professional emails for your owner, in the language of their notes (Spanish if none). \
Answer with JSON only: {\"subject\": \"…\", \"body\": \"…\"}. The body is plain text, 2 to 5 sentences, no signature placeholder.";
    let ask = format!(
        "Attached file: {}\nTo: {}\nWhat the owner wants (may be empty): {}",
        if file_name.trim().is_empty() { "none" } else { file_name.trim() },
        to.trim(),
        hint.trim(),
    );
    let msgs = [Msg::User(vec![Part::Text(ask)])];
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let turn = providers::complete(
        &ep,
        Request { system, messages: &msgs, tools: &[], web_search: false },
        &mut |_| {},
        &cancel,
    )
    .await?;
    parse_draft(&turn.text).ok_or_else(|| "no_draft".to_string())
}

async fn resend(from: &str, to: &[String], subject: &str, body: &str, file: Option<&Path>) -> Result<(), String> {
    let Some(key) = secrets::get("resend-api-key") else { return Err("no_mail".into()) };
    let mut payload = json!({
        "from": from,
        "to": to,
        "subject": subject,
        "text": if body.trim().is_empty() { " " } else { body },
    });
    if let Some(p) = file {
        let bytes = tokio::fs::read(p).await.map_err(|_| "file_missing".to_string())?;
        payload["attachments"] = json!([{ "filename": file_name(p), "content": crate::media::base64(&bytes) }]);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("ARIA")
        .build()
        .map_err(|e| format!("resend:{e}"))?;
    let r = client
        .post("https://api.resend.com/emails")
        .bearer_auth(key)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("resend:{e}"))?;
    if r.status().is_success() {
        Ok(())
    } else {
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        Err(format!("resend:HTTP {status} {}", text.chars().take(200).collect::<String>()))
    }
}

// ── Outlook ───────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod outlook {
    use super::*;

    /// Classic Outlook registers its automation class; the new one doesn't.
    pub fn installed() -> bool {
        use ::windows::core::w;
        use ::windows::Win32::System::Com::CLSIDFromProgID;
        unsafe { CLSIDFromProgID(w!("Outlook.Application")) }.is_ok()
    }

    /// Fixed scripts: what changes from one mail to the next comes in through
    /// the environment, never through the script's text.
    const SEND: &str = r#"
$ErrorActionPreference = 'Stop'
try { $o = New-Object -ComObject Outlook.Application } catch { [Console]::Out.Write('ERR no_outlook'); exit 2 }
try {
  $m = $o.CreateItem(0)
  foreach ($r in ($env:ARIA_MAIL_TO -split ';')) { if ($r) { [void]$m.Recipients.Add($r) } }
  if (-not $m.Recipients.ResolveAll()) { [Console]::Out.Write('ERR unresolved'); exit 3 }
  $m.Subject = $env:ARIA_MAIL_SUBJECT
  $m.Body = $env:ARIA_MAIL_BODY
  if ($env:ARIA_MAIL_FILE) { [void]$m.Attachments.Add($env:ARIA_MAIL_FILE) }
  $m.Send()
  try { $o.Session.SendAndReceive($false) } catch {}
  $outbox = $o.Session.GetDefaultFolder(4)
  for ($i = 0; $i -lt 40 -and $outbox.Items.Count -gt 0; $i++) { Start-Sleep -Milliseconds 500 }
  [Console]::Out.Write('OK')
} catch { [Console]::Out.Write('ERR ' + $_.Exception.Message); exit 1 }
"#;

    const CONTACTS: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$o = New-Object -ComObject Outlook.Application
$items = $o.Session.GetDefaultFolder(5).Items
$items.Sort('[SentOn]', $true)
$seen = @{}; $out = @(); $n = 0
foreach ($it in $items) {
  if ($n++ -ge 150 -or $out.Count -ge 30) { break }
  foreach ($r in $it.Recipients) {
    $a = $r.Address
    try { if ($r.AddressEntry.Type -eq 'EX') { $a = $r.AddressEntry.GetExchangeUser().PrimarySmtpAddress } } catch {}
    if ($a -and $a.Contains('@') -and -not $seen.ContainsKey($a.ToLower())) {
      $seen[$a.ToLower()] = 1
      $out += [pscustomobject]@{ name = [string]$r.Name; address = [string]$a }
    }
  }
}
ConvertTo-Json -InputObject @($out) -Compress
"#;

    fn encoded(script: &str) -> String {
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        crate::media::base64(&bytes)
    }

    fn powershell(script: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded(script)]);
        cmd.creation_flags(0x0800_0000);
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::null());
        cmd
    }

    pub async fn send(to: &[String], subject: &str, body: &str, file: Option<&Path>) -> Result<(), String> {
        let mut cmd = powershell(SEND);
        cmd.env("ARIA_MAIL_TO", to.join(";"))
            .env("ARIA_MAIL_SUBJECT", subject)
            .env("ARIA_MAIL_BODY", body)
            .env("ARIA_MAIL_FILE", file.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        let out = tokio::time::timeout(OUTLOOK_TIMEOUT, cmd.output())
            .await
            .map_err(|_| "outlook:timeout".to_string())?
            .map_err(|e| format!("outlook:{e}"))?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match said.as_str() {
            "OK" => Ok(()),
            "ERR no_outlook" => Err("no_outlook".into()),
            "ERR unresolved" => Err("unresolved".into()),
            other => Err(format!("outlook:{}", other.trim_start_matches("ERR ").chars().take(200).collect::<String>())),
        }
    }

    pub async fn recent_contacts() -> Vec<Contact> {
        if !tauri::async_runtime::spawn_blocking(installed).await.unwrap_or(false) {
            return Vec::new();
        }
        let Ok(Ok(out)) = tokio::time::timeout(OUTLOOK_TIMEOUT, powershell(CONTACTS).output()).await else {
            return Vec::new();
        };
        let mut found: Vec<Contact> = serde_json::from_slice(&out.stdout).unwrap_or_default();
        found.retain(|c| address(&c.address).is_some());
        found.truncate(CONTACTS_KEPT);
        found
    }
}

#[cfg(not(windows))]
mod outlook {
    use super::*;

    pub fn installed() -> bool {
        false
    }

    pub async fn send(_to: &[String], _subject: &str, _body: &str, _file: Option<&Path>) -> Result<(), String> {
        Err("no_outlook".into())
    }

    pub async fn recent_contacts() -> Vec<Contact> {
        Vec::new()
    }
}

// ── The assistant fills the card in ───────────────────────────────────────────

/// `compose_email`: ARIA prepares the card; sending stays the owner's click.
pub fn compose_spec() -> crate::providers::ToolSpec {
    crate::providers::ToolSpec {
        name: "compose_email".into(),
        description: "Open the island's email card filled in for the owner: recipients, subject, text and optionally a file to attach (absolute path, 20 MB max). Nothing is sent: the owner reads it and clicks Send. Use this whenever the owner asks to email something.".into(),
        schema: json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "One or more addresses, separated by commas." },
                "subject": { "type": "string" },
                "text": { "type": "string" },
                "path": { "type": "string", "description": "A file to attach." },
            },
            "required": ["to"],
        }),
    }
}

pub fn compose(app: &AppHandle, input: &serde_json::Value) -> crate::tools::Outcome {
    let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().trim().to_string();
    let path = s("path");
    let path = if path.is_empty() { None } else { Some(crate::tools::expand_path(&path).to_string_lossy().into_owned()) };
    if let Err(e) = attachment(path.as_deref()) {
        return crate::tools::Outcome::err(match e.as_str() {
            "file_too_big" => "That file is over 20 MB: too large to attach.",
            _ => "That file does not exist.",
        });
    }
    let to = s("to");
    crate::policy::audit("compose_email", "read", &format!("to {to} · {}", s("subject")));
    let _ = app.emit_to(
        crate::island::WINDOW_LABEL,
        "mail-compose",
        json!({ "to": to, "subject": s("subject"), "body": s("text"), "path": path }),
    );
    crate::tools::Outcome::ok("The email card is open in the island, filled in. Nothing is sent until the owner clicks Send there; tell them to check it.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_are_checked() {
        assert_eq!(recipients("a@x.com").unwrap(), vec!["a@x.com"]);
        assert_eq!(recipients(" Ana <ana@ejemplo.mx>; b@y.co , B@Y.CO").unwrap(), vec!["ana@ejemplo.mx", "b@y.co"]);
        assert_eq!(recipients("  ;, ").unwrap_err(), "no_recipient");
        assert_eq!(recipients("a@x.com, nope").unwrap_err(), "bad_address:nope");
        for bad in ["a@x", "@x.com", "a@.com", "a b@x.com", "a@x..com", "a@-x.com", "a\"@x.com"] {
            assert!(recipients(bad).is_err(), "{bad}");
        }
        assert!(recipients("millerquintero45@hotmail.com").is_ok());
    }

    #[test]
    fn the_attachment_must_exist_and_fit() {
        assert_eq!(attachment(None).unwrap(), None);
        assert_eq!(attachment(Some("  ")).unwrap(), None);
        assert_eq!(attachment(Some("C:/no/such/file.pdf")).unwrap_err(), "file_missing");
        let dir = std::env::temp_dir().join(format!("aria-mail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("a.txt");
        std::fs::write(&small, b"hola").unwrap();
        assert_eq!(attachment(Some(&small.to_string_lossy())).unwrap(), Some((small.clone(), 4)));
        assert_eq!(attachment(Some(&dir.to_string_lossy())).unwrap_err(), "file_missing", "a folder is not a file");
        let big = dir.join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_ATTACHMENT + 1).unwrap();
        drop(f);
        assert_eq!(attachment(Some(&big.to_string_lossy())).unwrap_err(), "file_too_big");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drafts_are_read_out_of_the_answer() {
        let d = parse_draft("```json\n{\"subject\": \"Informe\", \"body\": \"Hola, te envío el informe.\"}\n```").unwrap();
        assert_eq!(d, Draft { subject: "Informe".into(), body: "Hola, te envío el informe.".into() });
        assert_eq!(parse_draft("no json here"), None);
        assert_eq!(parse_draft("{\"subject\": \" \", \"body\": \"\"}"), None);
    }

    #[test]
    fn the_request_reads_the_card_shape() {
        let r: MailRequest = serde_json::from_value(json!({ "to": "a@x.com", "subject": "Hola" })).unwrap();
        assert_eq!((r.body.as_str(), r.path), ("", None));
    }
}
