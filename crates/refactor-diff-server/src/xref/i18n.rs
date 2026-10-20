//! `xref-i18n-orphan`: translation keys and their `t("key")` / `i18nKey="key"` uses.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use refactor_diff_core::{Report, Severity};
use regex::Regex;
use serde_json::Value;

use super::{Check, I18N_ORPHAN, head_paths, head_sha};
use crate::git::Git;

/// Lines that may use a key, for git grep (POSIX ERE).
const REF_GREP: &str = "(^|[^A-Za-z0-9_])t\\(|i18nKey=";

static REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:(?:^|[^\w])t\(\s*|i18nKey=\{?\s*)(?:"([^"$\\]+)"|'([^'$\\]+)'|`([^`$\\]+)`)"#)
        .expect("valid regex")
});

const PLURAL_SUFFIXES: &[&str] = &[
    "_zero", "_one", "_two", "_few", "_many", "_other", "_plural",
];

/// Translation files: JSON under an `i18n`, `locales`, `locale` or `translations` directory.
pub fn is_i18n_path(path: &str) -> bool {
    path.ends_with(".json")
        && path
            .split('/')
            .any(|seg| matches!(seg, "i18n" | "locales" | "locale" | "translations"))
}

/// The dotted leaf keys of a translation file, plus each plural key without its suffix.
fn flatten(text: &str) -> BTreeSet<String> {
    fn walk(prefix: &str, value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    walk(&key, v, out);
                }
            }
            _ if !prefix.is_empty() => {
                out.insert(prefix.to_string());
                for suffix in PLURAL_SUFFIXES {
                    if let Some(base) = prefix.strip_suffix(suffix) {
                        out.insert(base.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        walk("", &value, &mut out);
    }
    out
}

/// Key uses on one line; a namespaced key (`ns:a.b`) is also returned without its namespace.
fn keys_on(line: &str) -> Vec<String> {
    REF_RE
        .captures_iter(line)
        .filter_map(|c| c.get(1).or(c.get(2)).or(c.get(3)))
        .map(|m| m.as_str().trim().to_string())
        .filter(|k| !k.is_empty() && !k.contains(' '))
        .collect()
}

fn known(keys: &BTreeSet<String>, key: &str) -> bool {
    keys.contains(key)
        || key
            .split_once(':')
            .is_some_and(|(_, rest)| keys.contains(rest))
}

fn strip_ns(key: &str) -> &str {
    key.split_once(':').map_or(key, |(_, rest)| rest)
}

/// The line defining a dotted key in pretty-printed JSON: each segment's `"name":` in turn,
/// at its nesting depth, falling back to the deepest segment found.
fn key_line(text: &str, key: &str) -> u32 {
    let mut depths = vec![];
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for line in text.lines() {
        depths.push(depth);
        for c in line.chars() {
            match c {
                _ if escaped => escaped = false,
                '\\' if in_str => escaped = true,
                '"' => in_str = !in_str,
                '{' | '[' if !in_str => depth += 1,
                '}' | ']' if !in_str => depth -= 1,
                _ => {}
            }
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut at = 0;
    let mut found = 1;
    for (k, seg) in key.split('.').enumerate() {
        let needle = format!("\"{seg}\"");
        let plural = format!("\"{seg}_");
        match (at..lines.len()).find(|&i| {
            let l = lines[i].trim_start();
            depths[i] == k as i32 + 1
                && (l.starts_with(&needle) || l.starts_with(&plural))
                && l.contains(':')
        }) {
            Some(i) => {
                at = i;
                found = i as u32 + 1;
            }
            None => break,
        }
    }
    found
}

fn refs(git: &Git, sha: Option<&str>) -> Vec<(String, u32, String)> {
    let mut out = vec![];
    for (path, line, text) in git.grep_lines(sha, REF_GREP, true, &[]) {
        if is_i18n_path(&path) || text.len() > 1000 {
            continue;
        }
        for key in keys_on(&text) {
            out.push((path.clone(), line, key));
        }
    }
    out
}

pub fn check(git: &Git, report: &Report) -> Vec<Check> {
    // Keys defined by the changed translation files in the base, by file.
    let base_files: BTreeMap<&str, BTreeSet<String>> = report
        .texts
        .iter()
        .filter(|(path, _)| is_i18n_path(path))
        .map(|(path, (old, _))| (path.as_str(), flatten(old)))
        .collect();
    if base_files.is_empty() {
        return vec![];
    }
    let head = head_sha(report);
    let head_i18n: Vec<String> = head_paths(git, report)
        .into_iter()
        .filter(|p| is_i18n_path(p))
        .collect();
    let head_refs: Vec<&str> = head_i18n.iter().map(String::as_str).collect();
    let head_texts = git.read_files(head, &head_refs).unwrap_or_default();
    let head_keys: BTreeSet<String> = head_texts.values().flat_map(|t| flatten(t)).collect();

    let mut out = vec![];
    // Deleted keys still used at head.
    let mut deleted_from: BTreeMap<&str, &str> = BTreeMap::new();
    for (path, keys) in &base_files {
        for key in keys {
            if !head_keys.contains(key) {
                deleted_from.entry(key.as_str()).or_insert(path);
            }
        }
    }
    let used_at_head = refs(git, head);
    for (path, line, key) in &used_at_head {
        if known(&head_keys, key) {
            continue;
        }
        let Some(file) = deleted_from
            .get(key.as_str())
            .or_else(|| deleted_from.get(strip_ns(key)))
        else {
            continue;
        };
        out.push(Check {
            code: I18N_ORPHAN,
            severity: Severity::Warning,
            path: path.clone(),
            line: *line,
            message: format!("translation key `{key}` was deleted from {file} but is still used"),
        });
    }

    // Keys of the changed translation files, still defined at head, whose last use was deleted.
    let in_scope: BTreeSet<String> = report
        .texts
        .iter()
        .filter(|(p, _)| is_i18n_path(p))
        .flat_map(|(_, (old, new))| flatten(old).into_iter().chain(flatten(new)))
        .collect();
    let mut head_flat: Vec<(&String, &String, BTreeSet<String>)> =
        head_texts.iter().map(|(p, t)| (p, t, flatten(t))).collect();
    // Changed files first, so a key is located where the diff touched it.
    head_flat.sort_by_key(|(p, _, _)| !report.texts.contains_key(p.as_str()));
    let at_head: BTreeSet<&str> = used_at_head.iter().map(|(_, _, k)| strip_ns(k)).collect();
    let used_at_base = refs(git, Some(report.source.base_sha.as_str()));
    let mut seen = BTreeSet::new();
    for (path, _, key) in &used_at_base {
        let key = strip_ns(key);
        if at_head.contains(key) || !in_scope.contains(key) || !seen.insert(key) {
            continue;
        }
        let Some((file, text, _)) = head_flat.iter().find(|(_, _, keys)| keys.contains(key)) else {
            continue;
        };
        out.push(Check {
            code: I18N_ORPHAN,
            severity: Severity::Info,
            path: file.to_string(),
            line: key_line(text, key),
            message: format!(
                "translation key `{key}` is no longer used: its last use (in {path}) was removed"
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_keys() {
        let keys = flatten(r#"{"a": {"b": "x", "c_one": "y", "c_other": "z"}, "d": "w"}"#);
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        assert_eq!(keys, ["a.b", "a.c", "a.c_one", "a.c_other", "d"]);
    }

    #[test]
    fn finds_key_uses() {
        assert_eq!(
            keys_on(r#"{t("a.b")} <Trans i18nKey="c.d" /> i18n.t('e', {x}) set("no")"#),
            ["a.b", "c.d", "e"]
        );
        assert!(keys_on("t(`a.${x}`)").is_empty());
    }

    #[test]
    fn locates_keys() {
        let text = "{\n  \"a\": {\n    \"x\": 1,\n    \"b\": \"y\"\n  },\n  \"b\": 2\n}\n";
        assert_eq!(key_line(text, "a.b"), 4);
        assert_eq!(key_line(text, "b"), 6);
    }
}
