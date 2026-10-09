//! Port of tests/test_verify.py: AST verification of mechanical edits.

mod common;

use std::collections::HashMap;

use common::*;
use refactor_diff_core::{FileStatus, Report, analyze};

/// `analyze(repo, "main", "feature")` over an in-memory repository; unchanged files are left
/// out, as `git diff` would.
fn report_for(before: &[(&str, &str)], after: &[(&str, &str)]) -> Report {
    let mut changes = changes_from(before, after);
    changes.retain(|c| c.status != FileStatus::Modified || c.old_text != c.new_text);
    let head = DirHeadFiles::from_files(after);
    analyze(source("main", "feature"), changes, 2, &head)
}

/// `(path, new_start) -> verified` for every unit.
fn verified(report: &Report) -> HashMap<(String, u32), bool> {
    report
        .units
        .values()
        .map(|u| ((u.path.clone(), u.new_start), u.verified))
        .collect()
}

fn all_verified(report: &Report) -> bool {
    verified(report).values().all(|&ok| ok)
}

fn any_verified(report: &Report) -> bool {
    verified(report).values().any(|&ok| ok)
}

const BEFORE: &str = "def get_user(uid):
    \"\"\"Load a user.\"\"\"
    return db[uid]


def show(uid):
    user = get_user(uid)
    print(user)


def hide(uid):
    user = get_user(uid)
    del user
";

#[test]
fn mechanical_rename_is_verified() {
    let report = report_for(
        &[("a.py", BEFORE)],
        &[("a.py", &BEFORE.replace("get_user", "fetch_user"))],
    );
    assert!(all_verified(&report));
    assert_eq!(report.stats().verified_units, 3);
}

#[test]
fn rename_plus_logic_change_is_not_verified() {
    let after = BEFORE
        .replace("get_user", "fetch_user")
        .replace("    print(user)", "    if user:\n        print(user)");
    let report = report_for(&[("a.py", BEFORE)], &[("a.py", &after)]);
    let v = verified(&report);
    assert!(v[&("a.py".to_string(), 1)]); // the def
    assert!(v[&("a.py".to_string(), 13)]); // hide()
    // show()
    assert!(
        !v.iter()
            .any(|((_, line), ok)| *ok && (6..=10).contains(line))
    );
}

#[test]
fn docstring_and_retype_are_verified() {
    let before = concat!(
        "from typing import List\n\n\n",
        "def f(xs: List[int]) -> List[int]:\n    \"\"\"Old.\"\"\"\n    return xs\n"
    );
    let after = before
        .replace("List[int]", "list[int]")
        .replace("Old.", "New words.");
    let report = report_for(&[("a.py", before)], &[("a.py", &after)]);
    assert!(all_verified(&report));
}

#[test]
fn conflicting_renames_in_one_function_are_not_verified() {
    let before = "def f(a):\n    x = a\n    y = a\n    return x, y\n\n\ndef g(a):\n    return a\n";
    let after = "def f(a):\n    x = b\n    y = c\n    return x, y\n\n\ndef g(b):\n    return b\n";
    let report = report_for(&[("a.py", before)], &[("a.py", after)]);
    assert!(
        !verified(&report)
            .iter()
            .any(|((_, line), ok)| *ok && *line < 6)
    );
}

#[test]
fn inserted_top_level_function_is_not_verified() {
    let before = "def f():\n    return 1\n";
    let report = report_for(
        &[("a.py", before)],
        &[("a.py", &format!("{before}\n\ndef g():\n    return 2\n"))],
    );
    assert!(!any_verified(&report));
}

#[test]
fn decorator_rename_is_verified() {
    let before = "@old_dec\ndef f():\n    return 1\n\n\n@old_dec\ndef g():\n    return 2\n";
    let report = report_for(
        &[("a.py", before)],
        &[("a.py", &before.replace("old_dec", "new_dec"))],
    );
    assert!(all_verified(&report));
}

#[test]
fn syntax_error_side_does_not_break() {
    let report = report_for(
        &[("a.py", "def f(:\n    pass\n")],
        &[("a.py", "def f():\n    pass\n")],
    );
    assert!(!any_verified(&report));
}
