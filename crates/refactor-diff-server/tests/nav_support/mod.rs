//! Shared helpers for the navigation integration tests: fixture repositories built the way
//! `tests/conftest.py::_fixture_repo` builds them, and tool discovery for skipping.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use refactor_diff_server::exec::Tools;
use refactor_diff_server::git::Git;
use refactor_diff_server::nav::{Kind, Location};
use refactor_diff_server::snapshots::Snapshots;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .canonicalize()
        .expect("tests/fixtures exists")
}

pub fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            copy_fresh(&entry.path(), &dest);
        }
    }
}

/// Copy a file and give it a fresh mtime, so git never considers the rewritten file "racily
/// clean" when the next commit follows within the same second as the previous one.
fn copy_fresh(from: &Path, to: &Path) {
    std::fs::copy(from, to).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(to).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    f.set_modified(later).unwrap();
}

/// A git repo at `repo` with `main` (before/) and `feature` (after/) branches.
pub fn fixture_repo(project: &str, repo: &Path) -> PathBuf {
    let project = fixtures().join(project);
    copy_tree(&project.join("before"), repo);
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "before"]);
    git(repo, &["checkout", "-qb", "feature"]);
    for entry in std::fs::read_dir(project.join("after")).unwrap() {
        let entry = entry.unwrap();
        copy_fresh(&entry.path(), &repo.join(entry.file_name()));
    }
    git(repo, &["commit", "-qam", "after"]);
    repo.to_path_buf()
}

pub struct Fixture {
    pub repo: PathBuf,
    pub snapshots: Arc<Snapshots>,
    pub tools: Arc<Tools>,
    pub base: String,
    pub head: String,
    _dir: tempfile::TempDir,
}

pub fn fixture(project: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let repo = fixture_repo(project, &dir.path().join("repo"));
    let tools = Arc::new(Tools::default());
    let git = Git::new(repo.clone(), Arc::clone(&tools));
    let base = git.rev("main").unwrap();
    let head = git.rev("feature").unwrap();
    let snapshots = Arc::new(Snapshots::new(git, Some(dir.path().join("snaps"))).unwrap());
    Fixture {
        repo,
        snapshots,
        tools,
        base,
        head,
        _dir: dir,
    }
}

pub fn where_(locations: &[Location]) -> Vec<(Kind, String, Option<u32>)> {
    locations
        .iter()
        .map(|l| (l.kind, l.path.clone(), l.line))
        .collect()
}

/// `python3` from PATH when it is 3.10 or newer (what Jedi needs).
pub fn python3() -> Option<PathBuf> {
    let python = which::which("python3").ok()?;
    let out = Command::new(&python)
        .args(["-c", "import sys; print(sys.version_info >= (3, 10))"])
        .output()
        .ok()?;
    (String::from_utf8_lossy(&out.stdout).trim() == "True").then_some(python)
}

/// Node from PATH or the newest nvm install (nvm only exports it in interactive shells).
pub fn node() -> Option<PathBuf> {
    if let Ok(found) = which::which("node") {
        return Some(found);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let nvm = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".nvm"));
    let mut installs: Vec<PathBuf> = std::fs::read_dir(nvm.join("versions/node"))
        .ok()?
        .flatten()
        .map(|e| e.path().join("bin/node"))
        .filter(|p| p.is_file())
        .collect();
    installs.sort();
    installs.pop()
}

/// A real tsserver: `$TSSERVER`, `tsserver` on PATH, or a global TypeScript install.
pub fn tsserver() -> Option<PathBuf> {
    if let Some(given) = std::env::var_os("TSSERVER").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(given));
    }
    if let Ok(found) = which::which("tsserver") {
        return Some(found);
    }
    let mut roots: Vec<PathBuf> = vec![
        PathBuf::from("/usr/local/lib/node_modules"),
        PathBuf::from("/opt/homebrew/lib/node_modules"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let nvm = std::env::var_os("NVM_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".nvm"));
        if let Ok(entries) = std::fs::read_dir(nvm.join("versions/node")) {
            roots.extend(entries.flatten().map(|e| e.path().join("lib/node_modules")));
        }
    }
    roots
        .into_iter()
        .map(|r| r.join("typescript/lib/tsserver.js"))
        .find(|p| p.is_file())
}
