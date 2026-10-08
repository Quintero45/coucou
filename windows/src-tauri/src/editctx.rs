// Where an agent's edit sits in its file, for the island's editor view: the
// line the new text starts on and a few lines around it. The caller must
// already hold the text it looks for (the hook's new_string), so this never
// hands out more of a file than the handful of lines next to it.

use serde::Serialize;

/// Files past this are not read: the view shows the edit without line numbers.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const BEFORE: usize = 3;
const AFTER: usize = 4;

#[derive(Serialize, Debug, PartialEq)]
pub struct EditContext {
    /// 1-based line the text starts on.
    pub line: usize,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// The edit `needle` in `text`, or None when it isn't there (edited again since, empty).
pub fn locate(text: &str, needle: &str) -> Option<EditContext> {
    let text = text.replace("\r\n", "\n");
    let needle = needle.replace("\r\n", "\n");
    let needle = needle.trim_end_matches('\n');
    if needle.trim().is_empty() {
        return None;
    }
    let at = text.find(needle)?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    let first = text[..at].matches('\n').count();
    let last = first + needle.matches('\n').count();
    let cut = |s: &&str| s.chars().take(400).collect::<String>();
    Some(EditContext {
        line: first + 1,
        before: lines[first.saturating_sub(BEFORE)..first].iter().map(cut).collect(),
        after: lines.get(last + 1..(last + 1 + AFTER).min(lines.len())).unwrap_or(&[]).iter().map(cut).collect(),
    })
}

/// One lookup per hunk of the edit (at most 8), each None when not found.
#[tauri::command]
pub fn edit_context(path: String, needles: Vec<String>) -> Vec<Option<EditContext>> {
    let file = std::path::Path::new(&path);
    let readable = file.is_absolute()
        && std::fs::metadata(file).map(|m| m.is_file() && m.len() <= MAX_FILE_BYTES).unwrap_or(false);
    let text = if readable { std::fs::read_to_string(file).ok() } else { None };
    needles
        .iter()
        .take(8)
        .map(|n| text.as_deref().and_then(|t| locate(t, n)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "a\nb\nc\nd\nconst IVA = 0.19;\nconst X = 1;\ne\nf\ng\nh\ni\n";

    #[test]
    fn finds_the_line_and_its_neighbours() {
        let c = locate(FILE, "const IVA = 0.19;\nconst X = 1;\n").unwrap();
        assert_eq!(c.line, 5);
        assert_eq!(c.before, vec!["b", "c", "d"]);
        assert_eq!(c.after, vec!["e", "f", "g", "h"]);
    }

    #[test]
    fn crlf_files_and_the_edges() {
        let c = locate("x\r\ny\r\nz", "x").unwrap();
        assert_eq!((c.line, c.before.len(), c.after.clone()), (1, 0, vec!["y".to_string(), "z".to_string()]));
        let end = locate(FILE, "i").unwrap();
        assert_eq!(end.line, 11);
        assert!(end.after.is_empty());
    }

    #[test]
    fn nothing_for_missing_or_blank_text() {
        assert_eq!(locate(FILE, "nope"), None);
        assert_eq!(locate(FILE, "  \n"), None);
    }

    #[test]
    fn a_relative_or_missing_path_reads_nothing() {
        assert_eq!(edit_context("relative.txt".into(), vec!["a".into()]), vec![None]);
        assert_eq!(edit_context("C:\\no\\such\\file.txt".into(), vec!["a".into()]), vec![None]);
    }
}
