//! Shared helpers: fixture projects as change sets, without git.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use refactor_diff_core::categories::fnmatch;
use refactor_diff_core::{FileChange, FileStatus, HeadFiles, Source};

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");
pub const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/goldens");
pub const BASE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub const HEAD_SHA: &str = "89abcdef0123456789abcdef0123456789abcdef";

pub fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

pub fn golden_json(name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(Path::new(GOLDENS).join(name)).expect("golden file");
    serde_json::from_str(&text).expect("golden json")
}

pub fn golden_text(name: &str) -> String {
    std::fs::read_to_string(Path::new(GOLDENS).join(name)).expect("golden file")
}

/// Every file under `root` as `relative/posix/path -> bytes`.
pub fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    walk(root, root, &mut out);
    out
}

/// The change set between two directories, the way the golden generator built it: files in
/// both are `M` even when identical, a deleted and an added file with identical content are
/// one `R`, files containing NUL are skipped.
pub fn changes_between(before: &Path, after: &Path) -> Vec<FileChange> {
    let (old, new) = (tree(before), tree(after));
    let mut entries: Vec<(FileStatus, Option<String>, String)> = Vec::new();
    for p in old.keys().filter(|p| new.contains_key(*p)) {
        entries.push((FileStatus::Modified, Some(p.clone()), p.clone()));
    }
    let deleted: Vec<&String> = old.keys().filter(|p| !new.contains_key(*p)).collect();
    let mut unmatched_added: Vec<&String> = new.keys().filter(|p| !old.contains_key(*p)).collect();
    for d in deleted {
        if let Some(pos) = unmatched_added.iter().position(|a| new[*a] == old[d]) {
            let a = unmatched_added.remove(pos);
            entries.push((FileStatus::Renamed, Some(d.clone()), a.clone()));
        } else {
            entries.push((FileStatus::Deleted, Some(d.clone()), d.clone()));
        }
    }
    for a in unmatched_added {
        entries.push((FileStatus::Added, None, a.clone()));
    }
    let mut changes: Vec<FileChange> = entries
        .into_iter()
        .filter_map(|(status, old_path, new_path)| {
            let old_bytes: &[u8] = match (&status, &old_path) {
                (FileStatus::Added, _) => &[],
                (_, Some(p)) => old.get(p).map_or(&[][..], Vec::as_slice),
                _ => &[],
            };
            let new_bytes: &[u8] = if status == FileStatus::Deleted {
                &[]
            } else {
                new.get(&new_path).map_or(&[][..], Vec::as_slice)
            };
            if old_bytes.contains(&0) || new_bytes.contains(&0) {
                return None;
            }
            Some(FileChange {
                path: new_path,
                old_path: if status == FileStatus::Renamed {
                    old_path
                } else {
                    None
                },
                status,
                old_text: String::from_utf8_lossy(old_bytes).into_owned(),
                new_text: String::from_utf8_lossy(new_bytes).into_owned(),
            })
        })
        .collect();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    changes
}

/// A change set built from in-memory files: `before` and `after` as `(path, text)` lists.
pub fn changes_from(before: &[(&str, &str)], after: &[(&str, &str)]) -> Vec<FileChange> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (sub, files) in [("before", before), ("after", after)] {
        for (path, text) in files {
            let full = dir.path().join(sub).join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, text).unwrap();
        }
    }
    changes_between(&dir.path().join("before"), &dir.path().join("after"))
}

/// Head-side lookups over a directory: `git grep -w -F` and `cat-file` without git.
pub struct DirHeadFiles {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl DirHeadFiles {
    pub fn new(after: &Path) -> Self {
        Self { files: tree(after) }
    }

    pub fn from_files(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, t)| (p.to_string(), t.as_bytes().to_vec()))
                .collect(),
        }
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `(?<!\w)word(?!\w)`.
pub fn contains_word(text: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(i) = text[from..].find(word) {
        let start = from + i;
        let end = start + word.len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + word.len().max(1);
    }
    false
}

impl HeadFiles for DirHeadFiles {
    fn grep_word(&self, word: &str, globs: &[&str]) -> Vec<String> {
        self.files
            .iter()
            .filter(|(path, _)| globs.iter().any(|g| fnmatch(path, g)))
            .filter(|(_, data)| contains_word(&String::from_utf8_lossy(data), word))
            .map(|(path, _)| path.clone())
            .collect()
    }

    fn read(&self, paths: &[&str]) -> HashMap<String, String> {
        paths
            .iter()
            .filter_map(|p| {
                self.files
                    .get(*p)
                    .map(|d| (p.to_string(), String::from_utf8_lossy(d).into_owned()))
            })
            .collect()
    }
}

pub fn source(base: &str, head: &str) -> Source {
    Source {
        label: format!("{base}...{head}"),
        base: base.to_string(),
        head: head.to_string(),
        base_sha: BASE_SHA.to_string(),
        head_sha: Some(HEAD_SHA.to_string()),
        pr: None,
        identity: format!("refs:{base}:{head}"),
        min_count: 0,
    }
}

pub fn worktree_source(base: &str) -> Source {
    Source {
        label: format!("{base} → working tree"),
        base: base.to_string(),
        head: ":worktree:".to_string(),
        base_sha: BASE_SHA.to_string(),
        head_sha: None,
        pr: None,
        identity: format!("worktree:{base}"),
        min_count: 0,
    }
}

/// Analyze a fixture project (`before/` vs `after/`).
pub fn analyze_fixture(name: &str) -> refactor_diff_core::Report {
    let project = fixture(name);
    let changes = changes_between(&project.join("before"), &project.join("after"));
    let head = DirHeadFiles::new(&project.join("after"));
    refactor_diff_core::analyze(source("main", "feature"), changes, 2, &head)
}

/// Analyze an in-memory change set with the default min_count.
pub fn analyze_files(
    before: &[(&str, &str)],
    after: &[(&str, &str)],
) -> refactor_diff_core::Report {
    let changes = changes_from(before, after);
    let head = DirHeadFiles::from_files(after);
    refactor_diff_core::analyze(source("main", "feature"), changes, 2, &head)
}

/// Pretty JSON with sorted keys, for readable diffs.
pub fn canonical(v: &serde_json::Value) -> String {
    fn sort(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                let sorted: BTreeMap<_, _> = m.iter().map(|(k, v)| (k.clone(), sort(v))).collect();
                serde_json::Value::Object(sorted.into_iter().collect())
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string_pretty(&sort(v)).unwrap()
}
