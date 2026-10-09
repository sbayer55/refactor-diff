//! Port of tests/test_end_to_end.py: the fixture projects, analyzed the way the Python tests
//! did (`git diff main...feature`, so untouched files are not part of the change set).

mod common;

use std::collections::BTreeMap;

use common::*;
use refactor_diff_core::{
    Category, FileChange, FileStatus, Group, Report, SignatureKind, Source, WarningKind, analyze,
};

/// A fixture side (`before/` or `after/`) as `(path, text)` pairs.
fn fixture_tree(name: &str, side: &str) -> Vec<(String, String)> {
    tree(&fixture(name).join(side))
        .into_iter()
        .map(|(p, data)| (p, String::from_utf8(data).expect("utf-8 fixture")))
        .collect()
}

fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

/// The change set git would report: files whose content did not change are left out.
fn git_changes(before: &[(&str, &str)], after: &[(&str, &str)]) -> Vec<FileChange> {
    let mut changes = changes_from(before, after);
    changes.retain(|c| c.status != FileStatus::Modified || c.old_text != c.new_text);
    changes
}

fn analyze_trees(
    source: Source,
    before: &[(String, String)],
    after: &[(String, String)],
) -> Report {
    let (before, after) = (as_refs(before), as_refs(after));
    let head = DirHeadFiles::from_files(&after);
    analyze(source, git_changes(&before, &after), 2, &head)
}

/// `analyze(repo, "main", "feature")` on a fixture project.
fn fixture_report(name: &str) -> Report {
    analyze_trees(
        source("main", "feature"),
        &fixture_tree(name, "before"),
        &fixture_tree(name, "after"),
    )
}

/// Overwrite one file in a tree, like editing it in the working tree.
fn edit(files: &mut [(String, String)], path: &str, f: impl FnOnce(&str) -> String) {
    let entry = files
        .iter_mut()
        .find(|(p, _)| p == path)
        .unwrap_or_else(|| panic!("{path} in tree"));
    entry.1 = f(&entry.1);
}

fn mechanical<'r>(report: &'r Report, kind: SignatureKind, old: &str, new: &str) -> &'r Group {
    report
        .groups
        .iter()
        .find(|g| g.mechanical && g.kind == kind && g.old == old && g.new == new)
        .unwrap_or_else(|| panic!("mechanical group {kind:?} {old} → {new}"))
}

fn has_mechanical(report: &Report, kind: SignatureKind, old: &str, new: &str) -> bool {
    report
        .groups
        .iter()
        .any(|g| g.mechanical && g.kind == kind && g.old == old && g.new == new)
}

fn details(group: &Group) -> BTreeMap<&str, usize> {
    group
        .details
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect()
}

fn residual_positions(report: &Report) -> Vec<(String, u32)> {
    report
        .units
        .values()
        .filter(|u| !u.explained)
        .map(|u| (u.path.clone(), u.new_start))
        .collect()
}

fn missed_renames(report: &Report) -> Vec<&refactor_diff_core::Warning> {
    report
        .warnings
        .iter()
        .filter(|w| w.kind == WarningKind::MissedRename)
        .collect()
}

fn locations(w: &refactor_diff_core::Warning) -> Vec<(String, u32)> {
    w.locations
        .iter()
        .map(|l| (l.path.clone(), l.line))
        .collect()
}

#[test]
fn fixture_project() {
    let report = fixture_report("rename_project");

    let rename = mechanical(&report, SignatureKind::Rename, "get_user", "fetch_user");
    assert_eq!(rename.unit_ids.len(), 8);
    assert_eq!(
        details(rename),
        BTreeMap::from([("call", 4), ("import", 3), ("definition", 1)])
    );
    assert_eq!(
        details(mechanical(&report, SignatureKind::Retype, "int", "str")),
        BTreeMap::from([
            ("param user_id", 2),
            ("variable owner_id", 1),
            ("param owner_id", 1)
        ])
    );
    assert_eq!(
        mechanical(
            &report,
            SignatureKind::Replace,
            "cfg.get(\"timeout\")",
            "settings.timeout"
        )
        .unit_ids
        .len(),
        2
    );
    assert!(has_mechanical(&report, SignatureKind::Formatting, "", ""));

    // The only thing a reviewer has to read: the new condition in api.py.
    assert_eq!(residual_positions(&report), vec![("api.py".to_string(), 6)]);
    assert_eq!(
        report
            .residual_hunk_ids
            .iter()
            .map(|h| report.hunks[h].path.as_str())
            .collect::<Vec<_>>(),
        vec!["api.py"]
    );

    // legacy.py was never touched but still calls the old name.
    let [missed] = missed_renames(&report)[..] else {
        panic!("expected exactly one missed-rename warning");
    };
    assert_eq!(missed.total, 2);
    assert_eq!(
        locations(missed),
        vec![("legacy.py".to_string(), 1), ("legacy.py".to_string(), 5)]
    );

    let readme = report.file("README.txt").unwrap();
    assert!(!readme.analyzed);
    assert_eq!(readme.category, Category::Docs);
    assert_eq!(report.file("users.py").unwrap().category, Category::Source);
    assert_eq!(report.file("api.py").unwrap().residual_units, 1);
}

#[test]
fn worktree_source_report() {
    // On `feature`, legacy.py is edited in the working tree: `feature` → working tree.
    let base = fixture_tree("rename_project", "after");
    let mut head = base.clone();
    edit(&mut head, "legacy.py", |t| {
        t.replace("get_user", "fetch_user")
    });
    let report = analyze_trees(worktree_source("feature"), &base, &head);
    assert!(report.source.head_sha.is_none());
    assert_eq!(
        report
            .groups
            .iter()
            .filter(|g| g.mechanical)
            .map(|g| g.label.as_str())
            .collect::<Vec<_>>(),
        vec!["get_user → fetch_user"]
    );
    assert_eq!(report.stats().residual_units, 0);
}

#[test]
fn no_missed_rename_warning_when_old_name_still_defined() {
    // On `feature`, users.py gains a new `get_user` in the working tree: `main` → working tree.
    let base = fixture_tree("rename_project", "before");
    let mut head = fixture_tree("rename_project", "after");
    edit(&mut head, "users.py", |t| {
        format!("{t}\n\ndef get_user(user_id):\n    return None\n")
    });
    let report = analyze_trees(worktree_source("main"), &base, &head);
    assert!(missed_renames(&report).is_empty());
}

#[test]
fn typescript_fixture_project() {
    let report = fixture_report("ts_rename_project");

    let rename = mechanical(&report, SignatureKind::Rename, "getUser", "fetchUser");
    assert_eq!(rename.unit_ids.len(), 8);
    assert_eq!(
        details(rename),
        BTreeMap::from([("call", 4), ("import", 3), ("definition", 1)])
    );
    assert_eq!(
        details(mechanical(
            &report,
            SignatureKind::Retype,
            "number",
            "string"
        )),
        BTreeMap::from([
            ("param userId", 4),
            ("property ownerId", 1),
            ("param ownerId", 1)
        ])
    );
    // quote style and semicolons in reports.ts
    assert!(has_mechanical(&report, SignatureKind::Formatting, "", ""));

    assert_eq!(residual_positions(&report), vec![("api.ts".to_string(), 5)]);

    // legacy.js was never touched but still imports and calls the old name.
    let [missed] = missed_renames(&report)[..] else {
        panic!("expected exactly one missed-rename warning");
    };
    assert_eq!(
        locations(missed),
        vec![("legacy.js".to_string(), 1), ("legacy.js".to_string(), 4)]
    );
    assert!(report.files.iter().all(|f| f.analyzed));
}
