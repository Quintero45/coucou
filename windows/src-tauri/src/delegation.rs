// ARIA's errands: work she hands to Cursor (an agent of its own in a project
// folder, cursor.rs) or to a Grok Bot (grokbot.rs), followed until it ends.
//
// When an errand ends, a report goes to the island (`errand-report`), which
// sends it to ARIA's chat: she tells the owner what happened and carries on.
// The list lives in memory and the island shows it (`errands`); finished
// errands are also appended to errands.jsonl, next to settings.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::island::WINDOW_LABEL;
use crate::providers::{Endpoint, Provider, ToolCall, ToolSpec};
use crate::tools::Outcome;
use crate::{log, policy, settings, Shared};

/// Errands kept for the island and for tasks_status.
const KEPT: usize = 20;
/// Projects offered by list_projects.
const MAX_PROJECTS: usize = 25;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Errand {
    pub id: u64,
    /// "cursor" or "bot:<id>".
    pub worker: String,
    /// What the island and the reports call it: "Cursor", the Bot's name.
    pub worker_name: String,
    pub title: String,
    pub project: Option<String>,
    /// "working", "needs", "done" or "error".
    pub status: String,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// The latest line the worker wrote.
    pub last: String,
    pub result: String,
}

impl Errand {
    pub fn open(&self) -> bool {
        matches!(self.status.as_str(), "working" | "needs")
    }
}

static ERRANDS: Mutex<Vec<Errand>> = Mutex::new(Vec::new());
static NEXT: Mutex<u64> = Mutex::new(1);

fn now_ms() -> u64 {
    crate::media::now_ms()
}

fn emit_list(app: &AppHandle) {
    let list = ERRANDS.lock().unwrap().clone();
    let _ = app.emit_to(WINDOW_LABEL, "errands", list);
}

fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// A new errand, shown at once.
pub fn start(app: &AppHandle, worker: &str, worker_name: &str, title: &str, project: Option<String>) -> u64 {
    let id = {
        let mut n = NEXT.lock().unwrap();
        let id = *n;
        *n += 1;
        id
    };
    let errand = Errand {
        id,
        worker: worker.into(),
        worker_name: worker_name.into(),
        title: one_line(title, 140),
        project,
        status: "working".into(),
        started_ms: now_ms(),
        ended_ms: None,
        last: String::new(),
        result: String::new(),
    };
    log::line(format!("errand #{id}: {worker_name} started «{}»", errand.title));
    {
        let mut list = ERRANDS.lock().unwrap();
        list.push(errand);
        let excess = list.len().saturating_sub(KEPT);
        // Finished ones go first; an open errand is never dropped.
        for _ in 0..excess {
            if let Some(i) = list.iter().position(|e| !e.open()) {
                list.remove(i);
            }
        }
    }
    emit_list(app);
    id
}

pub fn progress(app: &AppHandle, id: u64, line: &str) {
    let line = one_line(line, 120);
    let changed = {
        let mut list = ERRANDS.lock().unwrap();
        match list.iter_mut().find(|e| e.id == id) {
            Some(e) if e.last != line => {
                e.last = line;
                true
            }
            _ => false,
        }
    };
    if changed {
        emit_list(app);
    }
}

/// The text ARIA gets when an errand ends or needs the owner.
pub fn report_text(e: &Errand) -> String {
    let what = match e.status.as_str() {
        "done" => "terminó",
        "needs" => "necesita algo del dueño en",
        _ => "falló en",
    };
    let place = e.project.as_deref().map(|p| format!(" (proyecto {p})")).unwrap_or_default();
    format!(
        "[Informe del encargo #{id}] {who} {what} «{title}»{place}.\n\n{result}\n\n\
Cuéntale al dueño en pocas palabras qué pasó y, si la tarea sigue, continúa.",
        id = e.id,
        who = e.worker_name,
        title = e.title,
        result = if e.result.trim().is_empty() { "(sin texto)" } else { e.result.trim() },
    )
}

/// An errand ended ("done" or "error") or needs the owner ("needs"): ARIA gets the report.
pub fn finish(app: &AppHandle, id: u64, status: &str, result: &str) {
    let errand = {
        let mut list = ERRANDS.lock().unwrap();
        let Some(e) = list.iter_mut().find(|e| e.id == id) else { return };
        e.status = status.into();
        e.result = result.chars().take(6000).collect();
        if status != "needs" {
            e.ended_ms = Some(now_ms());
        }
        e.clone()
    };
    log::line(format!("errand #{id}: {} {status}", errand.worker_name));
    if !errand.open() {
        let line = serde_json::to_string(&errand).unwrap_or_default();
        let path = settings::config_dir().join("errands.jsonl");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
    }
    emit_list(app);
    let _ = app.emit_to(
        WINDOW_LABEL,
        "errand-report",
        json!({ "id": id, "text": report_text(&errand), "display": report_line(&errand) }),
    );
}

/// The short line the chat shows for a report.
pub fn report_line(e: &Errand) -> String {
    let what = match e.status.as_str() {
        "done" => "terminó",
        "needs" => "te necesita",
        _ => "falló",
    };
    format!("Encargo #{} · {} {what}: {}", e.id, e.worker_name, one_line(&e.title, 80))
}

/// The latest open errand of a worker.
pub fn open_of(worker: &str) -> Option<u64> {
    ERRANDS.lock().unwrap().iter().rev().find(|e| e.worker == worker && e.open()).map(|e| e.id)
}

/// A Grok Bot reported (`aria-hook --bot … --status …`): its open errand moves on.
#[tauri::command]
pub fn errand_bot_update(app: AppHandle, bot: String, status: String, message: String) {
    let Some(id) = open_of(&format!("bot:{}", crate::grokbot::slug(&bot))) else { return };
    match status.as_str() {
        "working" => progress(&app, id, &message),
        "done" | "needs" | "error" => finish(&app, id, &status, &message),
        _ => {}
    }
}

#[tauri::command]
pub fn errands_list() -> Vec<Errand> {
    ERRANDS.lock().unwrap().clone()
}

// ── Projects ──────────────────────────────────────────────────────────────────

/// "file:///c%3A/Users/x/My%20App" → "c:/Users/x/My App".
fn file_uri_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file:///").or_else(|| uri.strip_prefix("file://"))?;
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Folders opened in Cursor, most recent first (Cursor's own workspace list; read only).
pub fn projects() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA") else { return Vec::new() };
    let storage = PathBuf::from(appdata).join("Cursor").join("User").join("workspaceStorage");
    let Ok(dirs) = std::fs::read_dir(storage) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for dir in dirs.flatten() {
        let file = dir.path().join("workspace.json");
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let Some(path) = v["folder"].as_str().and_then(file_uri_path) else { continue };
        let path = PathBuf::from(path);
        if !path.is_dir() {
            continue;
        }
        let when = std::fs::metadata(dir.path()).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        found.push((when, path));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<PathBuf> = Vec::new();
    for (_, p) in found {
        if !out.iter().any(|q| same_path(q, &p)) {
            out.push(p);
        }
        if out.len() >= MAX_PROJECTS {
            break;
        }
    }
    out
}

fn norm(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase()
}

fn same_path(a: &Path, b: &Path) -> bool {
    norm(a) == norm(b)
}

/// A folder path, or the name of a known project ("coucou").
pub fn resolve_project(input: &str, known: &[PathBuf]) -> Result<PathBuf, String> {
    let input = input.trim().trim_matches('"');
    if input.is_empty() {
        return Err("Falta el proyecto.".into());
    }
    let direct = PathBuf::from(input);
    if direct.is_absolute() {
        return if direct.is_dir() { Ok(direct) } else { Err(format!("La carpeta {input} no existe.")) };
    }
    let wanted = input.to_lowercase();
    let by_name: Vec<&PathBuf> = known
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == wanted))
        .collect();
    match by_name.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("No conozco un proyecto llamado «{input}». Usa list_projects o pide la ruta al dueño.")),
        many => Err(format!(
            "Hay varios proyectos «{input}»: {}. Pregunta al dueño cuál.",
            many.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// An errand may not work on ARIA's own source (her code changes go through
/// the evolve tools, which keep the protected core) nor on a folder holding it.
pub fn forbidden_project(project: &Path, aria_repo: &Path) -> bool {
    let (p, r) = (norm(project), norm(aria_repo));
    !r.is_empty() && (p == r || r.starts_with(&format!("{p}/")) || p.starts_with(&format!("{r}/")))
}

// ── Tools for the assistant ───────────────────────────────────────────────────

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "delegate_to_cursor".into(),
            description: "Hand a coding task to Cursor: a Cursor agent of its own starts in the project's folder, \
with Cursor's tools (read, edit, terminal), and works in the background. You get its report later in this chat \
as «[Informe del encargo #N]»; do not wait for it. Use it for changes in the owner's projects. Write a complete, \
self-contained task with a clear finish line. `project` is a folder path or a project name from list_projects."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Folder path, or a project name from list_projects" },
                    "task": { "type": "string", "description": "The task, complete and self-contained" }
                },
                "required": ["project", "task"]
            }),
        },
        ToolSpec {
            name: "list_projects".into(),
            description: "The folders the owner opened in Cursor, most recent first. Use it to find the project a task is about.".into(),
            schema: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "tasks_status".into(),
            description: "Your errands (tasks you handed to Cursor or to Grok Bots): who has each one, its state and its result.".into(),
            schema: json!({ "type": "object", "properties": {} }),
        },
        ToolSpec {
            name: "media_control".into(),
            description: "Control what is playing on this PC (Spotify, a browser tab, any player in Windows' media controls): \
play/pause, next or previous track, or set the system volume (0–1). Use action \"status\" to see what is playing."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["status", "toggle", "next", "prev", "volume"] },
                    "volume": { "type": "number", "description": "0 to 1, with action volume" }
                },
                "required": ["action"]
            }),
        },
    ]
}

fn status_word(s: &str) -> &'static str {
    match s {
        "working" => "trabajando",
        "needs" => "te necesita",
        "done" => "terminado",
        _ => "falló",
    }
}

/// Boxed with a named type: these tools also run inside Cursor's tool callback
/// (cursor.rs), and delegate_to_cursor goes back into cursor.rs.
pub fn run<'a>(app: &'a AppHandle, call: &'a ToolCall) -> Pin<Box<dyn Future<Output = Option<Outcome>> + Send + 'a>> {
    Box::pin(run_inner(app, call))
}

async fn run_inner(app: &AppHandle, call: &ToolCall) -> Option<Outcome> {
    let s = |k: &str| call.input.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    Some(match call.name.as_str() {
        "list_projects" => {
            policy::audit("list_projects", "read", "");
            let list = projects();
            if list.is_empty() {
                Outcome::ok("No encontré proyectos abiertos en Cursor en este PC.")
            } else {
                Outcome::ok(list.iter().map(|p| format!("- {}", p.display())).collect::<Vec<_>>().join("\n"))
            }
        }
        "tasks_status" => {
            policy::audit("tasks_status", "read", "");
            let list = ERRANDS.lock().unwrap().clone();
            if list.is_empty() {
                Outcome::ok("No tienes encargos.")
            } else {
                Outcome::ok(
                    list.iter()
                        .rev()
                        .map(|e| {
                            let detail = if e.open() { e.last.clone() } else { one_line(&e.result, 300) };
                            format!("#{} · {} · {} · «{}» — {}", e.id, e.worker_name, status_word(&e.status), e.title, detail)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
        }
        "media_control" => media(app, &s("action"), call.input.get("volume").and_then(Value::as_f64)).await,
        "delegate_to_cursor" => delegate_to_cursor(app, &s("project"), &s("task")).await,
        _ => return None,
    })
}

async fn media(app: &AppHandle, action: &str, volume: Option<f64>) -> Outcome {
    if action == "status" {
        policy::audit("media_control", "read", "status");
        let snap = tauri::async_runtime::spawn_blocking(crate::media::snapshot).await.unwrap_or_default();
        return Outcome::ok(if !snap.active {
            "Nada está sonando.".to_string()
        } else {
            format!(
                "{} · «{}»{} — {}",
                if snap.app.is_empty() { "Reproductor".into() } else { snap.app.clone() },
                snap.title,
                if snap.artist.is_empty() { String::new() } else { format!(" de {}", snap.artist) },
                if snap.playing { "sonando" } else { "en pausa" }
            )
        });
    }
    let target = match action {
        "volume" => format!("volumen {:.0} %", volume.unwrap_or(0.0).clamp(0.0, 1.0) * 100.0),
        other => other.to_string(),
    };
    if !policy::approve(app, "media_control", &target).await {
        return Outcome::err("The owner declined this action.");
    }
    let action = action.to_string();
    match tauri::async_runtime::spawn_blocking(move || crate::media::control(&action, volume)).await {
        Ok(Ok(())) => Outcome::ok("Hecho."),
        Ok(Err(e)) => Outcome::err(e),
        Err(e) => Outcome::err(e.to_string()),
    }
}

async fn delegate_to_cursor(app: &AppHandle, project: &str, task: &str) -> Outcome {
    if task.trim().is_empty() {
        return Outcome::err("Falta la tarea.");
    }
    let path = match resolve_project(project, &projects()) {
        Ok(p) => p,
        Err(e) => return Outcome::err(e),
    };
    if forbidden_project(&path, &crate::selfmod::guard::repo_root()) {
        return Outcome::err(
            "Ese proyecto es (o contiene) tu propio código: un encargo a Cursor no puede tocarlo. Para cambiarte a ti misma usa las herramientas evolve_*.",
        );
    }
    let settings = app.state::<Shared>().settings.lock().unwrap().clone();
    let mut ep = match Endpoint::new(&settings, Provider::Cursor) {
        Ok(ep) => ep,
        Err(e) => return Outcome::err(e),
    };
    if ep.key().is_none() {
        return Outcome::err("Falta la clave de Cursor (Ajustes → Asistente).");
    }
    let place = path.display().to_string();
    let target = format!("Cursor en {place}: {}", one_line(task, 300));
    if !policy::approve(app, "delegate_to_cursor", &target).await {
        return Outcome::err("The owner declined this errand. Ask what they want instead.");
    }
    ep.model = match crate::cursor::model(&ep).await {
        Ok(m) => m,
        Err(e) => return Outcome::err(e),
    };
    let id = start(app, "cursor", "Cursor", task, Some(place.clone()));
    let (app2, task) = (app.clone(), task.to_string());
    tauri::async_runtime::spawn(async move {
        let cancel = AtomicBool::new(false);
        let app3 = app2.clone();
        let mut on_progress = move |line: &str| progress(&app3, id, line);
        let result = crate::cursor::delegate(&ep, &path, &task, &cancel, &mut on_progress).await;
        match result {
            Ok(text) => finish(&app2, id, "done", &text),
            Err(e) => finish(&app2, id, "error", &e),
        }
    });
    Outcome::ok(format!(
        "Encargo #{id} iniciado: Cursor trabaja en {place}. Su informe llegará a este chat cuando termine; dile al dueño que ya está en marcha."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(file_uri_path("file:///c%3A/Users/Dell/My%20App").as_deref(), Some("c:/Users/Dell/My App"));
        assert_eq!(file_uri_path("file:///c%3A/x/%C3%B1and%C3%BA").as_deref(), Some("c:/x/ñandú"));
        assert_eq!(file_uri_path("vscode-remote://ssh/x"), None);
    }

    #[test]
    fn projects_are_found_by_path_or_by_name() {
        let tmp = std::env::temp_dir().join(format!("aria-proj-{}", std::process::id()));
        let a = tmp.join("tienda");
        let b = tmp.join("otro").join("Tienda");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let known = vec![a.clone(), tmp.join("coucou")];
        assert_eq!(resolve_project("TIENDA", &known).unwrap(), a);
        assert_eq!(resolve_project(&a.display().to_string(), &[]).unwrap(), a);
        assert!(resolve_project("nada", &known).is_err());
        assert!(resolve_project("", &known).is_err());
        assert!(resolve_project("tienda", &[a.clone(), b.clone()]).unwrap_err().contains("varios"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn errands_never_touch_arias_own_code() {
        let repo = Path::new(r"C:\Users\Dell\coucou");
        assert!(forbidden_project(Path::new(r"C:\Users\Dell\coucou"), repo));
        assert!(forbidden_project(Path::new(r"c:/users/dell/coucou/windows"), repo));
        assert!(forbidden_project(Path::new(r"C:\Users\Dell"), repo));
        assert!(!forbidden_project(Path::new(r"C:\Users\Dell\tienda"), repo));
        assert!(!forbidden_project(Path::new(r"C:\Users\Dell\coucou2"), repo));
    }

    #[test]
    fn a_report_names_the_worker_the_task_and_the_result() {
        let e = Errand {
            id: 7,
            worker: "cursor".into(),
            worker_name: "Cursor".into(),
            title: "Arreglar el botón".into(),
            project: Some("C:/tienda".into()),
            status: "done".into(),
            started_ms: 0,
            ended_ms: Some(1),
            last: String::new(),
            result: "Cambié boton.tsx.".into(),
        };
        let text = report_text(&e);
        assert!(text.starts_with("[Informe del encargo #7] Cursor terminó «Arreglar el botón» (proyecto C:/tienda)."));
        assert!(text.contains("Cambié boton.tsx."));
        assert!(!Errand { status: "done".into(), ..e.clone() }.open());
        assert!(Errand { status: "needs".into(), ..e }.open());
    }

    #[test]
    fn long_text_is_one_short_line() {
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("abcdef", 3), "abc…");
    }
}
