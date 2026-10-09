//! The launcher's command line: help, argument errors and the repository check.

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use predicates::prelude::*;

fn refactor_diff() -> Command {
    cargo_bin_cmd!("refactor-diff")
}

#[test]
fn help_lists_the_flags() {
    refactor_diff()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("refactor-diff"))
        .stdout(predicate::str::contains("--exit-with-parent"))
        .stdout(predicate::str::contains("--hide <KINDS>"))
        .stdout(predicate::str::contains("--editor <NAME|TEMPLATE>"));
}

#[test]
fn a_directory_outside_a_repository_is_an_error() {
    let dir = std::env::temp_dir().join(format!("refactor-diff-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let result = refactor_diff()
        .args(["--repo", dir.to_str().unwrap(), "--no-browser"])
        .assert();
    std::fs::remove_dir_all(&dir).unwrap();
    result
        .code(1)
        .stderr(predicate::str::contains("refactor-diff: "))
        .stderr(predicate::str::contains("is not inside a git repository"));
}

#[test]
fn unknown_hide_values_are_an_error() {
    refactor_diff()
        .args(["--hide", "bogus", "--no-browser"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "refactor-diff: unknown --hide value(s): bogus",
        ));
}

#[test]
fn a_bad_editor_is_an_error() {
    refactor_diff()
        .args(["--editor", "notepad", "--no-browser"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("--editor must be one of vscode"));
}

#[test]
fn usage_errors_keep_clap_exit_code() {
    refactor_diff().args(["--port", "x"]).assert().code(2);
}
