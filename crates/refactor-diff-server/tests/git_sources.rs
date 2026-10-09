//! Port of tests/test_sources.py plus loading changes from a real repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use refactor_diff_core::FileStatus;
use refactor_diff_server::exec::Tools;
use refactor_diff_server::git::{Git, GitHub, PrInfo, ReviewCommentPayload, SourceError};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Copy a file and give it a fresh mtime, so git never considers the rewritten file "racily
/// clean" when the next commit follows within the same second as the previous one.
fn copy_fresh(from: &Path, to: &Path) {
    std::fs::copy(from, to).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(to).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    f.set_modified(later).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            copy_fresh(&entry.path(), &dest);
        }
    }
}

/// `tests/conftest.py::_fixture_repo`.
fn fixture_repo(project: &str, repo: &Path) -> PathBuf {
    let project = Path::new(FIXTURES).join(project);
    copy_dir(&project.join("before"), repo);
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "before"]);
    git(repo, &["checkout", "-qb", "feature"]);
    copy_dir(&project.join("after"), repo);
    git(repo, &["commit", "-qam", "after"]);
    repo.to_path_buf()
}

struct NoGitHub;

impl GitHub for NoGitHub {
    fn available(&self) -> bool {
        false
    }
    fn pr_list(&self) -> Result<Vec<serde_json::Value>, SourceError> {
        Err(SourceError::GhMissing)
    }
    fn pr_view(&self, _: i64) -> Result<PrInfo, SourceError> {
        Err(SourceError::GhMissing)
    }
    fn post_pr_comment(&self, _: i64, _: &str) -> Result<String, SourceError> {
        Err(SourceError::GhMissing)
    }
    fn post_review_comment(&self, _: i64, _: &ReviewCommentPayload) -> Result<String, SourceError> {
        Err(SourceError::GhMissing)
    }
}

fn clone_of(repo: &Path, dir: &Path) -> PathBuf {
    let path = dir.join("clone");
    let out = Command::new("git")
        .args(["clone", "-q", "-b", "main"])
        .arg(repo)
        .arg(&path)
        .output()
        .unwrap();
    assert!(out.status.success());
    path
}

#[test]
fn ref_falls_back_to_remote_tracking_branch() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo("rename_project", &dir.path().join("repo"));
    let clone = clone_of(&repo, dir.path());
    let g = Git::new(clone.clone(), Arc::new(Tools::default()));
    assert_eq!(
        g.resolve_ref("feature").unwrap(),
        git(&clone, &["rev-parse", "origin/feature"])
    );
    let src = g
        .resolve(Some("main"), Some("feature"), None, &NoGitHub)
        .unwrap();
    assert_eq!(src.label, "main...feature");
    assert_eq!(src.identity(), "refs:main:feature");
    let changes = g.load_changes(&src).unwrap();
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "README.txt",
            "api.py",
            "billing.py",
            "models.py",
            "reports.py",
            "users.py"
        ]
    );
    assert!(changes.iter().all(|c| c.status == FileStatus::Modified));
    let head = refactor_diff_server::git::HeadLookup {
        git: &g,
        head_sha: src.head_sha.as_deref(),
    };
    let report = refactor_diff_core::analyze(src.to_core(), changes, 2, &head);
    assert_eq!(report.stats().residual_units, 1);
    assert_eq!(report.warnings.len(), 1, "missed-rename via git grep");
}

#[test]
fn ref_missing_locally_is_fetched_from_remote() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo("rename_project", &dir.path().join("repo"));
    let clone = clone_of(&repo, dir.path());
    git(&repo, &["branch", "late", "feature"]);
    let g = Git::new(clone.clone(), Arc::new(Tools::default()));
    let sha = g.resolve_ref("late").unwrap();
    assert_eq!(sha, git(&repo, &["rev-parse", "late"]));
    assert_eq!(git(&clone, &["rev-parse", "origin/late"]), sha);
}

#[test]
fn unknown_ref_error_mentions_remote() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo("rename_project", &dir.path().join("repo"));
    let clone = clone_of(&repo, dir.path());
    let g = Git::new(clone, Arc::new(Tools::default()));
    let err = g.resolve_ref("nope").unwrap_err().to_string();
    assert!(err.contains("origin/nope"), "{err}");
    assert!(
        err.starts_with("Unknown ref 'nope': not a local ref, and not found as origin/nope"),
        "{err}"
    );
}

#[test]
fn worktree_source_reads_the_filesystem_and_guards_paths() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo("rename_project", &dir.path().join("repo"));
    std::fs::write(
        repo.join("users.py"),
        "def fetch_user(user_id: str) -> dict:\n    return {}\n",
    )
    .unwrap();
    std::fs::write(repo.join("new.py"), "x = 1\n").unwrap();
    let g = Git::new(repo.clone(), Arc::new(Tools::default()));
    let src = g
        .resolve(Some("main"), Some(":worktree:"), None, &NoGitHub)
        .unwrap();
    assert_eq!(src.label, "main → working tree");
    assert!(src.head_sha.is_none());
    let changes = g.load_changes(&src).unwrap();
    let users = changes.iter().find(|c| c.path == "users.py").unwrap();
    assert!(users.new_text.contains("fetch_user(user_id: str)"));
    // Untracked files are not part of `git diff` against a tree.
    assert!(changes.iter().all(|c| c.path != "new.py"));
    let files = g
        .read_files(
            None,
            &["users.py", "../outside", "/etc/hosts", "missing.py"],
        )
        .unwrap();
    assert_eq!(files.keys().collect::<Vec<_>>(), vec!["users.py"]);
    let hits = g.grep_files(None, "fetch_user", &["*.py"]);
    assert!(hits.contains(&"users.py".to_string()));
    let root = Git::repo_root(&repo, &Tools::default()).unwrap();
    assert_eq!(root.canonicalize().unwrap(), repo.canonicalize().unwrap());
}

#[test]
fn list_sources_and_commits() {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo("rename_project", &dir.path().join("repo"));
    let g = Git::new(repo.clone(), Arc::new(Tools::default()));
    let info = g.list_sources(&NoGitHub).unwrap();
    assert!(
        info.branches.contains(&"main".to_string())
            && info.branches.contains(&"feature".to_string())
    );
    assert_eq!(info.current, "feature");
    assert_eq!(info.default_base, "main");
    assert_eq!(info.pr_error.as_deref(), Some("gh CLI not found"));
    let src = g
        .resolve(Some("main"), Some("feature"), None, &NoGitHub)
        .unwrap();
    let commits = g
        .list_commits(&src.base_sha, src.head_sha.as_deref())
        .unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].subject, "after");
    assert_eq!(commits[0].author, "t");
    assert_eq!(commits[0].files, 6);
    assert!(g.list_commits(&src.base_sha, None).unwrap().is_empty());
    let single = g
        .resolve(
            Some(&format!("{}^", commits[0].sha)),
            Some(&commits[0].sha),
            None,
            &NoGitHub,
        )
        .unwrap();
    assert!(single.label.starts_with("commit "), "{}", single.label);
    assert!(single.label.ends_with(" after"));
    assert_eq!(
        g.resolve(None, None, None, &NoGitHub)
            .unwrap_err()
            .to_string(),
        "Choose a base ref to compare against."
    );
}

#[test]
fn renames_and_binaries_in_a_diff() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(
        repo.join("a.py"),
        "def f():\n    return 1\n\n\ndef g():\n    return 2\n",
    )
    .unwrap();
    std::fs::write(repo.join("img.bin"), [0u8, 1, 2, 3]).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
    git(&repo, &["checkout", "-qb", "feature"]);
    git(&repo, &["mv", "a.py", "b.py"]);
    std::fs::write(repo.join("img.bin"), [0u8, 1, 2, 3, 4]).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "two"]);
    let g = Git::new(repo, Arc::new(Tools::default()));
    let src = g
        .resolve(Some("main"), Some("feature"), None, &NoGitHub)
        .unwrap();
    let changes = g.load_changes(&src).unwrap();
    assert_eq!(changes.len(), 1, "binary file skipped: {changes:?}");
    assert_eq!(changes[0].status, FileStatus::Renamed);
    assert_eq!(changes[0].old_path.as_deref(), Some("a.py"));
    assert_eq!(changes[0].path, "b.py");
    assert_eq!(changes[0].old_text, changes[0].new_text);
}
