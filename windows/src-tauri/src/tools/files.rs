// Files: read, list and search freely; write only after a click, and never
// inside the protected core (selfmod::guard).

use std::path::Path;

use serde_json::{json, Value};

use super::{expand_path, schema, Outcome, Tool};
use crate::policy::Risk;
use crate::selfmod::guard;

const MAX_READ: u64 = 2_000_000;
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "$Recycle.Bin", "AppData"];

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "read_file",
            description: "Read a text file. Relative paths and ~ start at the home folder. Optional 1-based line range.",
            risk: Risk::Read,
            schema: || schema(json!({
                "path": { "type": "string" },
                "from_line": { "type": "integer" },
                "to_line": { "type": "integer" },
            }), &["path"]),
        },
        Tool {
            name: "list_dir",
            description: "List a folder: sub-folders end with /, files show their size.",
            risk: Risk::Read,
            schema: || schema(json!({ "path": { "type": "string" } }), &["path"]),
        },
        Tool {
            name: "find_files",
            description: "Find files whose name contains the given text (case-insensitive), under a folder. Skips node_modules, .git, target.",
            risk: Risk::Read,
            schema: || schema(json!({
                "root": { "type": "string" },
                "name_contains": { "type": "string" },
            }), &["root", "name_contains"]),
        },
        Tool {
            name: "search_text",
            description: "Search text files under a folder for lines containing the given text (case-insensitive). Optional extension filter like \"rs\" or \"ts\".",
            risk: Risk::Read,
            schema: || schema(json!({
                "root": { "type": "string" },
                "text": { "type": "string" },
                "extension": { "type": "string" },
            }), &["root", "text"]),
        },
        Tool {
            name: "write_file",
            description: "Create or overwrite a text file (mode \"overwrite\", default) or append to it (mode \"append\"). Needs the owner's click.",
            risk: Risk::Act,
            schema: || schema(json!({
                "path": { "type": "string" },
                "content": { "type": "string" },
                "mode": { "type": "string", "enum": ["overwrite", "append"] },
            }), &["path", "content"]),
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "read_file" | "list_dir" | "find_files" | "search_text" | "write_file")
}

pub async fn run(name: &str, input: &Value) -> Outcome {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default();
    match name {
        "read_file" => read_file(&expand_path(s("path")), input),
        "list_dir" => list_dir(&expand_path(s("path"))),
        "find_files" => find_files(&expand_path(s("root")), s("name_contains")),
        "search_text" => search_text(&expand_path(s("root")), s("text"), s("extension")),
        "write_file" => write_file(&expand_path(s("path")), s("content"), s("mode") == "append"),
        _ => Outcome::err("unknown file tool"),
    }
}

pub fn read_text(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.is_dir() {
        return Err(format!("{} is a folder — use list_dir", path.display()));
    }
    if meta.len() > MAX_READ {
        return Err(format!("{} is too large ({} bytes)", path.display(), meta.len()));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.iter().take(8000).any(|b| *b == 0) {
        return Err(format!("{} looks like a binary file", path.display()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_file(path: &Path, input: &Value) -> Outcome {
    let text = match read_text(path) {
        Ok(t) => t,
        Err(e) => return Outcome::err(e),
    };
    let from = input.get("from_line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let to = input.get("to_line").and_then(Value::as_u64).map(|n| n as usize);
    if from == 1 && to.is_none() {
        return Outcome::ok(text);
    }
    let lines: Vec<String> = text
        .lines()
        .enumerate()
        .skip(from - 1)
        .take(to.map(|t| t.saturating_sub(from - 1)).unwrap_or(usize::MAX))
        .map(|(i, l)| format!("{:>5}| {l}", i + 1))
        .collect();
    Outcome::ok(lines.join("\n"))
}

fn list_dir(path: &Path) -> Outcome {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => return Outcome::err(format!("{}: {e}", path.display())),
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.metadata() {
            Ok(m) if m.is_dir() => dirs.push(format!("{name}/")),
            Ok(m) => files.push(format!("{name}  ({} bytes)", m.len())),
            Err(_) => files.push(name),
        }
    }
    dirs.sort_by_key(|d| d.to_lowercase());
    files.sort_by_key(|f| f.to_lowercase());
    let total = dirs.len() + files.len();
    let mut out: Vec<String> = dirs.into_iter().chain(files).take(500).collect();
    if total > 500 {
        out.push(format!("… and {} more", total - 500));
    }
    Outcome::ok(format!("{}\n{}", path.display(), out.join("\n")))
}

/// Depth-first walk with the usual heavy folders skipped.
pub fn walk(root: &Path, max_depth: usize, visit: &mut dyn FnMut(&Path) -> bool) {
    fn go(dir: &Path, depth: usize, max: usize, visit: &mut dyn FnMut(&Path) -> bool) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else { return true };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                let name = entry.file_name();
                if SKIP_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s)) || depth >= max {
                    continue;
                }
                if !go(&path, depth + 1, max, visit) {
                    return false;
                }
            } else if kind.is_file() && !visit(&path) {
                return false;
            }
        }
        true
    }
    go(root, 0, max_depth, visit);
}

fn find_files(root: &Path, needle: &str) -> Outcome {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return Outcome::err("name_contains is empty");
    }
    let mut hits = Vec::new();
    walk(root, 10, &mut |p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        if name.contains(&needle) {
            hits.push(p.display().to_string());
        }
        hits.len() < 200
    });
    if hits.is_empty() {
        Outcome::ok("No file found.")
    } else {
        Outcome::ok(hits.join("\n"))
    }
}

pub fn search_text(root: &Path, needle: &str, extension: &str) -> Outcome {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return Outcome::err("text is empty");
    }
    let ext = extension.trim_start_matches('.').to_lowercase();
    let mut hits = Vec::new();
    walk(root, 12, &mut |p| {
        if !ext.is_empty() {
            let e = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if e != ext {
                return true;
            }
        }
        if std::fs::metadata(p).map(|m| m.len() > 1_000_000).unwrap_or(true) {
            return true;
        }
        if let Ok(text) = std::fs::read_to_string(p) {
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    let line: String = line.trim().chars().take(200).collect();
                    hits.push(format!("{}:{}: {line}", p.display(), i + 1));
                    if hits.len() >= 150 {
                        return false;
                    }
                }
            }
        }
        true
    });
    if hits.is_empty() {
        Outcome::ok("No match.")
    } else {
        Outcome::ok(hits.join("\n"))
    }
}

fn write_file(path: &Path, content: &str, append: bool) -> Outcome {
    if let Err(why) = guard::check_write(path) {
        return Outcome::err(why);
    }
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Outcome::err(format!("{}: {e}", parent.display()));
        }
    }
    let result = if append {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    } else {
        std::fs::write(path, content)
    };
    match result {
        Ok(()) => Outcome::ok(format!("Wrote {} ({} chars).", path.display(), content.chars().count())),
        Err(e) => Outcome::err(format!("{}: {e}", path.display())),
    }
}
