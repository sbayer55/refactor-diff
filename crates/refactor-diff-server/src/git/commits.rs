//! Commits in a range and the commits that touched one line.

use serde::Serialize;

use super::parse::{commit_records, records};
use super::{Git, SourceError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TouchingCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub body: String,
}

impl Git {
    /// The first-parent commits from base to head, oldest first, with their line counts.
    pub fn list_commits(
        &self,
        base_sha: &str,
        head_sha: Option<&str>,
    ) -> Result<Vec<Commit>, SourceError> {
        let Some(head_sha) = head_sha else {
            return Ok(vec![]);
        };
        let out = self.text(&[
            "log",
            "--reverse",
            "--first-parent",
            "--shortstat",
            "--format=%x1e%H%x1f%h%x1f%s%x1f%an%x1f%aI",
            &format!("{base_sha}..{head_sha}"),
        ])?;
        Ok(commit_records(&out))
    }
}

const TOUCH_FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%s%x1f%an%x1f%aI%x1f%b";

fn touching(out: &str) -> Vec<TouchingCommit> {
    records(out)
        .into_iter()
        .map(|(head, rest)| {
            let f = |i: usize| head.get(i).cloned().unwrap_or_default();
            let mut body = f(5);
            if !rest.is_empty() {
                body = format!("{body}\n{rest}");
            }
            let body = body.split("\ndiff --git").next().unwrap_or("").to_string();
            TouchingCommit {
                sha: f(0),
                short: f(1),
                subject: f(2),
                author: f(3),
                date: f(4),
                body,
            }
        })
        .collect()
}

/// Commits in `base..head` that touched line `line` of `path` on the new side (`git log -L`).
/// Errors yield an empty list.
pub fn line_history_new(
    git: &Git,
    base: &str,
    head: &str,
    path: &str,
    line: u32,
) -> Vec<TouchingCommit> {
    let out = git.run(&[
        "log",
        TOUCH_FORMAT,
        &format!("{base}..{head}"),
        &format!("-L{line},{line}:{path}"),
    ]);
    match out {
        Ok(bytes) => touching(&String::from_utf8_lossy(&bytes)),
        Err(_) => vec![],
    }
}

/// Commits in `base..head` whose diff added or removed `text` in `paths` (`git log -S`).
pub fn line_history_old(
    git: &Git,
    base: &str,
    head: &str,
    text: &str,
    paths: &[&str],
) -> Vec<TouchingCommit> {
    let mut args = vec!["log", TOUCH_FORMAT, "-S", text];
    let range = format!("{base}..{head}");
    args.insert(2, &range);
    args.push("--");
    args.extend(paths);
    match git.run(&args) {
        Ok(bytes) => touching(&String::from_utf8_lossy(&bytes)),
        Err(_) => vec![],
    }
}
