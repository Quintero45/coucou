// Self-evolution — ARIA changing its own source, always in a git worktree, the
// checks run, and the diff shown to the owner before anything is applied.
//
// The loop the model drives:
//   evolve_start  — opens a worktree on a new branch off the current HEAD.
//   code_search / read_file / list_dir — explore (the normal read tools work too).
//   evolve_edit   — write a file in the worktree; the core guard refuses protected
//                   paths, so policy.rs, secrets.rs, the hook, this module… can't change.
//   evolve_check  — cargo test, tsc --noEmit, the front-end build.
//   evolve_diff   — the full diff so far.
//   evolve_apply  — show the diff on an approval card; on the click, tag the
//                   current HEAD, commit, and fast-forward the branch.
//   evolve_rebuild — after another click: build the release, keep the running
//                   exe as aria.prev.exe, and hand over to a watchdog that
//                   starts the new one and puts the old one back if the new one
//                   does not report healthy within a minute. In a dev build,
//                   `tauri dev` already rebuilds on its own.
//   evolve_discard — throw the worktree away.
//
// Only one evolution at a time. If the protected core was changed behind the
// build's back (guard::tampered), evolve_start refuses until a human rebuilds.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;

use super::{arg, guard};
use crate::policy;
use crate::providers::{ToolCall, ToolSpec};
use crate::tools::{files, Outcome};
use crate::{log, platform};

const CHECK_TIMEOUT: u64 = 1800;

struct Session {
    branch: String,
    worktree: PathBuf,
    task: String,
    edited: Vec<String>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Where evolutions happen: inside the repository so the build finds its
/// relative paths, ignored by git, and off-limits to write_file (guard.rs).
pub fn worktree_dir() -> PathBuf {
    guard::repo_root().join(".aria-evolve")
}

fn git_root() -> Option<PathBuf> {
    let root = guard::repo_root();
    root.join(".git").exists().then_some(root)
}

async fn git(dir: &Path, args: &[&str], timeout: u64) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let child = cmd.spawn().map_err(|e| format!("git not available: {e}"))?;
    let out = tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output())
        .await
        .map_err(|_| "git timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(format!("{}{}", stdout, String::from_utf8_lossy(&out.stderr)))
    }
}

pub fn specs() -> Vec<ToolSpec> {
    let obj = |props: Value, req: &[&str]| json!({ "type": "object", "properties": props, "required": req });
    vec![
        ToolSpec {
            name: "evolve_start".into(),
            description: "Begin changing your own source code to carry out a task the owner asked for. Opens an isolated \
git worktree on a new branch. Explore with code_search and read_file, change files with evolve_edit, verify with \
evolve_check, then evolve_apply (the owner approves the diff). You cannot touch your protected core."
                .into(),
            schema: obj(json!({ "task": { "type": "string" } }), &["task"]),
        },
        ToolSpec {
            name: "code_search".into(),
            description: "Search your source code for text (optionally filtered by extension like \"rs\" or \"ts\"). Use during an evolution.".into(),
            schema: obj(json!({ "text": { "type": "string" }, "extension": { "type": "string" } }), &["text"]),
        },
        ToolSpec {
            name: "evolve_edit".into(),
            description: "Create or overwrite a file (path relative to the repo root) in the evolution worktree. Refused for protected-core files.".into(),
            schema: obj(json!({ "path": { "type": "string" }, "content": { "type": "string" } }), &["path", "content"]),
        },
        ToolSpec {
            name: "evolve_check".into(),
            description: "Run the checks in the worktree: \"rust\" (cargo test), \"ts\" (tsc --noEmit), \"build\" (front-end build), or \"all\".".into(),
            schema: obj(json!({ "which": { "type": "string", "enum": ["rust", "ts", "build", "all"] } }), &[]),
        },
        ToolSpec {
            name: "evolve_diff".into(),
            description: "Show the full diff of the evolution so far.".into(),
            schema: obj(json!({}), &[]),
        },
        ToolSpec {
            name: "evolve_apply".into(),
            description: "Finish the evolution: show the owner the diff for approval, then commit it and merge it into the branch. Tell the owner to rebuild to run the new version.".into(),
            schema: obj(json!({ "summary": { "type": "string" } }), &["summary"]),
        },
        ToolSpec {
            name: "evolve_rebuild".into(),
            description: "After evolve_apply: rebuild yourself and restart into the new version (the owner approves). If the new version does not start healthy, the previous one is restored automatically.".into(),
            schema: obj(json!({}), &[]),
        },
        ToolSpec {
            name: "evolve_discard".into(),
            description: "Throw away the current evolution worktree and its changes.".into(),
            schema: obj(json!({}), &[]),
        },
    ]
}

pub async fn run(app: &AppHandle, call: &ToolCall) -> Option<Outcome> {
    let out = match call.name.as_str() {
        "evolve_start" => {
            let task = arg(&call.input, "task");
            if policy::approve(app, "evolve_start", &format!("Start changing ARIA's own code: {task}")).await {
                start(task).await
            } else {
                Outcome::err("The owner declined. Do not change your code for this.")
            }
        }
        "code_search" => {
            policy::audit("code_search", "read", arg(&call.input, "text"));
            code_search(arg(&call.input, "text"), arg(&call.input, "extension"))
        }
        "evolve_edit" => {
            policy::audit("evolve_edit", "worktree", arg(&call.input, "path"));
            edit(arg(&call.input, "path"), arg(&call.input, "content"))
        }
        // The checks run code ARIA wrote (tests, build scripts): a click first.
        "evolve_check" => {
            let which = arg(&call.input, "which");
            let target = format!("Build and test the modified code in the worktree ({})", if which.is_empty() { "all" } else { which });
            if policy::approve(app, "evolve_check", &target).await {
                check(which).await
            } else {
                Outcome::err("The owner declined running the checks.")
            }
        }
        "evolve_diff" => diff().await.map(Outcome::ok).unwrap_or_else(Outcome::err),
        "evolve_apply" => apply(app, arg(&call.input, "summary")).await,
        "evolve_rebuild" => rebuild(app).await,
        "evolve_discard" => discard().await,
        _ => return None,
    };
    Some(out)
}

fn stamp() -> String {
    let t = platform::local_time();
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

async fn start(task: &str) -> Outcome {
    let task = task.trim();
    if task.is_empty() {
        return Outcome::err("say what the change is for");
    }
    let Some(root) = git_root() else {
        return Outcome::err("This build has no source repository, so it cannot change its own code.");
    };
    let tampered = guard::tampered();
    if !tampered.is_empty() {
        return Outcome::err(format!(
            "Protected-core files changed since this build ({}). Ask the owner to rebuild from a clean checkout before evolving.",
            tampered.join(", ")
        ));
    }
    if SESSION.lock().unwrap().is_some() {
        return Outcome::err("An evolution is already open. Use evolve_diff, evolve_apply or evolve_discard first.");
    }
    // The worktree starts from HEAD and is fast-forwarded back: uncommitted work
    // in the owner's checkout would be missing from it and could block the merge.
    match git(&root, &["status", "--porcelain", "--untracked-files=no"], 60).await {
        Ok(s) if !s.trim().is_empty() => {
            return Outcome::err(
                "The source checkout has uncommitted changes. Ask the owner to commit them first, then try again.",
            )
        }
        Err(e) => return Outcome::err(format!("git status failed: {e}")),
        _ => {}
    }
    let branch = format!("evolve/{}", stamp());
    let worktree = worktree_dir();
    let wt = worktree.to_string_lossy().to_string();
    unlink_modules(&worktree);
    let _ = git(&root, &["worktree", "remove", "--force", &wt], 60).await;
    let _ = tokio::fs::remove_dir_all(&worktree).await;
    let _ = git(&root, &["worktree", "prune"], 30).await;
    if let Err(e) = git(&root, &["worktree", "add", "-b", &branch, &wt, "HEAD"], 120).await {
        return Outcome::err(format!("could not open a worktree: {e}"));
    }
    // The checks need the front end's packages; share them instead of reinstalling.
    #[cfg(windows)]
    {
        let modules = root.join("windows").join("node_modules");
        if modules.is_dir() {
            let link = worktree.join("windows").join("node_modules");
            let _ = crate::tools::system::run_shell(
                &format!(
                    "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
                    link.display().to_string().replace('\'', "''"),
                    modules.display().to_string().replace('\'', "''")
                ),
                None,
                30,
                None,
            )
            .await;
        }
    }
    *SESSION.lock().unwrap() = Some(Session { branch: branch.clone(), worktree, task: task.to_string(), edited: Vec::new() });
    log::line(format!("evolve: started {branch} — {task}"));
    Outcome::ok(format!(
        "Evolution started on {branch}. Explore with code_search/read_file, change files with evolve_edit (repo-relative paths), \
run evolve_check, then evolve_apply. Task: {task}"
    ))
}

/// Maps a repo-relative path into the worktree, rejecting escapes and the core.
fn resolve_in_worktree(session: &Session, rel: &str) -> Result<PathBuf, String> {
    let rel = rel.trim().replace('\\', "/");
    let rel = rel.trim_start_matches('/');
    if rel.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err("path must be inside the repository".into());
    }
    if guard::is_protected_rel(rel) {
        return Err(format!("{rel} is part of the protected core and cannot be changed by evolution."));
    }
    Ok(session.worktree.join(rel))
}

fn edit(path: &str, content: &str) -> Outcome {
    let mut guard_session = SESSION.lock().unwrap();
    let Some(session) = guard_session.as_mut() else {
        return Outcome::err("No evolution is open. Call evolve_start first.");
    };
    let full = match resolve_in_worktree(session, path) {
        Ok(p) => p,
        Err(e) => return Outcome::err(e),
    };
    if let Some(parent) = full.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Outcome::err(e.to_string());
        }
    }
    if let Err(e) = std::fs::write(&full, content) {
        return Outcome::err(format!("{path}: {e}"));
    }
    let rel = path.trim().replace('\\', "/");
    if !session.edited.contains(&rel) {
        session.edited.push(rel);
    }
    Outcome::ok(format!("Wrote {path} in the worktree ({} chars). Run evolve_check when ready.", content.chars().count()))
}

fn code_search(text: &str, extension: &str) -> Outcome {
    let root = SESSION.lock().unwrap().as_ref().map(|s| s.worktree.clone()).unwrap_or_else(guard::repo_root);
    files::search_text(&root, text, extension)
}

async fn worktree() -> Result<PathBuf, String> {
    SESSION.lock().unwrap().as_ref().map(|s| s.worktree.clone()).ok_or_else(|| "No evolution is open.".into())
}

async fn check(which: &str) -> Outcome {
    let wt = match worktree().await {
        Ok(p) => p,
        Err(e) => return Outcome::err(e),
    };
    let windows = wt.join("windows");
    // The app bundles the release relay as a resource, so it has to exist first.
    const RUST: &str = "cargo build -q -p aria-hook --release; if ($LASTEXITCODE -eq 0) { cargo test --workspace }";
    let steps: Vec<(&str, &str, PathBuf)> = match which {
        "rust" => vec![("cargo test", RUST, windows.clone())],
        "ts" => vec![("tsc", "npx --no-install tsc --noEmit", windows.clone())],
        "build" => vec![("front-end build", "npm run build", windows.clone())],
        _ => vec![
            ("cargo test", RUST, windows.clone()),
            ("tsc", "npx --no-install tsc --noEmit", windows.clone()),
            ("front-end build", "npm run build", windows.clone()),
        ],
    };
    let mut report = String::new();
    let mut ok = true;
    for (label, script, dir) in steps {
        if !dir.exists() {
            continue;
        }
        let outcome = crate::tools::system::run_shell(script, Some(dir), CHECK_TIMEOUT, None).await;
        let tail: String = outcome.text.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        report.push_str(&format!("── {label}: {} ──\n{tail}\n\n", if outcome.is_error { "FAILED" } else { "ok" }));
        ok &= !outcome.is_error;
    }
    if report.is_empty() {
        report.push_str("nothing to check");
    }
    if ok { Outcome::ok(report) } else { Outcome::err(report) }
}

async fn diff() -> Result<String, String> {
    let wt = worktree().await?;
    let text = git(&wt, &["--no-pager", "diff", "HEAD"], 60).await?;
    Ok(if text.trim().is_empty() { "No changes yet.".into() } else { text })
}

async fn apply(app: &AppHandle, summary: &str) -> Outcome {
    let summary = summary.trim();
    if summary.is_empty() {
        return Outcome::err("give a short summary of the change");
    }
    let (wt, branch, task, edited) = {
        let guard_session = SESSION.lock().unwrap();
        let Some(s) = guard_session.as_ref() else {
            return Outcome::err("No evolution is open.");
        };
        (s.worktree.clone(), s.branch.clone(), s.task.clone(), s.edited.clone())
    };
    // Stage everything so new files are in the diff too, then judge the diff
    // itself — not the list of edits — against the protected core.
    if let Err(e) = git(&wt, &["add", "-A"], 60).await {
        return Outcome::err(e);
    }
    let changed: Vec<String> = match git(&wt, &["diff", "--cached", "--name-only", "HEAD"], 60).await {
        Ok(names) => names.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        Err(e) => return Outcome::err(e),
    };
    if changed.is_empty() {
        return Outcome::err("There are no changes to apply.");
    }
    let core = guard::core_paths_in(&changed);
    if !core.is_empty() {
        return Outcome::err(format!(
            "The change touches the protected core ({}); it cannot be applied. Undo those files or evolve_discard.",
            core.join(", ")
        ));
    }
    let full_diff = match git(&wt, &["--no-pager", "diff", "--cached", "HEAD"], 60).await {
        Ok(d) => d,
        Err(e) => return Outcome::err(e),
    };
    let _ = edited;

    let target = format!("Apply evolution to ARIA's own code: {summary}\n({} file(s): {})", changed.len(), changed.join(", "));
    let review: String = full_diff.chars().take(100_000).collect();
    if !policy::approve_with_detail(app, "evolve_apply", &target, Some(&review)).await {
        return Outcome::err("The owner declined the change. The worktree is kept; adjust it or evolve_discard.");
    }

    let Some(root) = git_root() else { return Outcome::err("no repository") };
    let message = format!("ARIA: {summary}\n\nTask: {task}");
    let tag = format!("aria-pre-{}", stamp());
    let run = async {
        git(&wt, &["-c", "user.name=ARIA", "-c", "user.email=aria@aria.local", "commit", "-m", &message], 60).await?;
        git(&root, &["tag", &tag], 30).await?;
        // Fast-forward only: if the owner's branch moved meanwhile, nothing is merged.
        git(&root, &["merge", "--ff-only", &branch], 120).await
    };
    match run.await {
        Ok(_) => {
            cleanup(&root, &wt, &branch, false).await;
            log::line(format!("evolve: applied {branch} — {summary} (previous state tagged {tag})"));
            let next = if cfg!(debug_assertions) {
                "This is a dev build: `npm run tauri dev` picks the change up and restarts by itself."
            } else {
                "Call evolve_rebuild to build and restart into it."
            };
            Outcome::ok(format!(
                "Applied: {summary}. Committed and merged; the previous state is tagged {tag} \
(`git reset --hard {tag}` undoes it). {next}"
            ))
        }
        Err(e) => {
            let _ = git(&root, &["tag", "-d", &tag], 30).await;
            Outcome::err(format!("could not apply the change: {e}. The worktree is kept."))
        }
    }
}

fn pending_path() -> PathBuf {
    crate::settings::local_dir().join("evolve-pending.json")
}

fn healthy_path() -> PathBuf {
    crate::settings::local_dir().join("evolve-healthy")
}

/// Builds the release, swaps the executable, and restarts through a watchdog
/// that rolls back if the new version does not come up healthy.
async fn rebuild(app: &AppHandle) -> Outcome {
    if cfg!(debug_assertions) {
        return Outcome::ok("This is a dev build: `npm run tauri dev` rebuilds and restarts on its own when the source changes.");
    }
    if !cfg!(windows) {
        return Outcome::err("Self-rebuild is only wired for Windows.");
    }
    let Some(root) = git_root() else { return Outcome::err("This build has no source repository.") };
    let Ok(exe) = std::env::current_exe() else { return Outcome::err("cannot locate the running program") };
    let built = root.join("windows").join("target").join("release").join("aria.exe");
    let prev = exe.with_file_name("aria.prev.exe");

    let target = format!("Rebuild ARIA from {} and restart into it (previous version kept as {})", root.display(), prev.display());
    if !policy::approve(app, "evolve_rebuild", &target).await {
        return Outcome::err("The owner declined the rebuild.");
    }

    // A running exe can be renamed but not overwritten: move it aside first,
    // since the build may write to the very same path.
    let _ = std::fs::remove_file(&prev);
    if let Err(e) = std::fs::rename(&exe, &prev) {
        return Outcome::err(format!("could not set the current version aside: {e}"));
    }
    let build = crate::tools::system::run_shell(
        "npm run tauri build -- --no-bundle",
        Some(root.join("windows")),
        CHECK_TIMEOUT,
        None,
    )
    .await;
    let restore = || {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&prev, &exe);
    };
    if build.is_error {
        restore();
        let tail: String = build.text.chars().rev().take(3000).collect::<Vec<_>>().into_iter().rev().collect();
        return Outcome::err(format!("The build failed; the current version stays.\n{tail}"));
    }
    if built != exe {
        if let Err(e) = std::fs::copy(&built, &exe) {
            restore();
            return Outcome::err(format!("could not install the new build: {e}"));
        }
    }

    let _ = std::fs::remove_file(healthy_path());
    let pending = json!({ "exe": exe, "prev": prev, "pid": std::process::id() });
    let _ = std::fs::write(pending_path(), pending.to_string());
    if let Err(e) = spawn_watchdog(&exe, &prev) {
        restore();
        return Outcome::err(format!("could not start the watchdog: {e}"));
    }
    log::line("evolve: rebuilt, handing over to the watchdog");
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        handle.exit(0);
    });
    Outcome::ok("Built. Restarting into the new version now.")
}

/// Waits for this process to exit, starts the new exe, and restores the
/// previous one if no health mark appears within 60 seconds.
fn spawn_watchdog(exe: &Path, prev: &Path) -> Result<(), String> {
    let q = |p: &Path| p.display().to_string().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Wait-Process -Id {pid} -Timeout 30; \
         $exe='{exe}'; $prev='{prev}'; $ok='{ok}'; $pending='{pending}'; \
         Start-Process -FilePath $exe; \
         $deadline=(Get-Date).AddSeconds(60); \
         while ((Get-Date) -lt $deadline -and -not (Test-Path $ok)) {{ Start-Sleep -Seconds 2 }}; \
         if (-not (Test-Path $ok)) {{ \
           Get-Process | Where-Object {{ $_.Path -eq $exe }} | Stop-Process -Force; \
           Start-Sleep -Seconds 1; \
           Move-Item -Force $exe ($exe + '.failed'); Move-Item -Force $prev $exe; \
           Add-Content -Path ($pending + '.log') -Value 'rolled back'; \
           Start-Process -FilePath $exe \
         }}; \
         Remove-Item -Force $pending, $ok",
        pid = std::process::id(),
        exe = q(exe),
        prev = q(prev),
        ok = q(&healthy_path()),
        pending = q(&pending_path()),
    );
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | DETACHED_PROCESS: outlives us, shows nothing.
        cmd.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Called at startup: a version started by evolve_rebuild marks itself healthy
/// once it has run for a while.
pub fn startup_health() {
    if !pending_path().exists() {
        return;
    }
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(Duration::from_secs(15)).await;
        let _ = std::fs::write(healthy_path(), "ok");
        log::line("evolve: new version healthy");
    });
}

async fn discard() -> Outcome {
    let Some(root) = git_root() else { return Outcome::err("no repository") };
    let session = SESSION.lock().unwrap().take();
    let Some(s) = session else { return Outcome::err("No evolution is open.") };
    cleanup(&root, &s.worktree, &s.branch, true).await;
    log::line(format!("evolve: discarded {}", s.branch));
    Outcome::ok("Evolution discarded.")
}

/// Removes the node_modules junction itself (never its target) so deleting the
/// worktree cannot reach into the real packages.
fn unlink_modules(worktree: &Path) {
    let link = worktree.join("windows").join("node_modules");
    if is_link(&link) {
        let _ = std::fs::remove_dir(&link);
    }
}

#[cfg(windows)]
fn is_link(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    path.symlink_metadata().map(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0).unwrap_or(false)
}

#[cfg(not(windows))]
fn is_link(path: &Path) -> bool {
    path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

async fn cleanup(root: &Path, worktree: &Path, branch: &str, delete_branch: bool) {
    let wt = worktree.to_string_lossy().to_string();
    unlink_modules(worktree);
    let _ = git(root, &["worktree", "remove", "--force", &wt], 60).await;
    let _ = tokio::fs::remove_dir_all(worktree).await;
    if delete_branch {
        let _ = git(root, &["branch", "-D", branch], 30).await;
    }
    *SESSION.lock().unwrap() = None;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn unlinking_the_modules_junction_keeps_its_target() {
        let base = std::env::temp_dir().join(format!("aria-evolve-test-{}", std::process::id()));
        let target = base.join("real_modules");
        let worktree = base.join("wt");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(worktree.join("windows")).unwrap();
        std::fs::write(target.join("keep.txt"), "x").unwrap();
        let link = worktree.join("windows").join("node_modules");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success());

        unlink_modules(&worktree);
        assert!(!link.exists());
        assert!(target.join("keep.txt").exists());

        unlink_modules(&base);
        assert!(target.join("keep.txt").exists(), "a real folder is never removed");
        let _ = std::fs::remove_dir_all(&base);
    }
}
