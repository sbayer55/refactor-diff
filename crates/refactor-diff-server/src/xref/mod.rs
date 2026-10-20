//! Cross-reference checks for "remove concept X" changes: what the diff left behind.
//!
//! These are **code issues** (`xref-*` codes, `issues.rs` severities), served from
//! `/api/report/{id}/xref`. They never enter the report, its warnings or its explained counts.
//!
//! - `xref-dangling`: identifiers and routes containing the concept stem still present at head.
//!   A `warning` when the same identifier was deleted elsewhere in this diff (a leftover),
//!   `info` otherwise and in docs, translations and other non-source files.
//! - `xref-i18n-orphan`: translation keys deleted in the diff but still used at head
//!   (`warning`), and keys still defined whose last use was deleted (`info`).
//! - `xref-storage-shape`: a `localStorage`/`sessionStorage` payload that loses a field.
//! - `xref-payload-drop`: a request body that loses a field (`info`).

mod dangling;
mod i18n;
pub mod idents;
mod objects;

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, OnceLock};

use lru::LruCache;
use refactor_diff_core::{LineType, Report, Severity};
use serde::Serialize;

use crate::git::Git;

pub const DANGLING: &str = "xref-dangling";
pub const I18N_ORPHAN: &str = "xref-i18n-orphan";
pub const STORAGE_SHAPE: &str = "xref-storage-shape";
pub const PAYLOAD_DROP: &str = "xref-payload-drop";

/// At most this many checks are returned; the rest are counted in `total`/`counts`.
pub const MAX_CHECKS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Check {
    pub code: &'static str,
    pub severity: Severity,
    pub path: String,
    /// 1-based line at head (or in the file the message names).
    pub line: u32,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Xref {
    /// The concept stem the dangling check used; `None` skips that check.
    pub concept: Option<String>,
    /// Warnings first, then by code, path and line; capped at [`MAX_CHECKS`].
    pub checks: Vec<Check>,
    pub truncated: bool,
    /// Checks found before the cap.
    pub total: usize,
    /// Checks found per code before the cap.
    pub counts: BTreeMap<&'static str, usize>,
}

/// All checks for `report`, with `concept` as the stem for `xref-dangling`.
pub fn run(git: &Git, report: &Report, concept: Option<&str>) -> Xref {
    let mut checks = vec![];
    if let Some(stem) = concept.and_then(idents::Stem::new) {
        checks.extend(dangling::check(git, report, &stem));
    }
    checks.extend(i18n::check(git, report));
    checks.extend(objects::check(report));
    finish(concept.map(str::to_string), checks)
}

/// [`run`], cached per repository, report and concept for the lifetime of the process.
pub fn cached(git: &Git, report: &Report, concept: Option<&str>) -> Arc<Xref> {
    type Key = (std::path::PathBuf, String, Option<String>);
    static CACHE: OnceLock<Mutex<LruCache<Key, Arc<Xref>>>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new(LruCache::new(NonZeroUsize::new(64).expect("nonzero"))));
    let key = (
        git.repo().to_path_buf(),
        report.id.clone(),
        concept.map(str::to_string),
    );
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return hit.clone();
    }
    let result = Arc::new(run(git, report, concept));
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .put(key, result.clone());
    result
}

fn code_rank(check: &Check) -> u8 {
    match check.code {
        STORAGE_SHAPE => 0,
        I18N_ORPHAN => 1,
        PAYLOAD_DROP => 2,
        _ => 3,
    }
}

fn finish(concept: Option<String>, mut checks: Vec<Check>) -> Xref {
    checks.sort_by(|a, b| {
        (a.severity, code_rank(a), &a.path, a.line).cmp(&(
            b.severity,
            code_rank(b),
            &b.path,
            b.line,
        ))
    });
    checks.dedup();
    let mut counts = BTreeMap::new();
    for c in &checks {
        *counts.entry(c.code).or_insert(0) += 1;
    }
    let total = checks.len();
    let truncated = total > MAX_CHECKS;
    checks.truncate(MAX_CHECKS);
    Xref {
        concept,
        checks,
        truncated,
        total,
        counts,
    }
}

/// Changed line numbers of one file: removed lines on the old side, added on the new side.
#[derive(Debug, Default)]
pub(crate) struct Changed {
    pub old: std::collections::HashSet<u32>,
    pub new: std::collections::HashSet<u32>,
}

pub(crate) fn changed_lines(report: &Report) -> BTreeMap<&str, Changed> {
    let mut out: BTreeMap<&str, Changed> = BTreeMap::new();
    for hunk in report.hunks.values() {
        let entry = out.entry(hunk.path.as_str()).or_default();
        for line in &hunk.lines {
            match line.kind {
                LineType::Removed => entry.old.extend(line.old_no),
                LineType::Added => entry.new.extend(line.new_no),
                LineType::Context => {}
            }
        }
    }
    out
}

/// The commit for the head side (`None`: working tree).
pub(crate) fn head_sha(report: &Report) -> Option<&str> {
    report.source.head_sha.as_deref()
}

/// Every tracked path at head.
pub(crate) fn head_paths(git: &Git, report: &Report) -> Vec<String> {
    match head_sha(report) {
        Some(sha) => git.ls_tree(sha).unwrap_or_default(),
        None => git
            .text(&["ls-files"])
            .map(|t| t.lines().map(String::from).collect())
            .unwrap_or_default(),
    }
}

/// Whether `path` is JavaScript/TypeScript-like source the object checks understand.
pub(crate) fn is_js_like(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(
        ext,
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "svelte"
    )
}
