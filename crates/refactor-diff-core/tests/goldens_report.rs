//! Parity with the Python implementation on the fixture projects (`tests/goldens/README.md`).

mod common;

use common::*;
use pretty_assertions::assert_eq;
use refactor_diff_core::{ReviewMarks, file_diff, markdown_summary};

#[test]
fn rename_project_report_matches_golden() {
    let report = analyze_fixture("rename_project");
    let got = serde_json::to_value(&report).unwrap();
    assert_eq!(
        canonical(&got),
        canonical(&golden_json("rename_project.report.json"))
    );
}

#[test]
fn ts_rename_project_report_matches_golden() {
    let report = analyze_fixture("ts_rename_project");
    let got = serde_json::to_value(&report).unwrap();
    assert_eq!(
        canonical(&got),
        canonical(&golden_json("ts_rename_project.report.json"))
    );
}

#[test]
fn rename_project_worktree_report_matches_golden() {
    let project = fixture("rename_project");
    let changes = changes_between(&project.join("before"), &project.join("after"));
    let head = DirHeadFiles::new(&project.join("after"));
    let report = refactor_diff_core::analyze(worktree_source("main"), changes, 2, &head);
    let got = serde_json::to_value(&report).unwrap();
    assert_eq!(
        canonical(&got),
        canonical(&golden_json("rename_project.worktree.report.json"))
    );
}

#[test]
fn rename_project_fileview_matches_golden() {
    let report = analyze_fixture("rename_project");
    let view = file_diff(&report, "api.py").unwrap();
    let got = serde_json::to_value(&view).unwrap();
    assert_eq!(
        canonical(&got),
        canonical(&golden_json("rename_project.fileview.json"))
    );
}

#[test]
fn rename_project_summary_matches_golden() {
    let report = analyze_fixture("rename_project");
    let first_group = report
        .groups
        .iter()
        .find(|g| g.mechanical)
        .map(|g| g.id.clone())
        .unwrap();
    let first_residual = report.hunks[&report.residual_hunk_ids[0]]
        .fingerprint
        .clone();
    let marks = ReviewMarks::new([first_group], [first_residual]);
    assert_eq!(
        markdown_summary(&report, Some(&marks)),
        golden_text("rename_project.summary.md")
    );
}

#[test]
fn analysis_is_deterministic() {
    let first = serde_json::to_value(analyze_fixture("rename_project")).unwrap();
    for _ in 0..5 {
        assert_eq!(
            serde_json::to_value(analyze_fixture("rename_project")).unwrap(),
            first
        );
    }
}
