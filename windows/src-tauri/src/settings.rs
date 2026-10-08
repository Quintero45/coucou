// Preferences, stored as plain JSON in settings.json under platform::config_dir().
// No secret ever lands here — API keys live in the OS keychain (see secrets.rs).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `default` at the struct level: a settings.json written by an older build is
/// missing the newer fields, and that must fill them in rather than throw the
/// whole file away.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    /// Claude model used by the chat. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
    /// The pill the island opens on and lists first.
    pub main_pill: String,
    /// Global keyboard shortcuts (see shortcuts.rs).
    pub shortcuts_enabled: bool,
    /// AI provider the assistant talks to (see providers.rs).
    pub provider: crate::providers::Provider,
    /// Model per provider other than Anthropic, which keeps `model`.
    pub provider_models: BTreeMap<String, String>,
    /// Base URL per local provider (Ollama, LM Studio).
    pub provider_urls: BTreeMap<String, String>,
    /// Let the assistant use tools (files, PowerShell, apps, MCP…).
    pub assistant_tools: bool,
    /// Claude Code (VS Code), Codex and Gemini CLI: pills, hooks and settings.
    pub show_agents: bool,
    /// The Cursor IDE agent: its pill, questions and hooks.
    pub show_cursor_agent: bool,
    /// The owner's Grok Bots, reached through their routines' webhooks.
    pub grok_bots: Vec<crate::grokbot::GrokBot>,
    /// Voice per Grok Bot: bot id -> voice id (`piper:es_MX-claude-high`…), see voice.rs.
    pub voices: BTreeMap<String, String>,
    /// Read the Cursor agent's events aloud (the island calls speak(text, "cursor")).
    #[serde(default = "default_true")]
    pub speak_cursor: bool,
    /// Read Grok bot replies aloud (frontend toggle "Leer respuestas en voz alta").
    #[serde(default = "default_true")]
    pub read_replies: bool,
}

fn default_true() -> bool {
    true
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

impl Settings {
    /// The model chosen for a provider, or its default.
    pub fn model_for(&self, provider: crate::providers::Provider) -> String {
        let chosen = match provider {
            crate::providers::Provider::Anthropic => Some(self.model.clone()),
            other => self.provider_models.get(other.id()).cloned(),
        };
        chosen
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| provider.info().model.to_string())
    }

    /// The base URL for a provider: the user's for local servers, else the fixed one.
    pub fn base_for(&self, provider: crate::providers::Provider) -> String {
        let info = provider.info();
        if info.local {
            if let Some(url) = self.provider_urls.get(provider.id()).filter(|u| !u.trim().is_empty()) {
                return url.trim().trim_end_matches('/').to_string();
            }
        }
        info.base.to_string()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            model: default_model(),
            main_pill: "integration_claude".into(),
            shortcuts_enabled: true,
            provider: Default::default(),
            provider_models: BTreeMap::new(),
            provider_urls: BTreeMap::new(),
            assistant_tools: true,
            show_agents: false,
            show_cursor_agent: true,
            grok_bots: Vec::new(),
            voices: BTreeMap::new(),
            speak_cursor: true,
            read_replies: true,
        }
    }
}

pub use crate::platform::{config_dir, local_dir};

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join(crate::platform::HOOK_EXE)
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    crate::platform::ensure_private_dir(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}
