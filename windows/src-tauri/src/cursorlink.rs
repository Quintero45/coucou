// Orders to the Cursor agent from its conversation in the island. Cursor has no
// API for the chat open in its window, so an order reaches it one of two ways:
// * Cursor working: the order waits here and goes out as the `followup_message`
//   of Cursor's next `stop` hook (aria-hook asks the pipe for it). The
//   keyboard is never touched.
// * Cursor idle: Cursor comes to the front, UI Automation puts the focus in its
//   chat box (class `aislash-editor-input`, never the code editor or the
//   terminal), and the text is typed there and sent. Nothing is typed unless
//   that box really holds the focus, and a draft of the owner's is never
//   added to.
// Either way the owner approves the exact text first (`gate`): the island's
// usual approval card, on Cursor's pill, before anything is queued or typed.
// A queued order carries its approval (`gate::Approved`), so Cursor's stop,
// which cannot wait for a click, only ever hands over approved text.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::grokbot::AttachmentIn;
use crate::island::WINDOW_LABEL;
use crate::log;

/// Cursor's default cap on automatic follow-ups per conversation (`loop_limit`).
const FOLLOWUP_LIMIT: u64 = 5;
const MAX_ORDER_CHARS: usize = 8_000;
/// Orders waiting for Cursor's next stop; past this the island types them or waits.
const MAX_QUEUED: usize = 20;

struct Order {
    id: String,
    /// Only ever text the owner approved on the card.
    text: gate::Approved,
}

static QUEUE: Mutex<Vec<Order>> = Mutex::new(Vec::new());

/// Why an order did not reach Cursor. `typed`: some of it may already be in
/// Cursor's chat box, so it must not be queued again (it would arrive twice).
pub struct Failure {
    pub msg: String,
    pub typed: bool,
}

impl From<String> for Failure {
    fn from(msg: String) -> Self {
        Failure { msg, typed: false }
    }
}

impl From<&str> for Failure {
    fn from(msg: &str) -> Self {
        Failure { msg: msg.to_string(), typed: false }
    }
}

#[derive(Serialize, Clone)]
struct OrderEvent<'a> {
    id: &'a str,
    /// "sent" (it reached Cursor) or "stuck" (Cursor stopped without taking it).
    state: &'a str,
}

fn emit(app: &AppHandle, id: &str, state: &str) {
    let _ = app.emit_to(WINDOW_LABEL, "cursor-order", OrderEvent { id, state });
}

/// Every attachment as a file on this PC that Cursor can open: dropped files are
/// already in the inbox; pasted text and images are written there.
fn attachment_paths(items: &[AttachmentIn]) -> Result<Vec<PathBuf>, String> {
    items
        .iter()
        .map(|a| match a {
            AttachmentIn::Inbox { id } => {
                crate::files::inbox_file(id).ok_or_else(|| "Un archivo adjunto ya no está; vuelve a soltarlo.".to_string())
            }
            AttachmentIn::Text { name, text, .. } => crate::files::save_to_inbox(name, text.as_bytes()),
            AttachmentIn::Base64 { name, base64, .. } => {
                let (clean, _) = crate::grokbot::clean_base64(base64).ok_or("Una imagen pegada no se pudo leer.")?;
                crate::files::save_to_inbox(name, &crate::grokbot::decode_base64(&clean))
            }
        })
        .collect()
}

/// A path as people write it: without the `\\?\` prefix canonicalised paths carry.
fn shown(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// The order as Cursor gets it, and the names of its files (for the card).
struct Composed {
    text: String,
    files: Vec<String>,
}

fn compose(app: &AppHandle, text: &str, screen: bool, attachments: &[AttachmentIn]) -> Result<Composed, String> {
    let mut out: String = text.trim().chars().take(MAX_ORDER_CHARS).collect();
    let paths = attachment_paths(attachments)?;
    let files = paths.iter().map(|p| p.file_name().map_or_else(|| shown(p), |n| n.to_string_lossy().into_owned())).collect();
    if !paths.is_empty() {
        let list: Vec<String> = paths.iter().map(|p| format!("- {}", shown(p))).collect();
        out.push_str(&format!("\n\n[Archivos adjuntos — ábrelos para verlos:\n{}]", list.join("\n")));
    }
    if screen {
        let path = crate::context::screen_png_file(app)?;
        out.push_str(&format!(
            "\n\n[Captura de mi pantalla al enviar esto: {} — ábrela para verla]",
            shown(&path)
        ));
    }
    Ok(Composed { text: out.trim().to_string(), files })
}

fn take(id: &str) -> Option<Order> {
    let mut q = QUEUE.lock().unwrap();
    let i = q.iter().position(|o| o.id == id)?;
    Some(q.remove(i))
}

async fn deliver_now(id: &str, approved: &gate::Approved) -> Result<(), Failure> {
    let text = approved.as_str().to_string();
    let chars = text.chars().count();
    let result = tauri::async_runtime::spawn_blocking(move || imp::type_order(&text))
        .await
        .map_err(|e| Failure::from(e.to_string()))?;
    match &result {
        Ok(()) => log::line(format!("cursor order {id} typed into Cursor's chat ({chars} chars)")),
        Err(e) => log::line(format!("cursor order {id} not typed (partly typed: {}): {}", e.typed, e.msg)),
    }
    result
}

/// An order from the island. `busy`: Cursor is working, so it waits for the
/// next `stop`; otherwise it is typed into Cursor's chat now. Either way only
/// after the owner approved the card; no answer or a no means nothing happens.
#[tauri::command]
pub async fn cursor_send(
    app: AppHandle,
    id: String,
    text: String,
    screen: bool,
    busy: bool,
    attachments: Option<Vec<AttachmentIn>>,
) -> Result<String, String> {
    let attachments = attachments.unwrap_or_default();
    if text.trim().is_empty() && attachments.is_empty() {
        return Err("La orden está vacía.".into());
    }
    if busy && QUEUE.lock().unwrap().len() >= MAX_QUEUED {
        return Err("Ya hay muchas órdenes en cola para Cursor. Espera a que termine o envía una ahora.".into());
    }
    let handle = app.clone();
    let full = tauri::async_runtime::spawn_blocking(move || compose(&handle, &text, screen, &attachments))
        .await
        .map_err(|e| e.to_string())??;
    let approved = gate::ask(&app, &id, full.text, &full.files, screen).await?;
    if busy {
        log::line(format!("cursor order {id} queued ({} chars, screen={screen})", approved.as_str().chars().count()));
        QUEUE.lock().unwrap().push(Order { id, text: approved });
        return Ok("queued".into());
    }
    deliver_now(&id, &approved).await.map_err(|f| f.msg)?;
    Ok("typed".into())
}

/// A queued order, typed into Cursor's chat right away: the text the owner
/// already approved when it was queued, and nothing else.
#[tauri::command]
pub async fn cursor_order_now(id: String) -> Result<(), String> {
    let order = take(&id).ok_or("Esa orden ya salió.")?;
    if let Err(f) = deliver_now(&id, &order.text).await {
        // Partly typed: it is in Cursor's chat box now; queued again it would
        // also go out with the next stop.
        if !f.typed {
            QUEUE.lock().unwrap().insert(0, order);
        }
        return Err(f.msg);
    }
    Ok(())
}

/// Err when the order is no longer queued (a stop already took it, or it is
/// being typed): the island must not say it was not sent.
#[tauri::command]
pub fn cursor_order_cancel(id: String) -> Result<bool, String> {
    if take(&id).is_none() {
        return Err("Esa orden ya salió hacia Cursor.".into());
    }
    log::line(format!("cursor order {id} cancelled"));
    Ok(true)
}

/// Orders taken for a `stop` reply; `settle` says whether they went out.
pub struct Followup {
    orders: Vec<Order>,
}

impl Followup {
    pub fn text(&self) -> String {
        join(&self.orders)
    }
}

/// Cursor's `stop` hook, through the pipe: every queued order as one follow-up
/// message. All of them were approved when queued; nothing waits for a click here. Cursor only takes it after a completed turn, under its loop limit;
/// otherwise the orders stay here and the island offers to type them.
pub fn followup(app: &AppHandle, payload: &Value) -> Option<Followup> {
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("completed");
    let loops = payload.get("loop_count").and_then(Value::as_u64).unwrap_or(0);
    let mut q = QUEUE.lock().unwrap();
    if q.is_empty() {
        return None;
    }
    if status != "completed" || loops >= FOLLOWUP_LIMIT {
        log::line(format!("cursor orders kept: stop status={status} loop_count={loops}"));
        for o in q.iter() {
            emit(app, &o.id, "stuck");
        }
        return None;
    }
    let orders: Vec<Order> = q.drain(..).collect();
    Some(Followup { orders })
}

/// After the reply to the stop: "sent" only once it was written to aria-hook;
/// otherwise (the hook gave up, Cursor killed it) the orders go back first in line.
pub fn settle(app: &AppHandle, f: Followup, written: bool) {
    if written {
        for o in &f.orders {
            emit(app, &o.id, "sent");
        }
        log::line(format!("cursor orders sent as a follow-up: {}", f.orders.len()));
        return;
    }
    log::line(format!("cursor follow-up not delivered, {} order(s) kept", f.orders.len()));
    for o in &f.orders {
        emit(app, &o.id, "stuck");
    }
    QUEUE.lock().unwrap().splice(0..0, f.orders);
}

fn join(orders: &[Order]) -> String {
    orders.iter().map(|o| o.text.as_str()).collect::<Vec<_>>().join("\n\n")
}

/// The owner's approval of an order, before it is queued or typed. The card is
/// the island's PermissionRequest card, raised and answered like a Grok Bot's
/// tool card (pipe.rs `approve_bot_tool`): same Pending / ack / decision path,
/// on Cursor's pill. "Siempre" makes a policy.rs allow rule for this tool and
/// this caller only (1 h, or the session). aria.log gets sizes, never the text.
mod gate {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use serde_json::{json, Value};
    use tauri::{AppHandle, Emitter, Manager};
    use tokio::sync::mpsc;

    use crate::island::WINDOW_LABEL;
    use crate::log;
    use crate::pipe::{Pending, Reply};
    use crate::policy;

    /// The tool on the card and in allow rules.
    pub const TOOL: &str = "Escribir en Cursor";
    /// The caller: the pill the card lands on, and the rules' `who`.
    pub const WHO: &str = "cursor";
    pub const DENIED: &str = "No se aprobó el envío a Cursor.";
    /// How much of the text the card shows; the click approves all of it.
    const SHOWN_CHARS: usize = 1_500;
    const SHOWN_FILES: usize = 10;
    /// As pipe.rs: the island acks at once, and drops the card after 110 s.
    const ACK_TIMEOUT: Duration = Duration::from_millis(800);
    const DECISION_TIMEOUT: Duration = Duration::from_secs(108);

    static COUNTER: AtomicU64 = AtomicU64::new(1);

    /// Text the owner approved for Cursor. Only `ask` makes one, so an order
    /// that did not get a yes can be neither queued, typed nor sent at a stop.
    pub struct Approved(String);

    impl Approved {
        pub fn as_str(&self) -> &str {
            &self.0
        }

        #[cfg(test)]
        pub fn for_test(text: &str) -> Self {
            Approved(text.to_string())
        }
    }

    #[derive(Debug, PartialEq)]
    enum Verdict {
        Once,
        Remember,
        No,
    }

    /// Only an explicit allow counts: deny, anything unexpected or no answer is no.
    fn verdict(decision: Option<&str>) -> Verdict {
        match decision {
            Some("allow") => Verdict::Once,
            Some("always" | "always-session") => Verdict::Remember,
            _ => Verdict::No,
        }
    }

    /// What allow rules and the audit log get: sizes, never the text.
    fn summary(text: &str, files: usize) -> String {
        format!("{} caracteres, {files} adjunto(s)", text.chars().count())
    }

    /// The card's text: the order (cut for display), then its attachments.
    fn shown(text: &str, files: &[String], screen: bool) -> String {
        let total = text.chars().count();
        let mut out: String = text.chars().take(SHOWN_CHARS).collect();
        if total > SHOWN_CHARS {
            out.push_str(&format!("… (+{} caracteres más; se aprueba el texto completo)", total - SHOWN_CHARS));
        }
        let mut extra = Vec::new();
        if !files.is_empty() {
            let mut names = files.iter().take(SHOWN_FILES).cloned().collect::<Vec<_>>().join(", ");
            if files.len() > SHOWN_FILES {
                names.push_str(&format!(", +{}", files.len() - SHOWN_FILES));
            }
            extra.push(format!("{} adjunto(s): {names}", files.len()));
        }
        if screen {
            extra.push("captura de pantalla".to_string());
        }
        if !extra.is_empty() {
            out.push_str(&format!("\n\n[{}]", extra.join(" · ")));
        }
        out
    }

    /// A PermissionRequest as the island renders it, on Cursor's pill.
    fn card(request_id: &str, shown: &str, chars: usize, files: &[String]) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "aria_agent": WHO,
            "message": format!("ARIA quiere escribir esto en el chat de Cursor ({chars} caracteres)"),
            "cwd": "",
            "tool_name": TOOL,
            // `prompt` is the field the island shows on the card's target line.
            "tool_input": { "prompt": shown, "chars": chars, "attachments": files },
            "request_id": request_id,
            "allow_always": !policy::never_always(TOOL),
        })
    }

    /// Waits for the owner's answer to the card for `text` (the full order).
    pub async fn ask(app: &AppHandle, order: &str, text: String, files: &[String], screen: bool) -> Result<Approved, String> {
        let what = summary(&text, files.len());
        if policy::allowed_by_rule(WHO, TOOL, &what) {
            policy::audit(TOOL, "allowed (rule)", &what);
            return Ok(Approved(text));
        }
        let id = format!("cursor-{}", COUNTER.fetch_add(1, Ordering::Relaxed));
        let (tx, mut rx) = mpsc::channel::<Reply>(4);
        app.state::<Pending>().0.lock().unwrap().insert(id.clone(), tx);
        log::line(format!("hook PermissionRequest id={id} cursor order {order} · {what}"));
        let _ = app.emit_to(WINDOW_LABEL, "hook", card(&id, &shown(&text, files, screen), text.chars().count(), files));

        let decision = wait(&mut rx).await;
        app.state::<Pending>().0.lock().unwrap().remove(&id);
        match verdict(decision.as_deref()) {
            Verdict::Once => {
                policy::audit(TOOL, "allowed", &what);
                Ok(Approved(text))
            }
            Verdict::Remember => {
                let made = policy::remember_always(WHO, TOOL, &what, decision.as_deref().unwrap_or_default());
                policy::audit(TOOL, if made { "allowed (rule added)" } else { "allowed (once; no rule for this tool)" }, &what);
                Ok(Approved(text))
            }
            Verdict::No => {
                policy::audit(TOOL, if decision.is_some() { "denied" } else { "not answered — denied" }, &what);
                Err(DENIED.into())
            }
        }
    }

    /// The card must be acknowledged at once (it is up), then answered in time.
    async fn wait(rx: &mut mpsc::Receiver<Reply>) -> Option<String> {
        match tokio::time::timeout(ACK_TIMEOUT, rx.recv()).await {
            Ok(Some(Reply::Ack)) => {}
            Ok(Some(Reply::Decision(d))) => return Some(d),
            _ => return None,
        }
        match tokio::time::timeout(DECISION_TIMEOUT, rx.recv()).await {
            Ok(Some(Reply::Decision(d))) => Some(d),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use std::time::Instant;

        use super::*;

        #[test]
        fn only_an_explicit_allow_approves() {
            assert_eq!(verdict(Some("allow")), Verdict::Once);
            assert_eq!(verdict(Some("always")), Verdict::Remember);
            assert_eq!(verdict(Some("always-session")), Verdict::Remember);
            for d in ["deny", "", "Allow", "allow ", r#"{"decision":"allow","answer":"x"}"#] {
                assert_eq!(verdict(Some(d)), Verdict::No, "{d}");
            }
            assert_eq!(verdict(None), Verdict::No, "timeout, busy or paused island");
        }

        #[test]
        fn the_card_is_a_permission_request_on_cursors_pill() {
            let files = vec!["a.pdf".to_string()];
            let c = card("cursor-1", "haz esto", 8, &files);
            assert_eq!(c["hook_event_name"], "PermissionRequest");
            assert_eq!(c["aria_agent"], "cursor");
            assert_eq!(c["tool_name"], "Escribir en Cursor");
            assert_eq!(c["request_id"], "cursor-1");
            assert_eq!(c["tool_input"]["prompt"], "haz esto");
            assert_eq!(c["tool_input"]["attachments"], json!(["a.pdf"]));
            assert_eq!(c["allow_always"], true);
            // No choices (any pick would be an allow) and no field the island
            // would show instead of the text.
            for k in ["options", "allowCustom", "allow_custom"] {
                assert!(c.get(k).is_none() && c["tool_input"].get(k).is_none(), "{k}");
            }
            for k in ["command", "file_path", "path", "url", "query", "pattern"] {
                assert!(c["tool_input"].get(k).is_none(), "{k}");
            }
        }

        #[test]
        fn the_card_shows_up_to_1500_chars_and_the_attachments() {
            let long = "é".repeat(2_000);
            let s = shown(&long, &[], false);
            assert!(s.starts_with(&"é".repeat(SHOWN_CHARS)));
            assert!(!s.starts_with(&"é".repeat(SHOWN_CHARS + 1)));
            assert!(s.contains("+500 caracteres"));
            assert_eq!(shown("corto", &[], false), "corto");
            let files: Vec<String> = (0..12).map(|i| format!("f{i}.png")).collect();
            let s = shown("mira", &files, true);
            assert!(s.starts_with("mira\n\n[12 adjunto(s): f0.png, "));
            assert!(s.contains("f9.png, +2 · captura de pantalla]"));
            assert!(!s.contains("f10.png"));
        }

        #[test]
        fn logs_and_rules_get_sizes_only() {
            let s = summary("borra todo con password=hunter2", 2);
            assert_eq!(s, "31 caracteres, 2 adjunto(s)");
        }

        #[test]
        fn a_rule_covers_this_tool_for_this_caller_only() {
            let now = Instant::now();
            let what = summary("x", 0);
            let rule = policy::rule_for(WHO, TOOL, &what, Some(policy::RULE_TTL)).expect("Siempre may be offered");
            assert!(policy::rule_covers(&rule, WHO, TOOL, "otra orden", now));
            assert!(!policy::rule_covers(&rule, "bot-cursor", TOOL, &what, now), "never across callers");
            assert!(!policy::rule_covers(&rule, policy::ARIA, TOOL, &what, now));
            assert!(!policy::rule_covers(&rule, WHO, "open_url", &what, now), "never another tool");
            assert!(!policy::rule_covers(&rule, WHO, TOOL, &what, now + policy::RULE_TTL + Duration::from_secs(1)), "expires");
            // Rules made for other tools or callers never cover this one.
            for (who, tool) in [(WHO, "open_url"), (WHO, "clipboard_write"), ("bot-a", TOOL), (policy::ARIA, TOOL)] {
                let other = policy::rule_for(who, tool, &what, None).unwrap();
                assert!(!policy::rule_covers(&other, WHO, TOOL, &what, now), "{who} {tool}");
            }
            // Shells still always ask.
            assert!(!policy::never_always(TOOL));
            assert!(policy::never_always("run_powershell"));
            assert!(policy::rule_for(WHO, "run_powershell", &what, None).is_none());
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn type_order(_text: &str) -> Result<(), super::Failure> {
        Err("Solo disponible en Windows.".into())
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationCondition, IUIAutomationElement, IUIAutomationValuePattern,
        TreeScope_Descendants, UIA_ClassNamePropertyId, UIA_ValuePatternId,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
        VIRTUAL_KEY, VK_MENU, VK_RETURN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
        SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    /// Cursor's chat box (a Lexical editor). The code editor is `inputarea`,
    /// the terminal `xterm-helper-textarea`.
    const CHAT_INPUT: &str = "aislash-editor-input";
    /// Keystrokes per SendInput call; the focus is checked between batches.
    const BATCH: usize = 240;

    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let found = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        if unsafe { IsWindowVisible(hwnd) }.as_bool() && unsafe { GetWindowTextLengthW(hwnd) } > 0 {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != 0 && crate::appwatch::exe_name(pid).is_some_and(|e| e.eq_ignore_ascii_case("cursor.exe")) {
                found.push(hwnd);
            }
        }
        true.into()
    }

    /// Cursor's windows, the most recently active first (EnumWindows walks the Z order).
    fn cursor_windows() -> Vec<HWND> {
        let mut found: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut found as *mut Vec<HWND> as isize));
        }
        found
    }

    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    fn chat_input(uia: &IUIAutomation, hwnd: HWND, cond: &IUIAutomationCondition) -> Option<IUIAutomationElement> {
        unsafe {
            let root = uia.ElementFromHandle(hwnd).ok()?;
            let all = root.FindAll(TreeScope_Descendants, cond).ok()?;
            (0..all.Length().ok()?)
                .filter_map(|i| all.GetElement(i).ok())
                .find(|e| e.CurrentIsOffscreen().is_ok_and(|off| !off.as_bool()))
        }
    }

    fn focused_is_chat(uia: &IUIAutomation) -> bool {
        unsafe { uia.GetFocusedElement().and_then(|e| e.CurrentClassName()) }
            .is_ok_and(|c| c.to_string().split_whitespace().any(|k| k == CHAT_INPUT))
    }

    fn has_draft(input: &IUIAutomationElement) -> bool {
        unsafe { input.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId).and_then(|p| p.CurrentValue()) }
            .is_ok_and(|v| v.to_string().chars().any(|c| !c.is_whitespace() && c != '\u{200b}' && c != '\u{feff}'))
    }

    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    fn unit(u: u16, up: bool) -> INPUT {
        let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: u, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        }
    }

    fn send(inputs: &[INPUT]) -> bool {
        unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
    }

    fn bring_to_front(hwnd: HWND) {
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
                // Windows only lets the last input's owner change the foreground: a bare Alt tap counts.
                send(&[key(VK_MENU, false), key(VK_MENU, true)]);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    /// Line breaks go in as Shift+Enter (Enter alone would send); tabs as spaces.
    fn keystrokes(text: &str) -> Vec<INPUT> {
        let mut out = Vec::with_capacity(text.len() * 2);
        for ch in text.chars() {
            match ch {
                '\r' => {}
                '\n' => out.extend([key(VK_SHIFT, false), key(VK_RETURN, false), key(VK_RETURN, true), key(VK_SHIFT, true)]),
                '\t' => out.extend([unit(' ' as u16, false), unit(' ' as u16, true)]),
                c => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        out.extend([unit(*u, false), unit(*u, true)]);
                    }
                }
            }
        }
        out
    }

    pub fn type_order(text: &str) -> Result<(), super::Failure> {
        let windows = cursor_windows();
        if windows.is_empty() {
            return Err("Cursor no está abierto.".into());
        }
        // A pool thread another caller left in an STA can still use COM; it is
        // not ours to uninitialize then.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let _com = if hr == RPC_E_CHANGED_MODE {
            None
        } else {
            hr.ok().map_err(|e| format!("COM: {e}"))?;
            Some(Com)
        };
        let uia: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.map_err(|e| format!("UI Automation: {e}"))?;
        let cond = unsafe { uia.CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(CHAT_INPUT)) }
            .map_err(|e| format!("UI Automation: {e}"))?;

        // The first query wakes Chromium's accessibility tree, so it may come back empty.
        let mut target = None;
        'find: for attempt in 0..4u64 {
            for &hwnd in &windows {
                if let Some(input) = chat_input(&uia, hwnd, &cond) {
                    target = Some((hwnd, input));
                    break 'find;
                }
            }
            std::thread::sleep(Duration::from_millis(250 * (attempt + 1)));
        }
        let (hwnd, input) = target.ok_or("No encontré el chat de Cursor. Ábrelo en su ventana (Ctrl+L) y vuelve a enviar.")?;

        bring_to_front(hwnd);
        unsafe { input.SetFocus() }.map_err(|e| format!("No pude enfocar el chat de Cursor: {e}"))?;
        std::thread::sleep(Duration::from_millis(150));
        if !focused_is_chat(&uia) {
            return Err("No pude poner el foco en el chat de Cursor, así que no escribí nada.".into());
        }
        if has_draft(&input) {
            return Err("El chat de Cursor tiene un mensaje a medio escribir. Envíalo o bórralo y vuelve a intentarlo.".into());
        }

        let partial = |msg: &str| super::Failure { msg: msg.to_string(), typed: true };
        let mut typed = false;
        for batch in keystrokes(text).chunks(BATCH) {
            if !focused_is_chat(&uia) {
                if !typed {
                    return Err("El foco salió del chat de Cursor antes de escribir. La orden no se envió.".into());
                }
                return Err(partial("El foco salió del chat de Cursor mientras escribía. Revisa su chat: la orden quedó a medias y no se envió."));
            }
            if !send(batch) {
                return Err(partial("Windows no dejó escribir en Cursor. Revisa su chat: la orden no se envió."));
            }
            typed = true;
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(120));
        if !focused_is_chat(&uia) {
            return Err(partial("El foco salió del chat de Cursor antes de enviar. La orden quedó escrita allí, sin enviar."));
        }
        if !send(&[key(VK_RETURN, false), key(VK_RETURN, true)]) {
            return Err(partial("Windows no dejó pulsar Enter en Cursor. La orden quedó escrita allí, sin enviar."));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn line_breaks_become_shift_enter_and_text_is_unicode() {
            let k = keystrokes("a\nñ");
            assert_eq!(k.len(), 2 + 4 + 2);
            let ki = |i: usize| unsafe { k[i].Anonymous.ki };
            assert_eq!(ki(0).wScan, 'a' as u16);
            assert_eq!(ki(2).wVk, VK_SHIFT);
            assert_eq!(ki(3).wVk, VK_RETURN);
            assert_eq!(ki(6).wScan, 'ñ' as u16);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_lose_the_verbatim_prefix() {
        assert_eq!(shown(std::path::Path::new(r"\\?\C:\Users\a.pdf")), r"C:\Users\a.pdf");
        assert_eq!(shown(std::path::Path::new(r"C:\b.png")), r"C:\b.png");
    }

    #[test]
    fn orders_join_with_a_blank_line() {
        let orders = vec![
            Order { id: "1".into(), text: gate::Approved::for_test("uno") },
            Order { id: "2".into(), text: gate::Approved::for_test("dos") },
        ];
        assert_eq!(join(&orders), "uno\n\ndos");
    }

    #[test]
    fn a_stop_hands_over_exactly_the_approved_text() {
        let f = Followup { orders: vec![Order { id: "1".into(), text: gate::Approved::for_test("aprobada") }] };
        assert_eq!(f.text(), "aprobada");
        assert_eq!(gate::DENIED, "No se aprobó el envío a Cursor.");
    }
}
