// The Cursor engine: Grok (or any model on the owner's Cursor plan) answering
// as Mochi, with Mochi's own tools.
//
// Cursor has no plain chat API; its SDK drives agents through a local bridge
// process (github.com/cursor/sdk-bridge) that speaks Connect JSON on loopback.
// Coucou downloads the bridge on a click in settings (checked against the
// release's SHA256SUMS), starts it with the first message and stops it after
// IDLE_STOP without use. One local agent per conversation, with Cursor's own
// tools switched off (`tools: []`): Mochi's tools are offered as custom tools,
// which the bridge calls back on a loopback server here. Custom tools skip the
// SDK's approval, so policy.rs (through tools::run) is the only gate — the
// same one every other provider goes through.
//
// The bridge has no system prompt option: the directive and Mochi's prompt go
// in front of the first message of each conversation.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::claude::{self, ChatContext};
use crate::island::WINDOW_LABEL;
use crate::providers::{Endpoint, Part, ToolCall, ToolSpec};
use crate::selfmod::guard::sha256::sha256_hex;
use crate::{log, platform, settings, tools};

/// "Pick the newest Grok on the account" — the default model.
pub const AUTO_MODEL: &str = "grok";

const RELEASES: &str = "https://api.github.com/repos/cursor/sdk-bridge/releases/latest";
const READY_PREFIX: &str = "cursor-sdk-bridge ready ";
const READY_TIMEOUT: Duration = Duration::from_secs(40);
const IDLE_STOP: Duration = Duration::from_secs(15 * 60);
const RPC_TIMEOUT: Duration = Duration::from_secs(90);
const CALLBACK_PATH: &str = "/sdk.v1.SdkCustomToolCallbackService/CallCustomTool";
const MAX_CALLBACK_BODY: usize = 8 * 1024 * 1024;

#[cfg(windows)]
const ASSET: &str = "cursor-sdk-bridge-standalone-win32-x64.tar.gz";
#[cfg(not(windows))]
const ASSET: &str = "cursor-sdk-bridge-standalone-linux-x64.tar.gz";
#[cfg(windows)]
const EXE: &str = "cursor-sdk-bridge.exe";
#[cfg(not(windows))]
const EXE: &str = "cursor-sdk-bridge";

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

fn root() -> PathBuf {
    settings::local_dir().join("cursor-sdk-bridge")
}

fn package() -> PathBuf {
    root().join("pkg")
}

fn exe() -> PathBuf {
    package().join("bin").join(EXE)
}

fn client() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .user_agent("Coucou")
            .connect_timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default()
    });
    &CLIENT
}

// ── Install ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    installed: bool,
    version: Option<String>,
}

pub fn status() -> Status {
    let version = std::fs::read(package().join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|m| m["sdkVersion"].as_str().map(|v| format!("v{v}")));
    Status { installed: exe().is_file(), version }
}

async fn download(url: &str) -> Result<Vec<u8>, String> {
    let response = client()
        .get(url)
        .timeout(Duration::from_secs(900))
        .send()
        .await
        .map_err(|e| format!("No se pudo descargar el motor: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub respondió {} al descargar el motor.", response.status()));
    }
    response.bytes().await.map(|b| b.to_vec()).map_err(|e| format!("Descarga interrumpida: {e}"))
}

fn expected_hash(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, name) = (parts.next()?, parts.next()?);
        (name.trim_start_matches('*') == asset).then(|| hash.to_ascii_lowercase())
    })
}

fn tar_command() -> std::process::Command {
    // System32's bsdtar, not whatever `tar` is first on PATH (Git's reads C: as a host).
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        std::process::Command::new(system.join("System32").join("tar.exe"))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("tar")
    }
}

/// Downloads the latest bridge release, verifies it and swaps it in.
pub async fn install() -> Result<String, String> {
    let release: Value = client()
        .get(RELEASES)
        .timeout(RPC_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("No se pudo consultar GitHub: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Respuesta inesperada de GitHub: {e}"))?;
    let tag = release["tag_name"].as_str().unwrap_or("?").to_string();
    let url_of = |name: &str| {
        release["assets"]
            .as_array()
            .and_then(|list| list.iter().find(|a| a["name"].as_str() == Some(name)))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let asset_url = url_of(ASSET).ok_or("La última versión del motor no trae paquete para este sistema.")?;
    let sums_url = url_of("SHA256SUMS.txt").ok_or("La versión no publica SHA256SUMS.txt; no se instala sin verificar.")?;

    let sums = String::from_utf8_lossy(&download(&sums_url).await?).to_string();
    let expected = expected_hash(&sums, ASSET).ok_or("SHA256SUMS.txt no lista el paquete de este sistema.")?;
    let bytes = download(&asset_url).await?;
    let (actual, bytes) = tokio::task::spawn_blocking(move || (sha256_hex(&bytes), bytes))
        .await
        .map_err(|e| e.to_string())?;
    if actual != expected {
        log::line(format!("cursor engine: checksum mismatch for {tag}"));
        return Err("La descarga no coincide con la suma SHA-256 publicada por Cursor. No se instaló nada.".into());
    }

    stop().await;
    let root = root();
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let archive = root.join("download.tar.gz");
    tokio::fs::write(&archive, &bytes).await.map_err(|e| e.to_string())?;
    drop(bytes);
    let fresh = root.join("pkg-new");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
    let mut tar = tar_command();
    tar.arg("-xzf").arg(&archive).arg("-C").arg(&fresh);
    platform::no_console(&mut tar);
    let out = tokio::process::Command::from(tar).output().await;
    let _ = std::fs::remove_file(&archive);
    let out = out.map_err(|e| format!("No se pudo descomprimir el motor: {e}"))?;
    if !out.status.success() || !fresh.join("bin").join(EXE).is_file() {
        let _ = std::fs::remove_dir_all(&fresh);
        let why: String = String::from_utf8_lossy(&out.stderr).chars().take(200).collect();
        return Err(format!("No se pudo descomprimir el motor. {why}"));
    }
    let _ = std::fs::remove_dir_all(package());
    std::fs::rename(&fresh, package()).map_err(|e| format!("No se pudo instalar el motor: {e}"))?;
    log::line(format!("cursor engine: installed {tag}"));
    Ok(tag)
}

// ── The bridge process ────────────────────────────────────────────────────────

struct Proc {
    url: String,
    token: String,
    child: tokio::process::Child,
}

static BRIDGE: LazyLock<tokio::sync::Mutex<Option<Proc>>> = LazyLock::new(|| tokio::sync::Mutex::new(None));
static LAST_USE: Mutex<Option<Instant>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);

fn touch() {
    *LAST_USE.lock().unwrap() = Some(Instant::now());
}

/// The running bridge's URL and bearer token, starting it if needed.
async fn bridge() -> Result<(String, String), String> {
    let mut slot = BRIDGE.lock().await;
    touch();
    if let Some(p) = slot.as_mut() {
        if matches!(p.child.try_wait(), Ok(None)) {
            return Ok((p.url.clone(), p.token.clone()));
        }
        *slot = None;
        log::line("cursor engine: bridge exited, restarting");
    }
    // A new bridge knows none of the old agents.
    reset();
    if !exe().is_file() {
        return Err("Falta el motor de Cursor. Instálalo en Ajustes → Asistente → «Instalar motor».".into());
    }
    let (cb_url, cb_token) = callback().await?;
    let state = root().join("state");
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;

    let mut cmd = std::process::Command::new(exe());
    cmd.arg("--workspace")
        .arg(platform::home_dir())
        .arg("--state-root")
        .arg(&state)
        .arg("--tool-callback-url")
        .arg(&cb_url)
        .arg("--tool-callback-auth-token")
        .arg(&cb_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    platform::no_console(&mut cmd);
    let mut cmd = tokio::process::Command::from(cmd);
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("No se pudo arrancar el motor de Cursor: {e}"))?;
    if let Some(pid) = child.id() {
        platform::tie_to_app(pid);
    }

    let stderr = child.stderr.take().ok_or("El motor de Cursor no expuso su salida.")?;
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Value, String>>();
    tauri::async_runtime::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tx = Some(tx);
        let mut tail: Vec<String> = Vec::new();
        // Keep draining after the ready line so the pipe never fills up.
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(ready) = line.strip_prefix(READY_PREFIX) {
                if let Some(tx) = tx.take() {
                    let _ = tx.send(serde_json::from_str(ready).map_err(|e| e.to_string()));
                }
            } else if tx.is_some() {
                tail.push(line);
                if tail.len() > 6 {
                    tail.remove(0);
                }
            }
        }
        if let Some(tx) = tx.take() {
            let _ = tx.send(Err(tail.join(" / ")));
        }
    });
    let ready = tokio::time::timeout(READY_TIMEOUT, rx)
        .await
        .map_err(|_| "El motor de Cursor no arrancó a tiempo.".to_string())?
        .map_err(|_| "El motor de Cursor se cerró al arrancar.".to_string())?
        .map_err(|e| format!("El motor de Cursor no arrancó: {e}"))?;
    let url = ready["url"].as_str().ok_or("El motor de Cursor no dio su dirección.")?.to_string();
    let token_file = ready["authTokenFile"].as_str().ok_or("El motor de Cursor no dio su token.")?;
    let token = tokio::fs::read_to_string(token_file)
        .await
        .map_err(|e| format!("No se pudo leer el token del motor: {e}"))?
        .trim()
        .to_string();
    log::line("cursor engine: bridge started");
    *slot = Some(Proc { url: url.clone(), token: token.clone(), child });

    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }
            let idle = LAST_USE.lock().unwrap().is_none_or(|t| t.elapsed() > IDLE_STOP);
            if idle && !BUSY.load(Ordering::Relaxed) {
                stop().await;
                log::line("cursor engine: stopped after being idle");
                return;
            }
        }
    });
    Ok((url, token))
}

/// Stops the bridge, if it runs. The next message starts it again.
pub async fn stop() {
    let Some(mut p) = BRIDGE.lock().await.take() else { return };
    GENERATION.fetch_add(1, Ordering::Relaxed);
    let _ = client()
        .post(format!("{}/sdk.v1.SdkBridgeControlService/Shutdown", p.url))
        .bearer_auth(&p.token)
        .timeout(Duration::from_secs(3))
        .json(&json!({ "graceSeconds": 0 }))
        .send()
        .await;
    if tokio::time::timeout(Duration::from_secs(3), p.child.wait()).await.is_err() {
        let _ = p.child.kill().await;
    }
    reset();
}

fn connect_error(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = v["code"].as_str().unwrap_or_default();
    let msg = v["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| body.chars().take(200).collect());
    let lower = msg.to_lowercase();
    if code == "unauthenticated" || lower.contains("api key") {
        "Cursor rechazó la clave. Revísala en Ajustes → Asistente (se crea en cursor.com/dashboard/integrations → User API Keys).".into()
    } else if code == "resource_exhausted" || lower.contains("usage limit") || lower.contains("rate limit") {
        format!("Cursor: llegaste al límite de uso de tu plan. {msg}")
    } else if lower.contains("model") && (code == "invalid_argument" || code == "not_found") {
        format!("Cursor no acepta ese modelo: {msg}. Elige otro en Ajustes → Asistente → Ver modelos.")
    } else {
        format!("Cursor: {msg}")
    }
}

async fn rpc(url: &str, token: &str, method: &str, body: Value) -> Result<Value, String> {
    let response = client()
        .post(format!("{url}/sdk.v1.{method}"))
        .bearer_auth(token)
        .timeout(RPC_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("No se pudo hablar con el motor de Cursor: {e}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(connect_error(&text));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

// ── Models ────────────────────────────────────────────────────────────────────

fn version_of(id: &str) -> Vec<u32> {
    let lower = id.to_lowercase();
    let rest = lower.split_once("grok").map(|(_, r)| r).unwrap_or(&lower);
    let start = rest.find(|c: char| c.is_ascii_digit());
    let Some(start) = start else { return Vec::new() };
    rest[start..]
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()
        .unwrap_or_default()
        .split('.')
        .filter_map(|n| n.parse().ok())
        .collect()
}

/// The newest Grok, preferring the plain id over its variants.
fn pick_grok(ids: &[String]) -> Option<String> {
    ids.iter()
        .filter(|id| id.to_lowercase().contains("grok"))
        .max_by(|a, b| version_of(a).cmp(&version_of(b)).then_with(|| b.len().cmp(&a.len())))
        .cloned()
}

async fn model_ids(key: &str) -> Result<Vec<String>, String> {
    let (url, token) = bridge().await?;
    let v = rpc(&url, &token, "SdkCursorService/ListModels", json!({ "options": { "apiKey": key } })).await?;
    let mut ids: Vec<String> = v["items"]
        .as_array()
        .or_else(|| v["models"].as_array())
        .map(|list| list.iter().filter_map(|m| m["id"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub async fn list_models(ep: &Endpoint) -> Result<Vec<String>, String> {
    let mut ids = model_ids(ep.key().unwrap_or_default()).await?;
    ids.insert(0, AUTO_MODEL.to_string());
    Ok(ids)
}

/// The model id to use: the owner's pick, or the newest Grok on the account.
pub async fn model(ep: &Endpoint) -> Result<String, String> {
    static RESOLVED: Mutex<Option<String>> = Mutex::new(None);
    let chosen = ep.model.trim();
    if !chosen.is_empty() && chosen != AUTO_MODEL {
        return Ok(chosen.to_string());
    }
    if let Some(m) = RESOLVED.lock().unwrap().clone() {
        return Ok(m);
    }
    let ids = model_ids(ep.key().unwrap_or_default()).await?;
    let picked = pick_grok(&ids).ok_or_else(|| {
        format!(
            "Tu cuenta de Cursor no ofrece ningún modelo Grok. Elige otro en Ajustes → Asistente → Modelo ({}…).",
            ids.iter().take(4).cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    log::line(format!("cursor engine: picked model {picked}"));
    *RESOLVED.lock().unwrap() = Some(picked.clone());
    Ok(picked)
}

// ── Conversation ──────────────────────────────────────────────────────────────

struct Session {
    agent: String,
    tools: String,
    primed: bool,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// A new conversation: the next message creates a fresh agent.
pub fn reset() {
    SESSION.lock().unwrap().take();
}

struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Relaxed);
        touch();
    }
}

/// What one Send stream said, folded as it arrives.
#[derive(Default)]
struct Run {
    run_id: Option<String>,
    /// Streamed text, as shown in the island.
    text: String,
    /// Assistant text from whole messages, for bridges that send no deltas.
    fallback: String,
    need_sep: bool,
    status_error: Option<String>,
    result: Option<Value>,
    done: bool,
}

fn content_text(payload: &Value) -> String {
    let blocks = payload["message"]["content"].as_array().or_else(|| payload["content"].as_array());
    blocks
        .map(|list| {
            list.iter()
                .filter(|b| b["type"].as_str() == Some("text"))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

impl Run {
    fn handle(&mut self, msg: &Value, emit: &mut dyn FnMut(&str)) {
        if let Some(sdk) = msg.get("sdkMessage") {
            let payload = &sdk["message"];
            if self.run_id.is_none() {
                self.run_id = payload["run_id"].as_str().or_else(|| payload["runId"].as_str()).map(str::to_string);
            }
            match sdk["type"].as_str().unwrap_or_default() {
                "assistant" => {
                    let t = content_text(payload);
                    if !t.is_empty() {
                        if !self.fallback.is_empty() {
                            self.fallback.push_str("\n\n");
                        }
                        self.fallback.push_str(&t);
                    }
                }
                "status" => {
                    let st = payload["status"].as_str().unwrap_or_default().to_uppercase();
                    if st.contains("ERROR") || st.contains("FAIL") {
                        if let Some(m) = payload["message"].as_str().filter(|m| !m.is_empty()) {
                            self.status_error = Some(m.to_string());
                        }
                    }
                }
                _ => {}
            }
        } else if let Some(up) = msg.get("interactionUpdate") {
            let update = &up["update"];
            let kind = up["type"].as_str().or_else(|| update["type"].as_str()).unwrap_or_default();
            match kind {
                "text-delta" => {
                    let t = update["text"].as_str().unwrap_or_default();
                    if t.is_empty() {
                        return;
                    }
                    if self.need_sep && !self.text.is_empty() {
                        emit("\n\n");
                        self.text.push_str("\n\n");
                    }
                    self.need_sep = false;
                    emit(t);
                    self.text.push_str(t);
                }
                "tool-call-started" => self.need_sep = true,
                _ => {}
            }
        } else if let Some(result) = msg.get("result") {
            if self.run_id.is_none() {
                self.run_id = result["runId"].as_str().map(str::to_string);
            }
            self.result = Some(result.clone());
        } else if msg.get("done").is_some() {
            self.done = true;
        }
    }
}

/// Connect streaming frames: 1 flag byte, a 4-byte big-endian length, the JSON.
#[derive(Default)]
struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    fn push(&mut self, chunk: &[u8]) -> Vec<(u8, Vec<u8>)> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while self.buf.len() >= 5 {
            let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
            if self.buf.len() < 5 + len {
                break;
            }
            let flags = self.buf[0];
            let payload = self.buf[5..5 + len].to_vec();
            self.buf.drain(..5 + len);
            out.push((flags, payload));
        }
        out
    }
}

fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// One chat turn through Cursor. Text streams to the island as `chat-delta`;
/// tools run through the callback server below as the agent asks for them.
pub async fn chat(
    app: &AppHandle,
    ep: &Endpoint,
    system: &str,
    specs: &[ToolSpec],
    query: &str,
    context: Option<&ChatContext>,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let mut emit = |t: &str| {
        let _ = app.emit_to(WINDOW_LABEL, "chat-delta", t);
    };
    converse(&SESSION, ep, system, specs, query, context, cancel, &mut emit).await
}

#[allow(clippy::too_many_arguments)]
async fn converse(
    slot: &Mutex<Option<Session>>,
    ep: &Endpoint,
    system: &str,
    specs: &[ToolSpec],
    query: &str,
    context: Option<&ChatContext>,
    cancel: &AtomicBool,
    emit: &mut (dyn FnMut(&str) + Send),
) -> Result<String, String> {
    BUSY.store(true, Ordering::Relaxed);
    let _busy = BusyGuard;
    let key = ep.key().ok_or("Falta la clave de Cursor. Ponla en Ajustes → Asistente.")?.to_string();
    let (url, token) = bridge().await?;

    let custom: Map<String, Value> = specs
        .iter()
        .map(|t| (t.name.clone(), json!({ "description": t.description, "inputSchema": t.schema })))
        .collect();
    let signature = specs.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(",");
    let options = |agent: Option<&str>| {
        let mut o = json!({
            "model": { "id": ep.model },
            "apiKey": key,
            "name": "Mochi",
            "local": { "cwd": [platform::home_dir().to_string_lossy()], "customTools": custom },
            "tools": { "names": [] },
        });
        if let Some(id) = agent {
            o["agentId"] = json!(id);
        }
        o
    };

    let current = slot.lock().unwrap().as_ref().map(|s| (s.agent.clone(), s.tools.clone(), s.primed));
    let (agent, primed) = match current {
        Some((agent, tools, primed)) => {
            if tools != signature {
                // The tool set changed (an MCP server came up, a skill was added).
                rpc(&url, &token, "SdkAgentService/ResumeAgent", json!({ "agentId": agent, "options": options(Some(&agent)) })).await?;
                if let Some(s) = slot.lock().unwrap().as_mut() {
                    s.tools = signature.clone();
                }
            }
            (agent, primed)
        }
        None => {
            let v = rpc(&url, &token, "SdkAgentService/CreateAgent", json!({ "options": options(None) })).await?;
            let agent = v["agentId"].as_str().ok_or("Cursor no creó el agente.")?.to_string();
            *slot.lock().unwrap() = Some(Session { agent: agent.clone(), tools: signature.clone(), primed: false });
            (agent, false)
        }
    };

    let mut text = String::new();
    let mut images: Vec<Value> = Vec::new();
    if !primed {
        text.push_str(system);
        text.push_str("\n\n---\n\n");
        if let Some(ctx) = context {
            for part in claude::context_parts(ctx) {
                match part {
                    Part::Text(t) => {
                        text.push_str(&t);
                        text.push_str("\n\n");
                    }
                    Part::Image { media, data } => images.push(json!({ "data": { "data": data, "mimeType": media } })),
                    Part::Pdf { name, .. } => text.push_str(&format!(
                        "(El dueño adjuntó el PDF «{name}», pero este motor no lee PDFs: pídele el texto si lo necesitas.)\n\n"
                    )),
                }
            }
        }
    }
    text.push_str(query);
    let mut message = json!({ "text": text });
    if !images.is_empty() {
        message["images"] = Value::Array(images);
    }
    let body = json!({
        "agentId": agent,
        "message": message,
        "options": { "model": { "id": ep.model }, "enableDeltas": true },
    });

    let mut response = client()
        .post(format!("{url}/sdk.v1.SdkAgentService/Send"))
        .bearer_auth(&token)
        .header("content-type", "application/connect+json")
        .body(frame(body.to_string().as_bytes()))
        .send()
        .await
        .map_err(|e| format!("No se pudo hablar con el motor de Cursor: {e}"))?;
    if !response.status().is_success() {
        return Err(connect_error(&response.text().await.unwrap_or_default()));
    }
    if let Some(s) = slot.lock().unwrap().as_mut() {
        s.primed = true;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, String>>(64);
    tauri::async_runtime::spawn(async move {
        loop {
            let next = response.chunk().await;
            let item = match next {
                Ok(Some(chunk)) => Ok(chunk.to_vec()),
                Ok(None) => return,
                Err(e) => Err(e.to_string()),
            };
            let failed = item.is_err();
            if tx.send(item).await.is_err() || failed {
                return;
            }
        }
    });

    let mut run = Run::default();
    let mut frames = Frames::default();
    let mut cancelled_at: Option<Instant> = None;
    'stream: loop {
        if cancel.load(Ordering::Relaxed) && cancelled_at.is_none() {
            cancelled_at = Some(Instant::now());
            if let Some(run_id) = &run.run_id {
                let _ = rpc(&url, &token, "SdkAgentService/CancelRun", json!({ "runId": run_id, "agentId": agent })).await;
            }
        }
        if cancelled_at.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            break;
        }
        let chunk = match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
            Err(_) => continue,
            Ok(None) => break,
            Ok(Some(Err(e))) => return Err(format!("Se cortó la conexión con el motor de Cursor: {e}")),
            Ok(Some(Ok(chunk))) => chunk,
        };
        touch();
        for (flags, payload) in frames.push(&chunk) {
            let msg: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
            if flags & 0x02 != 0 {
                if let Some(err) = msg.get("error") {
                    return Err(connect_error(&err.to_string()));
                }
                break 'stream;
            }
            run.handle(&msg, &mut *emit);
        }
        if run.done {
            break;
        }
    }

    let result = run.result.clone().unwrap_or(Value::Null);
    let status = result["status"].as_str().unwrap_or_default().to_uppercase();
    if run.text.is_empty() {
        let final_text = result["result"]["result"].as_str().unwrap_or_default().to_string();
        let t = if !run.fallback.is_empty() { std::mem::take(&mut run.fallback) } else { final_text };
        if !t.is_empty() {
            emit(&t);
            run.text = t;
        }
    }
    if cancelled_at.is_some() || status.contains("CANCELLED") {
        return if run.text.is_empty() { Err("Detenido.".into()) } else { Ok(format!("{}\n\n(detenido)", run.text)) };
    }
    if status.contains("ERROR") || status.contains("EXPIRED") {
        let why = run
            .status_error
            .or_else(|| result["errorCode"].as_str().filter(|c| !c.is_empty()).map(str::to_string))
            .unwrap_or_else(|| "la ejecución falló".into());
        return Err(format!("Cursor: {why}"));
    }
    if run.result.is_none() && run.text.is_empty() {
        return Err("El motor de Cursor terminó sin responder.".into());
    }
    Ok(run.text)
}

// ── Custom-tool callbacks ─────────────────────────────────────────────────────

fn random_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    (0..4u64)
        .map(|i| {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The loopback server the bridge calls when the agent uses one of Mochi's tools.
async fn callback() -> Result<(String, String), String> {
    static CALLBACK: tokio::sync::OnceCell<(String, String)> = tokio::sync::OnceCell::const_new();
    CALLBACK
        .get_or_try_init(|| async {
            let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            let token = random_token();
            let expected = token.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let expected = expected.clone();
                            tauri::async_runtime::spawn(async move { serve(stream, &expected).await });
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                }
            });
            Ok((format!("http://127.0.0.1:{port}"), token))
        })
        .await
        .cloned()
}

struct Request {
    path: String,
    auth: String,
    body: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

async fn fill<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> Result<(), String> {
    let mut tmp = [0u8; 16 * 1024];
    let n = tokio::time::timeout(Duration::from_secs(30), reader.read(&mut tmp))
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("closed".into());
    }
    buf.extend_from_slice(&tmp[..n]);
    if buf.len() > MAX_CALLBACK_BODY {
        return Err("too large".into());
    }
    Ok(())
}

/// One HTTP/1.1 request, with a Content-Length or a chunked body.
async fn read_request<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Request, String> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        fill(reader, &mut buf).await?;
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut rest = buf[head_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let path = lines.next().unwrap_or_default().split_whitespace().nth(1).unwrap_or_default().to_string();
    let (mut length, mut chunked, mut auth) = (0usize, false, String::new());
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = v.trim().parse().unwrap_or(0),
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "authorization" => auth = v.trim().to_string(),
            _ => {}
        }
    }
    let body = if chunked {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(i) = find(&rest, b"\r\n") {
                    break i;
                }
                fill(reader, &mut rest).await?;
            };
            let size_line = String::from_utf8_lossy(&rest[..line_end]).to_string();
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or_default().trim(), 16)
                .map_err(|_| "bad chunk size".to_string())?;
            rest.drain(..line_end + 2);
            if size == 0 {
                break body;
            }
            while rest.len() < size + 2 {
                fill(reader, &mut rest).await?;
            }
            body.extend_from_slice(&rest[..size]);
            rest.drain(..size + 2);
            if body.len() > MAX_CALLBACK_BODY {
                return Err("too large".into());
            }
        }
    } else {
        if length > MAX_CALLBACK_BODY {
            return Err("too large".into());
        }
        while rest.len() < length {
            fill(reader, &mut rest).await?;
        }
        rest.truncate(length);
        rest
    };
    Ok(Request { path, auth, body })
}

async fn handle(req: Request, token: &str) -> (u16, Value) {
    if !same(req.auth.strip_prefix("Bearer ").unwrap_or_default().trim(), token) {
        return (401, json!({ "code": "unauthenticated", "message": "Unauthorized" }));
    }
    if req.path != CALLBACK_PATH {
        return (404, json!({ "code": "unimplemented", "message": "unknown method" }));
    }
    let Ok(v) = serde_json::from_slice::<Value>(&req.body) else {
        return (400, json!({ "code": "invalid_argument", "message": "bad JSON" }));
    };
    let Some(app) = APP.get() else {
        return (503, json!({ "code": "unavailable", "message": "starting" }));
    };
    let ours = SESSION.lock().unwrap().as_ref().map(|s| s.agent.clone());
    let text_result = |text: &str, is_error: bool| {
        (200, json!({ "result": { "content": [{ "type": "text", "text": text }], "isError": is_error } }))
    };
    if ours.as_deref() != v["agentId"].as_str() {
        return text_result("This conversation has ended.", true);
    }
    let call = ToolCall {
        id: v["toolCallId"].as_str().unwrap_or("cursor").to_string(),
        name: v["toolName"].as_str().unwrap_or_default().to_string(),
        input: if v["args"].is_object() { v["args"].clone() } else { json!({}) },
    };
    let label = crate::agent::step_label(&call.name, &tools::describe(&call.name, &call.input));
    let _ = app.emit_to(WINDOW_LABEL, "chat-step", json!({ "label": label }));
    touch();
    let outcome = tools::run(app, &call).await;
    touch();
    text_result(&outcome.text, outcome.is_error)
}

async fn serve(mut stream: TcpStream, token: &str) {
    let (status, body) = match read_request(&mut stream).await {
        Ok(req) => handle(req, token).await,
        Err(e) => (400, json!({ "code": "invalid_argument", "message": e })),
    };
    let body = body.to_string();
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Bad Request",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_plain_grok() {
        let ids: Vec<String> = ["composer-2", "grok-4", "grok-4.7-high-fast", "grok-4.7", "grok-code-fast-1", "gpt-5"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(pick_grok(&ids).as_deref(), Some("grok-4.7"));
        assert_eq!(pick_grok(&["gpt-5".to_string()]), None);
    }

    #[test]
    fn reads_versions_after_grok() {
        assert_eq!(version_of("grok-4.7-high"), vec![4, 7]);
        assert_eq!(version_of("cursor-grok-4.6-fast"), vec![4, 6]);
        assert_eq!(version_of("grok-code-fast-1"), vec![1]);
    }

    #[test]
    fn finds_the_asset_hash() {
        let sums = "aaa  other.tar.gz\nBBB *cursor-sdk-bridge-standalone-win32-x64.tar.gz\n";
        assert_eq!(expected_hash(sums, "cursor-sdk-bridge-standalone-win32-x64.tar.gz").as_deref(), Some("bbb"));
        assert_eq!(expected_hash(sums, "missing"), None);
    }

    #[test]
    fn frames_split_across_chunks() {
        let a = frame(br#"{"sdkMessage":{"type":"system"}}"#);
        let b = frame(br#"{"done":{}}"#);
        let all = [a.clone(), b.clone()].concat();
        let mut f = Frames::default();
        assert!(f.push(&all[..3]).is_empty());
        let got = f.push(&all[3..a.len() + 2]);
        assert_eq!(got.len(), 1);
        let got = f.push(&all[a.len() + 2..]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, br#"{"done":{}}"#.to_vec());
    }

    #[test]
    fn deltas_stream_and_tools_separate_paragraphs() {
        let mut run = Run::default();
        let mut shown = String::new();
        let mut emit = |t: &str| shown.push_str(t);
        let msgs = [
            json!({ "sdkMessage": { "type": "system", "message": { "run_id": "r1" } } }),
            json!({ "interactionUpdate": { "type": "text-delta", "update": { "type": "text-delta", "text": "Miro." } } }),
            json!({ "interactionUpdate": { "type": "tool-call-started", "update": {} } }),
            json!({ "interactionUpdate": { "type": "text-delta", "update": { "text": "Listo." } } }),
            json!({ "result": { "status": "RUN_LIFECYCLE_STATUS_FINISHED", "result": { "result": "Miro.\n\nListo." } } }),
            json!({ "done": {} }),
        ];
        for m in &msgs {
            run.handle(m, &mut emit);
        }
        assert_eq!(run.run_id.as_deref(), Some("r1"));
        assert!(run.done);
        assert_eq!(shown, "Miro.\n\nListo.");
    }

    #[tokio::test]
    async fn reads_chunked_and_sized_requests() {
        let chunked = b"POST /x HTTP/1.1\r\nAuthorization: Bearer t\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        let req = read_request(&mut &chunked[..]).await.unwrap();
        assert_eq!(req.path, "/x");
        assert_eq!(req.auth, "Bearer t");
        assert_eq!(req.body, b"{\"a\":1}");

        let sized = b"POST /y HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let req = read_request(&mut &sized[..]).await.unwrap();
        assert_eq!(req.body, b"{}");
    }

    /// Live: downloads the real bridge into %LOCALAPPDATA%\Coucou, starts it and
    /// checks that a wrong key comes back as a readable error. With
    /// COUCOU_TEST_CURSOR_KEY set, also lists the account's models.
    #[tokio::test]
    #[ignore]
    async fn bridge_installs_starts_and_checks_keys() {
        if !exe().is_file() {
            install().await.expect("install");
        }
        assert!(status().installed);
        let err = model_ids("key_not_a_real_key").await.unwrap_err();
        assert!(err.contains("clave") || err.starts_with("Cursor:"), "{err}");
        if let Ok(key) = std::env::var("COUCOU_TEST_CURSOR_KEY") {
            let ids = model_ids(&key).await.expect("models");
            println!("models: {ids:?}\npicked: {:?}", pick_grok(&ids));
        }
        stop().await;
        assert!(BRIDGE.lock().await.is_none());
    }

    #[test]
    fn tokens_compare_exactly() {
        let t = random_token();
        assert_eq!(t.len(), 64);
        assert!(same(&t, &t.clone()));
        assert!(!same(&t, &random_token()));
        assert!(!same("abc", "abcd"));
    }
}
