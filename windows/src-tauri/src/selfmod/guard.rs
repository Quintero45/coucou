// The protected core. Mochi may never write these files — not with write_file,
// not through an evolve diff — and at startup their contents are compared with
// the hashes build.rs baked into this binary, so a change made behind its back
// is noticed and self-evolution refuses to run until a human looks.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

#[path = "core_list.rs"]
mod core_list;
#[path = "sha256.rs"]
pub mod sha256;

pub use core_list::PROTECTED;

include!(concat!(env!("OUT_DIR"), "/core_hashes.rs"));

/// The repository this binary was built from (where `windows/` lives).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Absolute, `..`-free, `/`-separated, lower-case on Windows: comparable.
fn norm(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut parts: Vec<String> = Vec::new();
    let mut prefix = String::new();
    for c in absolute.components() {
        match c {
            Component::Prefix(p) => prefix = p.as_os_str().to_string_lossy().into_owned(),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
        }
    }
    let joined = format!("{prefix}/{}", parts.join("/"));
    if cfg!(windows) { joined.to_lowercase() } else { joined }
}

fn within(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{}/", dir.trim_end_matches('/')))
}

/// `rel` is repository-relative with `/` separators.
pub fn is_protected_rel(rel: &str) -> bool {
    let rel = rel.trim_start_matches("./").replace('\\', "/");
    let rel = if cfg!(windows) { rel.to_lowercase() } else { rel };
    PROTECTED.iter().any(|p| {
        let p = if cfg!(windows) { p.to_lowercase() } else { p.to_string() };
        within(&rel, &p)
    })
}

/// Files ARIA's write_file must leave alone: its own source (changed only
/// through evolve), its installed binaries, its own config, and the agents'
/// hook configs (changed only through the diff-and-confirm installers).
fn forbidden_dirs() -> Vec<(String, &'static str)> {
    let home = crate::platform::home_dir();
    let config = crate::settings::config_dir();
    let mut out = vec![
        (norm(&repo_root().join("windows")), "ARIA's own source — use the evolve tools, which show the owner a diff"),
        (norm(&repo_root().join("CLAUDE.md")), "part of the protected core"),
        (norm(&super::evolve::worktree_dir()), "the evolution worktree — use evolve_edit"),
        (norm(&crate::settings::local_dir().join("bin")), "the agent relay"),
        (norm(&config.join("settings.json")), "ARIA's settings — the owner changes them in Settings"),
        (norm(&config.join("mcp.json")), "ARIA's connections — the owner changes them in Settings → Connections"),
        (norm(&config.join("skills")), "installed skills — use create_skill, which the owner approves"),
        (norm(&home.join(".claude").join("settings.json")), "Claude Code's hooks — changed only through Settings"),
        (norm(&home.join(".cursor").join("hooks.json")), "Cursor's hooks — changed only through Settings"),
        (norm(&home.join(".cursor").join("mcp.json")), "Cursor's MCP servers — changed only through Settings"),
        (norm(&home.join(".codex").join("hooks.json")), "Codex's hooks — changed only through Settings"),
        (norm(&home.join(".gemini").join("settings.json")), "Gemini CLI's hooks — changed only through Settings"),
    ];
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        out.push((norm(&dir), "ARIA's installed program"));
    }
    out
}

/// Ok, or why this path must not be written.
pub fn check_write(path: &Path) -> Result<(), String> {
    let target = norm(path);
    for (dir, why) in forbidden_dirs() {
        if within(&target, &dir) {
            return Err(format!("Refused: {} is {why}.", path.display()));
        }
    }
    Ok(())
}

/// Protected files whose contents differ from what this binary was built with.
pub fn tampered() -> &'static [String] {
    static RESULT: OnceLock<Vec<String>> = OnceLock::new();
    RESULT.get_or_init(|| {
        let root = repo_root();
        CORE_HASHES
            .iter()
            .filter(|(rel, hash)| match std::fs::read(root.join(rel)) {
                Ok(bytes) => sha256::file_hash(&bytes) != *hash,
                // An installed build without its sources has nothing to compare.
                Err(_) => false,
            })
            .map(|(rel, _)| rel.to_string())
            .collect()
    })
}

/// Startup check; logs and returns the files that changed since the build.
pub fn verify_at_startup() {
    let changed = tampered();
    if changed.is_empty() {
        crate::log::line(format!("core guard: {} protected files verified", CORE_HASHES.len()));
    } else {
        crate::log::line(format!(
            "core guard: protected files changed since this build: {} — self-evolution is locked until the app is rebuilt by its owner",
            changed.join(", ")
        ));
    }
}

/// Which of these repository-relative paths are in the protected core.
pub fn core_paths_in(paths: &[String]) -> Vec<String> {
    paths.iter().filter(|p| is_protected_rel(p)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let long = vec![b'a'; 1000];
        assert_eq!(
            sha256::sha256_hex(&long),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn core_paths_are_recognised() {
        assert!(is_protected_rel("windows/src-tauri/src/policy.rs"));
        assert!(is_protected_rel("windows/src-tauri/src/selfmod/evolve.rs"));
        assert!(is_protected_rel("windows/hook/src/main.rs"));
        assert!(is_protected_rel("CLAUDE.md"));
        assert!(!is_protected_rel("windows/src-tauri/src/agent.rs"));
        assert!(!is_protected_rel("windows/src/views/chat.ts"));
        assert!(!is_protected_rel("windows/src-tauri/src/policy.rs.bak"));
    }

    #[test]
    fn writes_into_the_source_and_configs_are_refused() {
        assert!(check_write(&repo_root().join("windows/src-tauri/src/agent.rs")).is_err());
        assert!(check_write(&repo_root().join("windows/src/../src-tauri/src/policy.rs")).is_err());
        assert!(check_write(&crate::platform::home_dir().join(".cursor/hooks.json")).is_err());
        assert!(check_write(&crate::platform::home_dir().join("Documents/notes.txt")).is_ok());
    }

    #[test]
    fn the_build_hashed_the_core() {
        assert!(CORE_HASHES.iter().any(|(rel, _)| *rel == "windows/src-tauri/src/policy.rs"));
        assert!(tampered().is_empty(), "changed: {:?}", tampered());
    }
}
