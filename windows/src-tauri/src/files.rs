// Dropped files are copied into %LOCALAPPDATA%\Coucou\inbox so the original is
// never touched and the copy survives the drag source going away.
// The inbox is swept of anything older than a week, as on macOS.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::settings;

const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFile {
    pub name: String,
    pub path: String,
    pub size: u64,
}

pub fn inbox_dir() -> PathBuf {
    settings::local_dir().join("inbox")
}

// ── Paths a real drop delivered ─────────────────────────────────────────────
//
// `ingest_file` is callable from the page, so on its own it would copy any file
// the user can read (a script injected into the page could pull in ~/.ssh).
// Only paths the OS just delivered through a real drop are accepted: WebView2's
// drop objects on Windows (webview_drop.rs), Tauri's drag-drop window event
// elsewhere (lib.rs). Each is good once, for a couple of minutes.

const DROP_VALID_FOR: Duration = Duration::from_secs(120);
const DROP_MAX_PENDING: usize = 64;

static DROPPED: std::sync::Mutex<Vec<(String, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());

/// Records paths that came from a real drop.
pub fn allow_dropped<I: IntoIterator<Item = String>>(paths: I) {
    let mut list = DROPPED.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    list.retain(|(_, at)| now.duration_since(*at) < DROP_VALID_FOR);
    for p in paths {
        list.push((p, now));
    }
    let excess = list.len().saturating_sub(DROP_MAX_PENDING);
    list.drain(..excess);
}

/// True (once) when `path` was delivered by a drop in the last couple of minutes.
fn take_dropped(path: &str) -> bool {
    let mut list = DROPPED.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    list.retain(|(_, at)| now.duration_since(*at) < DROP_VALID_FOR);
    match list.iter().position(|(p, _)| p == path) {
        Some(i) => {
            list.remove(i);
            true
        }
        None => false,
    }
}

pub fn ingest(source: &str) -> Result<DroppedFile, String> {
    if !take_dropped(source) {
        return Err(crate::i18n::t("Only files dropped on the island can be added."));
    }
    let src = Path::new(source);
    let meta = std::fs::metadata(src)
        .map_err(|e| crate::i18n::tf("Cannot read {path}: {error}", &[("path", source), ("error", &e.to_string())]))?;
    if meta.is_dir() {
        return Err(crate::i18n::t("Folders can't be dropped yet."));
    }

    let dir = inbox_dir();
    crate::platform::ensure_private_dir(&settings::local_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let mut dest = dir.join(&name);
    if dest.exists() {
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = src.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        for i in 2..1000 {
            let candidate = dir.join(format!("{stem} ({i}){ext}"));
            if !candidate.exists() {
                dest = candidate;
                break;
            }
        }
    }

    std::fs::copy(src, &dest).map_err(|e| crate::i18n::tf("Cannot copy: {error}", &[("error", &e.to_string())]))?;
    // CopyFileEx carries the source's timestamps across, so a file last edited
    // three years ago would arrive already older than the sweep window and be
    // deleted on the spot. The inbox ages from when *we* copied it.
    if let Ok(file) = std::fs::File::options().write(true).open(&dest) {
        let _ = file.set_modified(SystemTime::now());
    }
    sweep(&dir);

    Ok(DroppedFile {
        name,
        path: dest.to_string_lossy().to_string(),
        size: meta.len(),
    })
}

// ── Grok Bot attachments ─────────────────────────────────────────────────────
// `ingest_files` copies each file under a unique id (`u<hex>-<original name>`),
// so two files with the same name never collide and the id alone is enough to
// find the copy again and show its original name.

/// A file copied into the inbox for a Grok Bot attachment. `id` is the inbox
/// file name; `name` is the original name, for display.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct IngestedFile {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

/// Mime type from the file extension. A small table on purpose: anything else
/// is `application/octet-stream`.
pub fn guess_mime(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "rs" => "text/x-rust",
        "ts" => "text/typescript",
        "js" => "text/javascript",
        "py" => "text/x-python",
        "html" => "text/html",
        "css" => "text/css",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "sql" => "application/sql",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

const ID_PREFIX: char = 'u';
const ID_HEX: usize = 16;

/// Original name carried inside an id (`u<16 hex>-<name>`); any other inbox
/// file (an older drop) is shown under its own file name.
pub fn display_name(id: &str) -> String {
    let mut chars = id.char_indices();
    if let Some((_, ID_PREFIX)) = chars.next() {
        let hex = &id[1..];
        if hex.len() > ID_HEX + 1
            && hex.is_char_boundary(ID_HEX)
            && hex[..ID_HEX].chars().all(|c| c.is_ascii_hexdigit())
            && hex[ID_HEX..].starts_with('-')
        {
            return hex[ID_HEX + 1..].to_string();
        }
    }
    id.to_string()
}

/// Ids are bare file names: no separators, no drive colon, no `..`, no NUL.
fn plausible_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id != "."
        && !id.contains("..")
        && !id.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
}

/// A file named `id` directly inside `dir`, or None. Both sides are
/// canonicalized, so a link or junction that leads out of `dir` is refused.
pub fn resolve_in(dir: &Path, id: &str) -> Option<PathBuf> {
    if !plausible_id(id) {
        return None;
    }
    let root = dir.canonicalize().ok()?;
    let path = root.join(id).canonicalize().ok()?;
    if path.parent() != Some(root.as_path()) || !path.is_file() {
        return None;
    }
    Some(path)
}

/// The inbox copy behind an attachment id.
pub fn inbox_file(id: &str) -> Option<PathBuf> {
    resolve_in(&inbox_dir(), id)
}

fn new_id(name: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let unique = nanos ^ (COUNTER.fetch_add(1, Ordering::Relaxed).rotate_left(48)) ^ ((std::process::id() as u64) << 32);
    format!("{ID_PREFIX}{unique:016x}-{name}")
}

/// A name that is safe as the tail of an inbox file name.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').replace("..", "_");
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

fn prepare_inbox() -> Result<PathBuf, String> {
    let dir = inbox_dir();
    crate::platform::ensure_private_dir(&settings::local_dir()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// A fresh, unused inbox path for `name`.
fn fresh_path(dir: &Path, name: &str) -> (String, PathBuf) {
    loop {
        let id = new_id(&clean_name(name));
        let path = dir.join(&id);
        if !path.exists() {
            return (id, path);
        }
    }
}

fn copy_one(dir: &Path, source: &str) -> Result<IngestedFile, String> {
    let src = Path::new(source);
    let meta = std::fs::metadata(src).map_err(|e| format!("cannot read {source}: {e}"))?;
    if meta.is_dir() {
        return Err("Todavía no se pueden soltar carpetas.".into());
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let (id, dest) = fresh_path(dir, &name);
    std::fs::copy(src, &dest).map_err(|e| format!("cannot copy: {e}"))?;
    // Same as `ingest`: the copy ages from now, not from the source's mtime.
    if let Ok(file) = std::fs::File::options().write(true).open(&dest) {
        let _ = file.set_modified(SystemTime::now());
    }
    Ok(IngestedFile { name: display_name(&id), mime: guess_mime(&name).into(), size: meta.len(), id })
}

/// Copies every file into the inbox under a unique id. Fails on the first file
/// that cannot be copied (copies already made age out with the sweep).
pub fn ingest_files(paths: &[String]) -> Result<Vec<IngestedFile>, String> {
    let dir = prepare_inbox()?;
    let out = paths.iter().map(|p| copy_one(&dir, p)).collect::<Result<Vec<_>, _>>()?;
    sweep(&dir);
    Ok(out)
}

/// Writes pasted bytes into the inbox so the Bot can find them on this PC when
/// they are too big to travel inline. Returns the full path.
pub fn save_to_inbox(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = prepare_inbox()?;
    let (_, path) = fresh_path(&dir, name);
    std::fs::write(&path, bytes).map_err(|e| format!("cannot write: {e}"))?;
    Ok(path)
}

/// Drops anything copied here more than a week ago. `ingest` stamps every copy
/// with the time it landed, so this really is the age of the copy and not the
/// age of whatever the user happened to drag in.
fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(copied) = meta.modified() else { continue };
        if now.duration_since(copied).map(|age| age > KEEP_FOR).unwrap_or(false) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_copies_and_never_overwrites() {
        let tmp = std::env::temp_dir().join(format!("coucou-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let source = tmp.join("note.txt");
        std::fs::write(&source, b"hello").unwrap();

        let drop = |p: &Path| allow_dropped([p.to_string_lossy().to_string()]);
        drop(&source);
        let first = ingest(source.to_str().unwrap()).unwrap();
        assert_eq!(first.name, "note.txt");
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");

        // A second drop of the same name must not clobber the first copy.
        std::fs::write(&source, b"second").unwrap();
        drop(&source);
        let second = ingest(source.to_str().unwrap()).unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"hello");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"second");

        // Folders are refused rather than silently ignored.
        drop(&tmp);
        assert!(ingest(tmp.to_str().unwrap()).is_err());

        // An ancient source must not arrive already older than the sweep window.
        let old_source = tmp.join("ancient.txt");
        std::fs::write(&old_source, b"old").unwrap();
        let long_ago = SystemTime::now() - KEEP_FOR - Duration::from_secs(60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old_source)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        drop(&old_source);
        let aged = ingest(old_source.to_str().unwrap()).unwrap();
        assert!(
            Path::new(&aged.path).exists(),
            "a file copied just now was swept as if it were a week old"
        );
        let _ = std::fs::remove_file(&aged.path);

        let _ = std::fs::remove_file(&first.path);
        let _ = std::fs::remove_file(&second.path);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn mime_is_guessed_from_the_extension() {
        assert_eq!(guess_mime("notes.TXT"), "text/plain");
        assert_eq!(guess_mime("a.md"), "text/markdown");
        assert_eq!(guess_mime("main.rs"), "text/x-rust");
        assert_eq!(guess_mime("x.yml"), "application/yaml");
        assert_eq!(guess_mime("photo.JPEG"), "image/jpeg");
        assert_eq!(guess_mime("pic.webp"), "image/webp");
        assert_eq!(guess_mime("doc.pdf"), "application/pdf");
        assert_eq!(guess_mime("archive.zip"), "application/zip");
        assert_eq!(guess_mime("setup.exe"), "application/octet-stream");
        assert_eq!(guess_mime("Makefile"), "application/octet-stream");
    }

    #[test]
    fn ids_carry_the_display_name() {
        let id = new_id("informe final.pdf");
        assert!(plausible_id(&id));
        assert_eq!(display_name(&id), "informe final.pdf");
        assert_eq!(display_name("note.txt"), "note.txt");
        assert_eq!(display_name("u123-short.txt"), "u123-short.txt");
        assert_ne!(new_id("a"), new_id("a"));
        let cleaned = clean_name("..\\..\\evil:name?.txt");
        assert!(plausible_id(&cleaned) && cleaned.ends_with("evil_name_.txt"), "{cleaned}");
        assert_eq!(clean_name(""), "file");
    }

    #[test]
    fn ingest_files_gives_resolvable_ids() {
        let tmp = std::env::temp_dir().join(format!("coucou-multi-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let a = tmp.join("same.txt");
        std::fs::write(&a, b"one").unwrap();
        let paths = vec![a.to_string_lossy().to_string(), a.to_string_lossy().to_string()];
        let got = ingest_files(&paths).unwrap();
        assert_eq!(got.len(), 2);
        assert_ne!(got[0].id, got[1].id);
        for f in &got {
            assert_eq!(f.name, "same.txt");
            assert_eq!(f.mime, "text/plain");
            assert_eq!(f.size, 3);
            let path = inbox_file(&f.id).expect("id resolves");
            assert_eq!(std::fs::read(&path).unwrap(), b"one");
            let _ = std::fs::remove_file(path);
        }
        assert!(ingest_files(&[tmp.to_string_lossy().to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolver_stays_inside_the_dir() {
        let tmp = std::env::temp_dir().join(format!("coucou-resolve-{}", std::process::id()));
        let inbox = tmp.join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("ok.txt"), b"x").unwrap();
        std::fs::write(tmp.join("secret.txt"), b"s").unwrap();
        std::fs::create_dir_all(inbox.join("sub")).unwrap();

        assert!(resolve_in(&inbox, "ok.txt").is_some());
        for bad in ["", ".", "..", "../secret.txt", "..\\secret.txt", "sub/../ok.txt", "sub\\x", "C:secret.txt", "C:\\Windows\\win.ini", "/etc/passwd", "missing.txt", "sub", "a\0b"] {
            assert!(resolve_in(&inbox, bad).is_none(), "accepted {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    // One test: the list is process-wide and tests run in parallel.
    #[test]
    fn only_a_dropped_path_is_ingested_once_and_the_list_stays_bounded() {
        let p = "/tmp/coucou-test-not-dropped.txt".to_string();
        assert!(ingest(&p).is_err(), "never dropped");
        allow_dropped([p.clone()]);
        assert!(take_dropped(&p));
        assert!(!take_dropped(&p), "good once");

        allow_dropped((0..200).map(|i| format!("/tmp/bounded-{i}")));
        assert!(DROPPED.lock().unwrap().len() <= DROP_MAX_PENDING);
        assert!(take_dropped("/tmp/bounded-199"));
        assert!(!take_dropped("/tmp/bounded-0"));
    }
}
