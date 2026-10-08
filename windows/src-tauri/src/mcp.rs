// MCP client — the assistant's connections to anything that speaks the Model
// Context Protocol: browsers (Playwright), the Windows desktop, GitHub, Notion,
// databases, home automation…
//
// A small client of our own rather than a crate: JSON-RPC 2.0 over stdio (one
// child process per server) and over Streamable HTTP. It does what the
// assistant needs — initialize, tools/list, tools/call — and nothing else.
//
// Servers live in %APPDATA%\Coucou\mcp.json, in the same shape as Cursor's
// ~/.cursor/mcp.json. Secret values (API keys in env or headers) never stay in
// that file: they move to the Credential Manager and the file keeps a
// `${secret:…}` placeholder. Each server tool reaches the model as
// `mcp__<server>__<tool>`; calls go through policy.rs like every other tool.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::policy::{self, Risk};
use crate::providers::{Part, ToolCall, ToolSpec};
use crate::tools::Outcome;
use crate::{log, platform, secrets, settings};

pub const PREFIX: &str = "mcp__";
const PROTOCOL_VERSION: &str = "2025-06-18";
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(180);
/// Images one tool result may hand the model, and their size as base64
/// (the providers' own limit is about 5 MB per image).
const MAX_IMAGES: usize = 2;
const MAX_IMAGE_B64: usize = 5_000_000;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Ask before every call, even tools that say they only read.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ask_all: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, ServerConfig>,
}

fn config_path() -> PathBuf {
    settings::config_dir().join("mcp.json")
}

pub fn load_config() -> Config {
    std::fs::read(config_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), String> {
    platform::ensure_private_dir(&settings::config_dir()).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(config_path(), bytes).map_err(|e| e.to_string())
}

/// Server names become part of tool names: keep them short and plain.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 24 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn looks_secret(field: &str) -> bool {
    let f = field.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "PASS", "AUTH", "CREDENTIAL", "PAT"]
        .iter()
        .any(|w| f.contains(w))
}

fn secret_id(server: &str, kind: &str, field: &str) -> String {
    let field: String = field
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    format!("{server}:{kind}:{field}")
}

/// Moves secret-looking env values and every header value into the Credential
/// Manager, leaving placeholders behind.
fn stash_secrets(name: &str, cfg: &mut ServerConfig, explicit: &BTreeMap<String, String>) -> Result<(), String> {
    for (field, value) in cfg.env.iter_mut() {
        let given = explicit.get(&format!("env:{field}"));
        if given.is_none() && (value.contains("${") || !looks_secret(field) || value.is_empty()) {
            continue;
        }
        let id = secret_id(name, "env", field);
        secrets::set(&format!("{}{id}", secrets::MCP_PREFIX), given.unwrap_or(value))?;
        *value = format!("${{secret:{id}}}");
    }
    for (field, value) in cfg.headers.iter_mut() {
        let given = explicit.get(&format!("header:{field}"));
        if given.is_none() && (value.contains("${") || value.is_empty()) {
            continue;
        }
        let id = secret_id(name, "header", field);
        secrets::set(&format!("{}{id}", secrets::MCP_PREFIX), given.unwrap_or(value))?;
        *value = format!("${{secret:{id}}}");
    }
    Ok(())
}

/// `${secret:id}`, `${env:VAR}` and `${userHome}` → their values.
fn resolve(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let token = &after[..end];
        let replacement = if let Some(id) = token.strip_prefix("secret:") {
            secrets::get(&format!("{}{id}", secrets::MCP_PREFIX)).unwrap_or_default()
        } else if let Some(var) = token.strip_prefix("env:") {
            std::env::var(var).unwrap_or_default()
        } else if token == "userHome" {
            platform::home_dir().display().to_string()
        } else {
            format!("${{{token}}}")
        };
        out.push_str(&replacement);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

// ── Runtime ───────────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub name: String,
    pub transport: String,
    /// "connecting", "connected", "disabled" or "error".
    pub state: String,
    pub error: Option<String>,
    pub tools: Vec<String>,
}

struct McpTool {
    /// Name the model sees.
    exposed: String,
    /// Name the server knows.
    name: String,
    description: String,
    schema: Value,
    read_only: bool,
}

struct Server {
    status: ServerStatus,
    ask_all: bool,
    tools: Vec<McpTool>,
    conn: Option<Arc<Conn>>,
}

enum Conn {
    Stdio(StdioConn),
    Http(HttpConn),
}

static SERVERS: LazyLock<Mutex<BTreeMap<String, Server>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// The owner's servers plus the MCP-kind skills.
fn all_servers() -> Vec<(String, ServerConfig)> {
    let mut out: Vec<(String, ServerConfig)> = load_config().mcp_servers.into_iter().collect();
    out.extend(crate::selfmod::skills::mcp_servers());
    out
}

pub fn statuses() -> Vec<ServerStatus> {
    let servers = SERVERS.lock().unwrap();
    let mut out: Vec<ServerStatus> = Vec::new();
    for (name, cfg) in &all_servers() {
        match servers.get(name) {
            Some(s) => out.push(s.status.clone()),
            None => out.push(ServerStatus {
                name: name.clone(),
                transport: if cfg.url.is_some() { "http".into() } else { "stdio".into() },
                state: if cfg.disabled { "disabled".into() } else { "connecting".into() },
                error: None,
                tools: Vec::new(),
            }),
        }
    }
    out
}

fn notify(app: &AppHandle) {
    let _ = app.emit("mcp-changed", statuses());
}

/// Connects every enabled server. Called at launch and after any change.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        shutdown_all();
        for (name, cfg) in all_servers() {
            if cfg.disabled {
                continue;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move { connect(app, name, cfg).await });
        }
        notify(&app);
    });
}

fn shutdown_all() {
    let mut servers = SERVERS.lock().unwrap();
    for (_, server) in servers.iter_mut() {
        if let Some(conn) = server.conn.take() {
            conn.close();
        }
    }
    servers.clear();
}

async fn connect(app: AppHandle, name: String, cfg: ServerConfig) {
    let transport = if cfg.url.is_some() { "http" } else { "stdio" };
    let set = |state: &str, error: Option<String>, tools: Vec<McpTool>, conn: Option<Arc<Conn>>| {
        let status = ServerStatus {
            name: name.clone(),
            transport: transport.into(),
            state: state.into(),
            error,
            tools: tools.iter().map(|t| t.name.clone()).collect(),
        };
        SERVERS.lock().unwrap().insert(name.clone(), Server { status, ask_all: cfg.ask_all, tools, conn });
    };
    set("connecting", None, Vec::new(), None);
    notify(&app);

    let result = async {
        let conn = Arc::new(match &cfg.url {
            Some(url) => Conn::Http(HttpConn::new(resolve(url), &cfg.headers)?),
            None => Conn::Stdio(StdioConn::spawn(&name, &cfg)?),
        });
        conn.initialize().await?;
        let tools = conn.list_tools(&name).await?;
        Ok::<_, String>((conn, tools))
    }
    .await;

    match result {
        Ok((conn, tools)) => {
            log::line(format!("mcp {name}: connected, {} tools", tools.len()));
            set("connected", None, tools, Some(conn));
        }
        Err(err) => {
            log::line(format!("mcp {name}: {err}"));
            set("error", Some(err), Vec::new(), None);
        }
    }
    notify(&app);
}

impl Conn {
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        match self {
            Conn::Stdio(c) => c.request(method, params, timeout).await,
            Conn::Http(c) => c.request(method, params, timeout).await,
        }
    }

    async fn notify(&self, method: &str) {
        let msg = json!({ "jsonrpc": "2.0", "method": method });
        match self {
            Conn::Stdio(c) => {
                let _ = c.write(&msg).await;
            }
            Conn::Http(c) => {
                let _ = c.post(&msg, Duration::from_secs(10)).await;
            }
        }
    }

    async fn initialize(&self) -> Result<(), String> {
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "coucou", "version": env!("CARGO_PKG_VERSION") },
        });
        self.request("initialize", params, INIT_TIMEOUT).await?;
        self.notify("notifications/initialized").await;
        Ok(())
    }

    async fn list_tools(&self, server: &str) -> Result<Vec<McpTool>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, INIT_TIMEOUT).await?;
            for t in result["tools"].as_array().cloned().unwrap_or_default() {
                let Some(name) = t["name"].as_str() else { continue };
                tools.push(McpTool {
                    exposed: exposed_name(server, name),
                    name: name.to_string(),
                    description: t["description"].as_str().unwrap_or_default().chars().take(1000).collect(),
                    schema: if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({ "type": "object" }) },
                    read_only: t["annotations"]["readOnlyHint"].as_bool() == Some(true),
                });
            }
            cursor = result["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    fn close(&self) {
        if let Conn::Stdio(c) = self {
            c.kill();
        }
    }
}

/// `mcp__server__tool`, limited to what every provider accepts (64 chars,
/// letters, digits, `_` and `-`).
fn exposed_name(server: &str, tool: &str) -> String {
    let clean: String = tool
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let mut name = format!("{PREFIX}{server}__{clean}");
    name.truncate(64);
    name
}

// ── stdio ─────────────────────────────────────────────────────────────────────

type Waiters = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

struct StdioConn {
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    waiters: Waiters,
    next: AtomicU64,
    child: Mutex<Option<tokio::process::Child>>,
}

impl StdioConn {
    fn spawn(name: &str, cfg: &ServerConfig) -> Result<Self, String> {
        let raw = cfg.command.clone().ok_or("no command and no url")?;
        let command = resolve(&raw);
        // `npx` is npx.cmd on Windows: find it the way a shell would.
        let program = if command.contains(['\\', '/']) {
            PathBuf::from(&command)
        } else {
            platform::find_on_path(&command).ok_or_else(|| format!("{command} is not installed (not on PATH)"))?
        };
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(cfg.args.iter().map(|a| resolve(a)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &cfg.env {
            cmd.env(k, resolve(v));
        }
        if let Some(cwd) = &cfg.cwd {
            cmd.current_dir(resolve(cwd));
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        let mut child = cmd.spawn().map_err(|e| format!("could not start {command}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr = child.stderr.take();

        let waiters: Waiters = Arc::new(Mutex::new(HashMap::new()));
        let stdin = Arc::new(tokio::sync::Mutex::new(stdin));
        let reader_waiters = waiters.clone();
        let reader_stdin = stdin.clone();
        let server = name.to_string();
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                if let (Some(method), Some(id)) = (msg.get("method").and_then(Value::as_str), msg.get("id")) {
                    // A request from the server: answer ping, decline the rest,
                    // so it is never left waiting on us.
                    let reply = if method == "ping" {
                        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                    } else {
                        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported" } })
                    };
                    let mut out = reply.to_string();
                    out.push('\n');
                    let _ = reader_stdin.lock().await.write_all(out.as_bytes()).await;
                    continue;
                }
                let Some(id) = msg.get("id").and_then(Value::as_u64) else { continue };
                if let Some(tx) = reader_waiters.lock().unwrap().remove(&id) {
                    let _ = tx.send(msg);
                }
            }
            log::line(format!("mcp {server}: process ended"));
            reader_waiters.lock().unwrap().clear();
        });
        if let Some(stderr) = stderr {
            let server = name.to_string();
            tauri::async_runtime::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut logged = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    if logged < 20 && !line.trim().is_empty() {
                        log::line(format!("mcp {server} stderr: {}", line.chars().take(200).collect::<String>()));
                        logged += 1;
                    }
                }
            });
        }
        Ok(Self {
            stdin,
            waiters,
            next: AtomicU64::new(1),
            child: Mutex::new(Some(child)),
        })
    }

    async fn write(&self, msg: &Value) -> Result<(), String> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("server closed: {e}"))?;
        stdin.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().unwrap().insert(id, tx);
        self.write(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let msg = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| format!("{method}: no answer in {} s", timeout.as_secs()))?
            .map_err(|_| "the server stopped".to_string())?;
        rpc_result(msg)
    }

    fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }
}

fn rpc_result(msg: Value) -> Result<Value, String> {
    if let Some(err) = msg.get("error") {
        let text = err["message"].as_str().map(str::to_string).unwrap_or_else(|| err.to_string());
        return Err(text);
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

// ── Streamable HTTP ───────────────────────────────────────────────────────────

struct HttpConn {
    url: String,
    headers: Vec<(String, String)>,
    session: Mutex<Option<String>>,
    next: AtomicU64,
    client: reqwest::Client,
}

impl HttpConn {
    fn new(url: String, headers: &BTreeMap<String, String>) -> Result<Self, String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("the URL must start with http:// or https://".into());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            url,
            headers: headers.iter().map(|(k, v)| (k.clone(), resolve(v))).collect(),
            session: Mutex::new(None),
            next: AtomicU64::new(1),
            client,
        })
    }

    async fn post(&self, msg: &Value, timeout: Duration) -> Result<Option<Value>, String> {
        let mut req = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(msg);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(session) = self.session.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", session);
        }
        let response = req.send().await.map_err(|e| format!("network error: {e}"))?;
        if let Some(session) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().unwrap() = Some(session.to_string());
        }
        let status = response.status();
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = response.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {}", body.chars().take(300).collect::<String>()));
        }
        let Some(id) = msg.get("id") else { return Ok(None) };
        if is_sse {
            for line in body.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                        if v.get("id") == Some(id) {
                            return Ok(Some(v));
                        }
                    }
                }
            }
            Err("no answer in the event stream".into())
        } else if body.trim().is_empty() {
            Ok(None)
        } else {
            serde_json::from_str(&body).map(Some).map_err(|e| e.to_string())
        }
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let answer = self.post(&msg, timeout).await?.ok_or("empty answer")?;
        rpc_result(answer)
    }
}

// ── The assistant's side ──────────────────────────────────────────────────────

pub fn specs(_app: &AppHandle) -> Vec<ToolSpec> {
    let servers = SERVERS.lock().unwrap();
    servers
        .values()
        .filter(|s| s.conn.is_some())
        .flat_map(|s| s.tools.iter())
        .map(|t| ToolSpec { name: t.exposed.clone(), description: t.description.clone(), schema: t.schema.clone() })
        .collect()
}

pub async fn call(app: &AppHandle, call: &ToolCall) -> Outcome {
    let found = {
        let servers = SERVERS.lock().unwrap();
        servers.iter().find_map(|(server, s)| {
            let tool = s.tools.iter().find(|t| t.exposed == call.name)?;
            let risk = if tool.read_only && !s.ask_all { Risk::Read } else { Risk::Act };
            Some((server.clone(), tool.name.clone(), risk, s.conn.clone()?))
        })
    };
    let Some((server, tool, risk, conn)) = found else {
        return Outcome::err(format!("{} is not connected", call.name));
    };
    let label = format!("MCP {server} · {tool}");
    let args = call.input.to_string();
    let target = if args == "{}" { String::new() } else { args.chars().take(400).collect() };
    match risk {
        Risk::Read => policy::audit(&label, "read", &target),
        Risk::Act => {
            if !policy::approve(app, &label, &target).await {
                return Outcome::err("The owner declined this action. Do not retry it another way; ask what they want instead.");
            }
        }
    }
    let params = json!({ "name": tool, "arguments": call.input });
    match conn.request("tools/call", params, CALL_TIMEOUT).await {
        Ok(result) => {
            let mut parts: Vec<String> = Vec::new();
            let mut images: Vec<Part> = Vec::new();
            for item in result["content"].as_array().cloned().unwrap_or_default() {
                match item["type"].as_str() {
                    Some("text") => parts.push(item["text"].as_str().unwrap_or_default().to_string()),
                    Some("image") => {
                        let data = item["data"].as_str().unwrap_or_default();
                        let media = item["mimeType"].as_str().unwrap_or("image/png");
                        if images.len() < MAX_IMAGES && !data.is_empty() && data.len() <= MAX_IMAGE_B64 && media.starts_with("image/") {
                            images.push(Part::Image { media: media.to_string(), data: data.to_string() });
                            parts.push("[image attached]".into());
                        } else {
                            parts.push("[image left out: too many or too large]".into());
                        }
                    }
                    Some("resource") => parts.push(
                        item["resource"]["text"].as_str().map(str::to_string).unwrap_or_else(|| "[resource]".into()),
                    ),
                    _ => {}
                }
            }
            if parts.is_empty() && !result["structuredContent"].is_null() {
                parts.push(result["structuredContent"].to_string());
            }
            let text = if parts.is_empty() { "(no output)".to_string() } else { parts.join("\n") };
            let mut outcome = if result["isError"].as_bool() == Some(true) { Outcome::err(text) } else { Outcome::ok(text) };
            outcome.images = images;
            outcome
        }
        Err(err) => Outcome::err(format!("{label}: {err}")),
    }
}

// ── Settings window ───────────────────────────────────────────────────────────

/// Adds or replaces a server. `secrets` carries values typed in the settings
/// window as `env:NAME` / `header:NAME` → value; they go to the Credential Manager.
pub fn save_server(app: &AppHandle, name: &str, mut cfg: ServerConfig, secrets_in: BTreeMap<String, String>) -> Result<(), String> {
    if !valid_name(name) {
        return Err("Name: letters, digits, - and _ only, up to 24 characters.".into());
    }
    if cfg.command.as_deref().unwrap_or("").trim().is_empty() && cfg.url.as_deref().unwrap_or("").trim().is_empty() {
        return Err("Give either a command or a URL.".into());
    }
    stash_secrets(name, &mut cfg, &secrets_in)?;
    let mut config = load_config();
    config.mcp_servers.insert(name.to_string(), cfg);
    save_config(&config)?;
    log::line(format!("mcp {name}: saved"));
    start(app.clone());
    Ok(())
}

pub fn remove_server(app: &AppHandle, name: &str) -> Result<(), String> {
    let mut config = load_config();
    if let Some(cfg) = config.mcp_servers.remove(name) {
        for v in cfg.env.values().chain(cfg.headers.values()) {
            if let Some(id) = v.strip_prefix("${secret:").and_then(|r| r.strip_suffix('}')) {
                let _ = secrets::clear(&format!("{}{id}", secrets::MCP_PREFIX));
            }
        }
    }
    save_config(&config)?;
    log::line(format!("mcp {name}: removed"));
    start(app.clone());
    Ok(())
}

pub fn set_enabled(app: &AppHandle, name: &str, enabled: bool) -> Result<(), String> {
    let mut config = load_config();
    let cfg = config.mcp_servers.get_mut(name).ok_or("unknown server")?;
    cfg.disabled = !enabled;
    save_config(&config)?;
    start(app.clone());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportCandidate {
    pub name: String,
    pub source: String,
    pub summary: String,
    pub exists: bool,
}

fn import_sources() -> Vec<(String, PathBuf)> {
    let home = platform::home_dir();
    vec![
        ("Cursor".into(), home.join(".cursor").join("mcp.json")),
        ("Claude Code".into(), home.join(".claude.json")),
    ]
}

fn read_foreign(path: &PathBuf) -> BTreeMap<String, ServerConfig> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Config>(&b).ok())
        .map(|c| c.mcp_servers)
        .unwrap_or_default()
}

/// What could be imported from Cursor and Claude Code, without touching anything.
pub fn import_preview() -> Vec<ImportCandidate> {
    let ours = load_config();
    let mut out = Vec::new();
    for (source, path) in import_sources() {
        for (name, cfg) in read_foreign(&path) {
            let summary = match (&cfg.url, &cfg.command) {
                (Some(url), _) => url.clone(),
                (None, Some(cmd)) => format!("{cmd} {}", cfg.args.join(" ")),
                _ => continue,
            };
            out.push(ImportCandidate {
                exists: ours.mcp_servers.contains_key(&name),
                name,
                source: source.clone(),
                summary: summary.chars().take(160).collect(),
            });
        }
    }
    out
}

/// Copies the chosen servers; secret values move to the Credential Manager.
pub fn import_apply(app: &AppHandle, names: &[String]) -> Result<usize, String> {
    let mut config = load_config();
    let mut count = 0;
    for (_, path) in import_sources() {
        for (name, mut cfg) in read_foreign(&path) {
            if !names.contains(&name) || !valid_name(&name) || config.mcp_servers.contains_key(&name) {
                continue;
            }
            stash_secrets(&name, &mut cfg, &BTreeMap::new())?;
            config.mcp_servers.insert(name, cfg);
            count += 1;
        }
    }
    save_config(&config)?;
    log::line(format!("mcp: imported {count} server(s)"));
    start(app.clone());
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_safe_for_every_provider() {
        assert_eq!(exposed_name("github", "create_issue"), "mcp__github__create_issue");
        assert_eq!(exposed_name("x", "a.b/c"), "mcp__x__a_b_c");
        assert!(exposed_name("server", &"t".repeat(100)).len() <= 64);
    }

    #[test]
    fn placeholders_resolve_env_and_home() {
        std::env::set_var("COUCOU_TEST_VAR", "42");
        assert_eq!(resolve("a ${env:COUCOU_TEST_VAR} b"), "a 42 b");
        assert!(!resolve("${userHome}").contains("${"));
        assert_eq!(resolve("no ${unknown} change"), "no ${unknown} change");
    }

    #[test]
    fn cursor_config_shape_parses() {
        let raw = r#"{"mcpServers":{"pw":{"command":"npx","args":["@playwright/mcp@latest"]},"remote":{"url":"https://x/mcp","headers":{"Authorization":"Bearer t"}}}}"#;
        let c: Config = serde_json::from_str(raw).unwrap();
        assert_eq!(c.mcp_servers["pw"].args[0], "@playwright/mcp@latest");
        assert_eq!(c.mcp_servers["remote"].url.as_deref(), Some("https://x/mcp"));
    }

    #[test]
    fn secret_looking_fields() {
        assert!(looks_secret("GITHUB_PERSONAL_ACCESS_TOKEN"));
        assert!(looks_secret("api_key"));
        assert!(!looks_secret("BROWSER"));
    }
}
