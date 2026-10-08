// Where an agent's edit sits in its file, for the island's editor view: the
// line a hunk starts on and a few lines around it. The caller must already
// hold the text it looks for (the hook's new_string), so this never hands out
// more of a file than the handful of lines next to it.

use serde::{Deserialize, Serialize};

/// Files past this are not read: the view shows the edit without line numbers.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const BEFORE: usize = 3;
const AFTER: usize = 4;

#[derive(Serialize, Debug, PartialEq)]
pub struct EditContext {
    /// 1-based file line of the hunk's first line.
    pub line: usize,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

/// One hunk to find: the text its edit left in the file, and the hunk's first
/// and last line inside that text, 1-based.
#[derive(Deserialize, Debug)]
pub struct Lookup {
    pub text: String,
    pub from: usize,
    pub to: usize,
}

/// Where `needle` is in `hay`, when it is there exactly once.
fn only_place(hay: &str, needle: &str) -> Option<usize> {
    let at = hay.find(needle)?;
    let step = needle.chars().next().map_or(1, char::len_utf8);
    match hay[at + step..].find(needle) {
        Some(_) => None,
        None => Some(at),
    }
}

/// The hunk `lookup` in `file`, or None when its text isn't there exactly once
/// (gone, edited again since, or too common to tell where it went).
pub fn locate(file: &str, lookup: &Lookup) -> Option<EditContext> {
    let file = file.replace("\r\n", "\n");
    let needle = lookup.text.replace("\r\n", "\n");
    if needle.trim().is_empty() || lookup.from == 0 || lookup.to < lookup.from {
        return None;
    }
    let at = only_place(&file, &needle)?;
    let mut lines: Vec<&str> = file.split('\n').collect();
    if file.ends_with('\n') {
        lines.pop();
    }
    let start = file[..at].matches('\n').count();
    let first = start + lookup.from - 1;
    let last = start + lookup.to - 1;
    if last >= lines.len() {
        return None;
    }
    let cut = |s: &&str| s.chars().take(400).collect::<String>();
    Some(EditContext {
        line: first + 1,
        before: lines[first.saturating_sub(BEFORE)..first].iter().map(cut).collect(),
        after: lines[last + 1..(last + 1 + AFTER).min(lines.len())].iter().map(cut).collect(),
    })
}

/// One answer per hunk of the edit (at most 8), each None when not found for sure.
#[tauri::command]
pub fn edit_context(path: String, lookups: Vec<Lookup>) -> Vec<Option<EditContext>> {
    let file = std::path::Path::new(&path);
    let readable = file.is_absolute()
        && std::fs::metadata(file).map(|m| m.is_file() && m.len() <= MAX_FILE_BYTES).unwrap_or(false);
    let text = if readable { std::fs::read_to_string(file).ok() } else { None };
    lookups
        .iter()
        .take(8)
        .map(|l| text.as_deref().and_then(|t| locate(t, l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "a\nb\nc\nd\nconst IVA = 0.19;\nconst X = 1;\ne\nf\ng\nh\ni\n";

    fn find(text: &str, from: usize, to: usize) -> Lookup {
        Lookup { text: text.into(), from, to }
    }

    #[test]
    fn finds_the_line_and_its_neighbours() {
        let c = locate(FILE, &find("const IVA = 0.19;\nconst X = 1;\n", 1, 2)).unwrap();
        assert_eq!(c.line, 5);
        assert_eq!(c.before, vec!["b", "c", "d"]);
        assert_eq!(c.after, vec!["e", "f", "g", "h"]);
    }

    #[test]
    fn a_hunk_further_down_its_edit() {
        let c = locate(FILE, &find("d\nconst IVA = 0.19;\nconst X = 1;", 3, 3)).unwrap();
        assert_eq!(c.line, 6);
        assert_eq!(c.after, vec!["e", "f", "g", "h"]);
    }

    #[test]
    fn short_text_is_fine_when_the_file_holds_it_once() {
        assert_eq!(locate(FILE, &find("0.19", 1, 1)).unwrap().line, 5);
        assert_eq!(locate("x = 1;\nfoo();\nx = 1;\n", &find("x = 1;", 1, 1)), None);
        assert_eq!(locate("aa", &find("a", 1, 1)), None);
    }

    #[test]
    fn crlf_files_and_the_edges() {
        let c = locate("x\r\ny\r\nz", &find("x", 1, 1)).unwrap();
        assert_eq!((c.line, c.before.len(), c.after.clone()), (1, 0, vec!["y".to_string(), "z".to_string()]));
        let end = locate(FILE, &find("i", 1, 1)).unwrap();
        assert_eq!(end.line, 11);
        assert!(end.after.is_empty());
    }

    #[test]
    fn nothing_for_missing_or_blank_text() {
        assert_eq!(locate(FILE, &find("nope", 1, 1)), None);
        assert_eq!(locate(FILE, &find(" \n", 1, 1)), None);
        assert_eq!(locate(FILE, &find("a", 2, 1)), None);
    }

    #[test]
    fn a_relative_or_missing_path_reads_nothing() {
        assert_eq!(edit_context("relative.txt".into(), vec![find("a", 1, 1)]), vec![None]);
        assert_eq!(edit_context("C:\\no\\such\\file.txt".into(), vec![find("a", 1, 1)]), vec![None]);
    }
}
