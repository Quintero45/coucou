// Coucou for Windows — app wiring and the commands the island calls.

mod agent;
mod claude;
mod cursor;
mod files;
mod grokbot;
mod hooks;
mod integrations;
mod island;
mod log;
mod mcp;
mod memory;
mod pipe;
mod platform;
mod policy;
mod providers;
mod secrets;
mod selfmod;
mod settings;
mod shortcuts;
mod tools;
mod tray;

use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::{ManagerExt, MacosLauncher};

use agent::{Assistant, ChatReply};
use claude::ChatContext;
use files::DroppedFile;
use hooks::{HookOptions, HookPreview, HookStatus, HookTarget};
use island::{PollGate, ScreenInfo};
use pipe::Pending;
use settings::Settings;

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub gate: Arc<PollGate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootInfo {
    settings: Settings,
    screen: ScreenInfo,
    version: String,
    hook_path: String,
    /// False where the OS has no global cursor (Wayland): the page then reports
    /// the cursor from its own mouse events.
    cursor_poll: bool,
}

#[tauri::command]
fn boot(app: AppHandle, shared: State<Shared>) -> BootInfo {
    let mut settings = shared.settings.lock().unwrap().clone();
    // The real state of ~/.claude/settings.json wins over whatever we stored.
    settings.hooks_installed = hooks::status(HookTarget::Claude).installed;
    let screen = island::screen_info(&app, &settings.screen);
    BootInfo {
        settings,
        screen,
        version: env!("CARGO_PKG_VERSION").to_string(),
        hook_path: settings::hook_exe_path().to_string_lossy().to_string(),
        cursor_poll: platform::CURSOR_POLL,
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, shared: State<Shared>, settings: Settings) {
    let (screen_changed, autostart_changed, shortcuts_changed) = {
        let mut current = shared.settings.lock().unwrap();
        let screen_changed = current.screen != settings.screen;
        let autostart_changed = current.autostart != settings.autostart;
        let shortcuts_changed = current.shortcuts_enabled != settings.shortcuts_enabled;
        *current = settings.clone();
        (screen_changed, autostart_changed, shortcuts_changed)
    };
    if shortcuts_changed {
        shortcuts::apply(&app, settings.shortcuts_enabled);
    }
    if let Err(err) = settings::save(&settings) {
        eprintln!("[coucou] could not save settings: {err}");
    }
    if autostart_changed {
        let manager = app.autolaunch();
        let result = if settings.autostart { manager.enable() } else { manager.disable() };
        if let Err(err) = result {
            eprintln!("[coucou] autostart: {err}");
        }
    }
    if screen_changed {
        let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
        island::apply_geometry(&app, &settings.screen, collapsed);
    }
    // Keep the other window in step (island ⇄ settings window).
    let _ = app.emit("settings-changed", settings);
}

/// Hidden island → shrink the window to the invisible wake strip and park the
/// cursor poll; anything else → full panel and 60 Hz polling.
#[tauri::command]
fn set_collapsed(app: AppHandle, shared: State<Shared>, collapsed: bool) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    shared.gate.collapsed.store(collapsed, Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
    // The wake strip must always take the mouse, and a resize invalidates the flag.
    island::refresh_click_through(&app, &shared.gate);
    shared.gate.set_active(!collapsed);
}

/// The front end pushes the island shape; Rust decides click-through from it.
#[tauri::command]
fn set_island_rect(app: AppHandle, shared: State<Shared>, x: f64, y: f64, width: f64, height: f64) {
    shared.gate.set_rect(island::IslandRect { x, y, w: width, h: height });
    // Without the cursor poll the input region is the click-through: it follows the island.
    if !platform::CURSOR_POLL {
        island::refresh_click_through(&app, &shared.gate);
    }
}

#[tauri::command]
fn focus_window(app: AppHandle, focused: bool) {
    let Some(win) = island::window(&app) else { return };
    platform::set_activating(&win, focused);
    if focused {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn reposition(app: AppHandle, shared: State<Shared>) {
    let pref = shared.settings.lock().unwrap().screen.clone();
    let collapsed = shared.gate.collapsed.load(Ordering::Relaxed);
    island::apply_geometry(&app, &pref, collapsed);
}

#[tauri::command]
fn open_url(url: String) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    platform::open_url(&url);
}

/// "Open terminal" opens the working folder in VS Code when `code` is on PATH,
/// and falls back to the file manager otherwise.
#[tauri::command]
fn open_in_vscode(path: Option<String>) -> bool {
    // No shell anywhere near this. The path is a project folder chosen by
    // whoever is using Claude Code, and a shell would happily read `&`, `^`, `%`
    // or `$` in a folder name as syntax. Finding the launcher ourselves and
    // handing the path over as a separate argument keeps it a path.
    let path = path.filter(|p| !p.is_empty());
    // It arrives in a hook payload: only an existing folder, given by its full
    // path, goes any further. `code` would read `--something` as an option, and
    // xdg-open would launch a file with whatever handles its type.
    if let Some(p) = path.as_deref() {
        let p = std::path::Path::new(p);
        if !(p.is_absolute() && p.is_dir()) {
            return false;
        }
    }
    if let Some(code) = platform::find_on_path("code") {
        let mut cmd = Command::new(code);
        if let Some(p) = path.as_deref() {
            cmd.arg(p);
        }
        if platform::no_console(&mut cmd).spawn().is_ok() {
            return true;
        }
    }
    if let Some(p) = path.as_deref() {
        platform::reveal_folder(p);
    }
    false
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

/// Tray → Pause. Paused means paused: the pollers stop talking to the network,
/// not just the island stopping showing things.
#[tauri::command]
fn set_paused(paused: bool) {
    integrations::set_paused(paused);
}

// ── Agent hooks (Claude Code, Cursor, Codex, Gemini CLI) ──────────────────────

#[tauri::command]
fn hooks_status(target: Option<HookTarget>) -> HookStatus {
    hooks::status(target.unwrap_or_default())
}

/// Returns the diff the user has to look at before anything is written.
#[tauri::command]
fn hooks_preview(
    target: Option<HookTarget>,
    install: bool,
    options: Option<HookOptions>,
) -> Result<HookPreview, String> {
    hooks::preview(target.unwrap_or_default(), install, options.unwrap_or_default())
}

/// Only ever called from an explicit click in the settings window.
#[tauri::command]
fn hooks_apply(
    app: AppHandle,
    shared: State<Shared>,
    target: Option<HookTarget>,
    install: bool,
    fingerprint: String,
    options: Option<HookOptions>,
) -> Result<String, String> {
    let target = target.unwrap_or_default();
    // The fingerprint comes from the preview the user actually looked at, so a
    // config file that changed in between is refused rather than overwritten.
    let backup = hooks::write(target, install, &fingerprint, options.unwrap_or_default())?;
    if target == HookTarget::Claude {
        let updated = {
            let mut current = shared.settings.lock().unwrap();
            current.hooks_installed = install;
            let _ = settings::save(&current);
            current.clone()
        };
        let _ = app.emit("settings-changed", updated);
    }
    Ok(backup)
}

#[tauri::command]
fn approval_decision(app: AppHandle, request_id: String, decision: String) {
    pipe::answer(&app, &request_id, &decision);
}

/// AskUserQuestion answered from the island: `{ question text: label(s) }`.
#[tauri::command]
fn question_answer(app: AppHandle, request_id: String, answers: serde_json::Value) {
    pipe::answer_questions(&app, &request_id, answers);
}

/// The island has the card on screen, so the long wait for a human may begin.
/// Until this arrives the relay only waits a few hundred milliseconds, which is
/// what stops a paused or unresponsive island from freezing Claude Code.
#[tauri::command]
fn approval_ack(app: AppHandle, request_id: String) {
    pipe::acknowledge(&app, &request_id);
}

/// Nobody can act on this request — the island is paused, or another card is
/// already up. Claude Code falls back to asking in the terminal immediately.
#[tauri::command]
fn approval_decline(app: AppHandle, request_id: String) {
    pipe::decline(&app, &request_id);
}

// ── Chat, files and secrets ───────────────────────────────────────────────────

/// One chat turn with the assistant. Keys and file bytes stay on the Rust side;
/// text streams back as `chat-delta` events, tools as `chat-step`.
#[tauri::command]
async fn chat_send(app: AppHandle, query: String, context: Option<ChatContext>) -> Result<ChatReply, String> {
    agent::send(app, query, context).await
}

#[tauri::command]
fn chat_reset(assistant: State<Assistant>) {
    assistant.reset();
}

#[tauri::command]
fn chat_stop(assistant: State<Assistant>) {
    assistant.stop();
}

/// Models a provider offers, for the picker in settings.
#[tauri::command]
async fn models_list(app: AppHandle, provider: providers::Provider) -> Result<Vec<String>, String> {
    agent::models(&app, provider).await
}

// ── Grok Bots ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn grokbot_list(app: AppHandle) -> Vec<grokbot::BotStatus> {
    grokbot::statuses(&app)
}

/// Only from the settings window: the owner typed this Bot in.
#[tauri::command]
fn grokbot_save(
    app: AppHandle,
    previous: Option<String>,
    name: String,
    color: String,
    url: String,
    key: String,
) -> Result<grokbot::GrokBot, String> {
    grokbot::save(&app, previous.as_deref(), &name, &color, &url, &key)
}

#[tauri::command]
fn grokbot_remove(app: AppHandle, id: String) -> Result<(), String> {
    grokbot::remove(&app, &id)
}

/// The owner typed the task and pressed send: that is the click.
#[tauri::command]
async fn grokbot_send(app: AppHandle, bot: String, message: String) -> Result<String, String> {
    grokbot::send(&app, &bot, &message).await
}

#[tauri::command]
fn grokbot_instructions(app: AppHandle, id: String) -> Result<String, String> {
    grokbot::instructions(&app, &id)
}

// ── Cursor engine ─────────────────────────────────────────────────────────────

#[tauri::command]
fn cursor_status() -> cursor::Status {
    cursor::status()
}

/// The owner pressed "Instalar motor" in settings.
#[tauri::command]
async fn cursor_install() -> Result<String, String> {
    cursor::install().await
}

// ── Connections (MCP), skills, core guard ─────────────────────────────────────

#[tauri::command]
fn mcp_list() -> Vec<mcp::ServerStatus> {
    mcp::statuses()
}

#[tauri::command]
fn mcp_config() -> mcp::Config {
    mcp::load_config()
}

/// Only from the settings window: the owner typed this server in.
#[tauri::command]
fn mcp_save(
    app: AppHandle,
    name: String,
    config: mcp::ServerConfig,
    secrets: Option<std::collections::BTreeMap<String, String>>,
) -> Result<(), String> {
    mcp::save_server(&app, &name, config, secrets.unwrap_or_default())
}

#[tauri::command]
fn mcp_remove(app: AppHandle, name: String) -> Result<(), String> {
    mcp::remove_server(&app, &name)
}

#[tauri::command]
fn mcp_set_enabled(app: AppHandle, name: String, enabled: bool) -> Result<(), String> {
    mcp::set_enabled(&app, &name, enabled)
}

#[tauri::command]
fn mcp_import_preview() -> Vec<mcp::ImportCandidate> {
    mcp::import_preview()
}

#[tauri::command]
fn mcp_import_apply(app: AppHandle, names: Vec<String>) -> Result<usize, String> {
    mcp::import_apply(&app, &names)
}

#[tauri::command]
fn mcp_reconnect(app: AppHandle) {
    mcp::start(app);
}

#[tauri::command]
fn skills_list() -> Vec<selfmod::skills::Skill> {
    selfmod::skills::list()
}

#[tauri::command]
fn skill_set_enabled(app: AppHandle, name: String, enabled: bool) -> Result<(), String> {
    selfmod::skills::set_enabled(&app, &name, enabled)
}

#[tauri::command]
fn skill_remove(app: AppHandle, name: String) -> Result<(), String> {
    selfmod::skills::remove(&app, &name)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CoreStatus {
    protected: Vec<String>,
    tampered: Vec<String>,
}

#[tauri::command]
fn core_status() -> CoreStatus {
    CoreStatus {
        protected: selfmod::guard::PROTECTED.iter().map(|s| s.to_string()).collect(),
        tampered: selfmod::guard::tampered().to_vec(),
    }
}

/// Opens one of Mochi's own folders in Explorer.
#[tauri::command]
fn open_data_folder(which: String) {
    let dir = match which.as_str() {
        "skills" => selfmod::skills::dir(),
        "memory" => memory::dir(),
        "log" => settings::local_dir(),
        _ => settings::config_dir(),
    };
    let _ = std::fs::create_dir_all(&dir);
    platform::reveal_folder(&dir.to_string_lossy());
}

/// Copies a dropped file into the inbox and reports its name back.
#[tauri::command]
fn ingest_file(path: String) -> Result<DroppedFile, String> {
    files::ingest(&path)
}

/// The island may only ask whether a key exists — never read it.
#[tauri::command]
fn secret_present(key: String) -> bool {
    secrets::present(&key)
}

#[tauri::command]
fn secret_set(key: String, value: String) -> Result<(), String> {
    secrets::set(&key, &value)
}

#[tauri::command]
fn secret_clear(key: String) -> Result<(), String> {
    secrets::clear(&key)
}

/// Opens the configured n8n instance — the URL lives in the Credential Manager.
#[tauri::command]
fn open_n8n() {
    if let Some(url) = secrets::get("n8n-url") {
        open_url(url);
    }
}

/// Refresh buttons in the integration cards.
#[tauri::command]
async fn refresh_integration(app: AppHandle, id: String) {
    integrations::poll_once(app, &id).await;
}

/// Lets the island write to the same log as the Rust side.
#[tauri::command]
fn log_line(message: String) {
    log::line(format!("ui  {message}"));
}

// ── Settings window ───────────────────────────────────────────────────────────

/// WebView2 allows exactly one browser environment per app, and its options are
/// fixed by whichever webview is created first. Every window must therefore ask
/// for the *same* arguments as the island (see `additionalBrowserArgs` in
/// tauri.conf.json) — a mismatch makes the second window come up blank, with no
/// error anywhere.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// In a dev build the pages are served by Vite, so the second window needs the
/// absolute dev URL; a bundled build resolves it inside the app bundle.
fn settings_page_url(app: &AppHandle) -> WebviewUrl {
    #[cfg(dev)]
    if let Some(mut base) = app.config().build.dev_url.clone() {
        base.set_path("/settings.html");
        return WebviewUrl::External(base);
    }
    let _ = app;
    WebviewUrl::App("settings.html".into())
}

/// The settings window is created hidden at launch and only ever shown and
/// hidden afterwards. A WebView2 window created later — on the main thread or
/// not — silently comes up blank in this app, so the window that works is the
/// one that exists before the island's webview does.
fn create_settings_window(app: &AppHandle) {
    let url = settings_page_url(app);
    match WebviewWindowBuilder::new(app, "settings", url)
        .additional_browser_args(BROWSER_ARGS)
        .title("Ajustes — Coucou")
        .inner_size(560.0, 680.0)
        .min_inner_size(460.0, 480.0)
        .resizable(true)
        .visible(false)
        .center()
        .build()
    {
        Ok(win) => {
            // Closing it must only hide it, or it could never be reopened.
            let hidden = win.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hidden.hide();
                }
            });
        }
        Err(err) => log::line(format!("settings window failed: {err}")),
    }
}

pub fn show_settings_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("settings") else {
        log::line("settings window missing");
        return;
    };
    let _ = win.unminimize();
    let _ = win.show();
    let _ = win.set_focus();
}

#[tauri::command]
fn open_settings_window(app: AppHandle) {
    show_settings_window(&app);
}

pub fn run() {
    platform::prepare_environment();
    let loaded = settings::load();
    let gate = Arc::new(PollGate::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            let _ = app.emit_to(island::WINDOW_LABEL, "tray", "open".to_string());
        }))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(shortcuts::plugin())
        .manage(Shared {
            settings: Mutex::new(loaded.clone()),
            gate: gate.clone(),
        })
        .manage(Pending::default())
        .manage(Assistant::default())
        .invoke_handler(tauri::generate_handler![
            boot,
            save_settings,
            set_collapsed,
            set_island_rect,
            focus_window,
            reposition,
            open_url,
            open_in_vscode,
            quit_app,
            hooks_status,
            hooks_preview,
            hooks_apply,
            approval_decision,
            question_answer,
            approval_ack,
            approval_decline,
            log_line,
            chat_send,
            chat_reset,
            chat_stop,
            models_list,
            grokbot_list,
            grokbot_save,
            grokbot_remove,
            grokbot_send,
            grokbot_instructions,
            cursor_status,
            cursor_install,
            mcp_list,
            mcp_config,
            mcp_save,
            mcp_remove,
            mcp_set_enabled,
            mcp_import_preview,
            mcp_import_apply,
            mcp_reconnect,
            skills_list,
            skill_set_enabled,
            skill_remove,
            core_status,
            open_data_folder,
            ingest_file,
            secret_present,
            secret_set,
            secret_clear,
            refresh_integration,
            open_n8n,
            open_settings_window,
            set_paused,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            // Before the island: see create_settings_window.
            create_settings_window(&handle);

            if let Some(win) = island::window(&handle) {
                platform::make_non_activating(&win);
                island::apply_geometry(&handle, &loaded.screen, false);
                let _ = win.show();
            }
            gate.collapsed.store(false, Ordering::Relaxed);
            // Nothing drawn yet, so nothing takes the mouse until the page
            // reports the island's shape.
            if !platform::CURSOR_POLL {
                island::refresh_click_through(&handle, &gate);
            }
            gate.set_active(true);
            island::spawn_cursor_poll(handle.clone(), gate.clone());

            log::line(format!("--- Coucou {} started ---", env!("CARGO_PKG_VERSION")));
            selfmod::guard::verify_at_startup();
            selfmod::evolve::startup_health();
            hooks::ensure_hook_exe(&handle);
            mcp::start(handle.clone());
            cursor::init(handle.clone());
            memory::ensure();
            shortcuts::apply(&handle, loaded.shortcuts_enabled);
            pipe::start(handle.clone());
            integrations::start(handle.clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Coucou");
}
