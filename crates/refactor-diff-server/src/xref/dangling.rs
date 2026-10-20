//! `xref-dangling`: concept identifiers and routes still present at head.

use std::collections::HashMap;

use refactor_diff_core::{LineType, Report, Severity};

use super::idents::{Stem, concept_names};
use super::{Check, DANGLING, head_sha};
use crate::git::Git;

/// Lines longer than this are generated or minified; they are skipped.
const MAX_LINE: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    Source,
    Docs,
    Translations,
    Other,
}

fn file_kind(path: &str) -> Option<FileKind> {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    if matches!(
        name,
        "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "cargo.lock" | "poetry.lock"
    ) || name.contains(".min.")
        || matches!(ext, "map" | "snap" | "svg" | "lock")
    {
        return None;
    }
    let in_dir = |d: &str| lower.split('/').any(|seg| seg == d);
    if matches!(ext, "md" | "mdx" | "rst" | "txt" | "adoc") || in_dir("docs") {
        return Some(FileKind::Docs);
    }
    if ext == "json" && super::i18n::is_i18n_path(path) {
        return Some(FileKind::Translations);
    }
    let source = matches!(
        ext,
        "js" | "jsx"
            | "ts"
            | "tsx"
            | "mjs"
            | "cjs"
            | "mts"
            | "cts"
            | "vue"
            | "svelte"
            | "html"
            | "css"
            | "scss"
            | "less"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "rb"
            | "php"
            | "cs"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "graphql"
            | "gql"
            | "sql"
    );
    Some(if source {
        FileKind::Source
    } else {
        FileKind::Other
    })
}

/// Concept names whose occurrences on removed lines outnumber those on added lines: the
/// diff deleted them (a moved or edited line that keeps the name doesn't count).
fn deleted_names(report: &Report, stem: &Stem) -> HashMap<String, i64> {
    let mut net: HashMap<String, i64> = HashMap::new();
    for line in report.hunks.values().flat_map(|h| &h.lines) {
        let delta = match line.kind {
            LineType::Removed => 1,
            LineType::Added => -1,
            LineType::Context => continue,
        };
        for name in concept_names(stem, &line.text) {
            *net.entry(name).or_insert(0) += delta;
        }
    }
    net.retain(|_, n| *n > 0);
    net
}

fn quoted(names: &[&String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn check(git: &Git, report: &Report, stem: &Stem) -> Vec<Check> {
    let deleted = deleted_names(report, stem);
    let hits = git.grep_lines(head_sha(report), &stem.grep_pattern(), true, &[]);
    let mut out = vec![];
    for (path, line, text) in hits {
        if text.len() > MAX_LINE {
            continue;
        }
        let Some(kind) = file_kind(&path) else {
            continue;
        };
        let names = concept_names(stem, &text);
        if names.is_empty() {
            continue;
        }
        let (gone, kept): (Vec<&String>, Vec<&String>) =
            names.iter().partition(|n| deleted.contains_key(*n));
        // A bare stem word (`program`) is a leftover only as a route segment (`/programs`).
        let specific = gone.iter().any(|n| n.starts_with('/') || !stem.is_bare(n));
        let (severity, message) = match (kind, gone.is_empty()) {
            (FileKind::Source, false) if !specific => (
                Severity::Info,
                format!(
                    "{} still mentioned after removal (a plain word; may be unrelated)",
                    quoted(&gone)
                ),
            ),
            (FileKind::Source, false) => (
                Severity::Warning,
                format!("{} still referenced after removal", quoted(&gone)),
            ),
            (FileKind::Docs, false) => (
                Severity::Info,
                format!("{} still mentioned in docs after removal", quoted(&gone)),
            ),
            (FileKind::Translations, false) => (
                Severity::Info,
                format!("{} still in translations after removal", quoted(&gone)),
            ),
            (FileKind::Other, false) => (
                Severity::Info,
                format!("{} still present after removal", quoted(&gone)),
            ),
            (_, true) => (
                Severity::Info,
                format!(
                    "{} mentions {} but was not removed in this diff",
                    quoted(&kept),
                    stem.first_word()
                ),
            ),
        };
        out.push(Check {
            code: DANGLING,
            severity,
            path,
            line,
            message,
        });
    }
    out
}
