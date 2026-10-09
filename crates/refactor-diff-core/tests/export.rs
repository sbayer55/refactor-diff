//! Port of tests/test_export.py: the Markdown summary.
//!
//! Only `test_markdown_summary` and the summary assertion of `test_summary_route_and_commits`
//! exercise the core; the commit list and the `gh` pull-request comment routes are server
//! behaviour and are not ported here.

mod common;

use common::*;
use refactor_diff_core::{FileStatus, Report, ReviewMarks, analyze, markdown_summary};

/// `analyze(repo, "main", "feature")` over an in-memory repository; unchanged files are left
/// out, as `git diff` would.
fn report_for(before: &[(&str, &str)], after: &[(&str, &str)]) -> Report {
    let mut changes = changes_from(before, after);
    changes.retain(|c| c.status != FileStatus::Modified || c.old_text != c.new_text);
    let head = DirHeadFiles::from_files(after);
    analyze(source("main", "feature"), changes, 2, &head)
}

/// `rename_repo` analyzed `main...feature`: the fixture's untouched files are not changes.
fn rename_repo_report() -> Report {
    let (before, after) = (fixture_side("before"), fixture_side("after"));
    report_for(&as_refs(&before), &as_refs(&after))
}

fn fixture_side(side: &str) -> Vec<(String, String)> {
    tree(&fixture("rename_project").join(side))
        .into_iter()
        .map(|(p, d)| (p, String::from_utf8(d).expect("utf-8 fixture")))
        .collect()
}

fn as_refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect()
}

#[test]
fn markdown_summary_with_review_marks() {
    let report = rename_repo_report();
    let [hunk] = &report.residual_hunk_ids[..] else {
        panic!("expected exactly one residual hunk");
    };
    let rename = report
        .groups
        .iter()
        .find(|g| g.label == "get_user → fetch_user")
        .expect("the rename group");
    let marks = ReviewMarks::new(
        [rename.id.clone()],
        [report.hunks[hunk].fingerprint.clone()],
    );
    let md = markdown_summary(&report, Some(&marks));
    assert!(md.starts_with("# refactor-diff: main...feature\n"), "{md}");
    assert!(
        md.contains("of changed lines collapsed · **1** change to review"),
        "{md}"
    );
    assert!(
        md.contains("| ✓ | rename | `get_user → fetch_user` | 8 | 4 |"),
        "{md}"
    );
    assert!(md.contains("|  | retype | `int → str` | 4 | 3 |"), "{md}");
    assert!(
        md.contains("- [x] `api.py:6` — `if user is None or not user.get(\"active\"):`"),
        "{md}"
    );
    assert!(
        md.contains("- **missed-rename**: get_user was renamed to fetch_user"),
        "{md}"
    );
    assert!(md.contains("(`legacy.py:1`, `legacy.py:5`)"), "{md}");
}

#[test]
fn summary_lists_unreviewed_residuals_unchecked() {
    // The summary route of test_summary_route_and_commits: `main...feature` after two commits.
    let report = report_for(
        &[("a.py", "x = 1\n")],
        &[("a.py", "x = 2\n"), ("b.py", "y = 1\ny2 = 2\n")],
    );
    let md = markdown_summary(&report, None);
    assert!(md.contains("- [ ] `a.py:1` — `x = 2`"), "{md}");
}
