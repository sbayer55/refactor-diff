//! One file's history: the commits that touched it (following renames), its content and
//! blame at any of them, and the two checkout actions the history view offers.

use std::collections::HashSet;

use serde::Serialize;

use super::parse::{blame_porcelain, numstat_z, records};
use super::sources::worktree_file;
use super::{Git, SourceError};

/// The literal the history API uses for the uncommitted working-tree version.
pub const WORKTREE_SHA: &str = ":worktree:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HistoryCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub insertions: u32,
    pub deletions: u32,
    /// The name the file had after this commit (it changes across renames).
    pub path: String,
}

#[derive(Clone, Debug, Default)]
pub struct HistoryPage {
    pub commits: Vec<HistoryCommit>,
    pub more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BlameCommit {
    pub sha: String,
    pub short: String,
    pub subject: String,
    pub author: String,
    pub date: String,
    /// The file's name in that commit (lines older than a rename come from the old name).
    pub path: String,
}

/// Who last wrote each line of one version of a file.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// Index into `commits`, one per line.
    pub lines: Vec<u32>,
    /// The 1-based line number each line had in the commit that wrote it.
    pub origin: Vec<u32>,
}

const LOG_FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%s%x1f%an%x1f%aI";

impl Git {
    /// The commits that touched `path` (as named at `rev`), newest first, following renames,
    /// at most `max` of them; `more` says the history was cut off there. (One call for the
    /// whole history: git's `--skip` loses the rename trail when combined with `--follow`.)
    pub fn file_history(
        &self,
        rev: &str,
        path: &str,
        max: usize,
    ) -> Result<HistoryPage, SourceError> {
        let count = format!("-n{}", max + 1);
        let out = self.run(&[
            "log",
            "--follow",
            "-M",
            "--numstat",
            "-z",
            LOG_FORMAT,
            &count,
            rev,
            "--",
            path,
        ])?;
        let mut commits = Vec::new();
        // A commit without a numstat line (a merge, an empty commit) keeps the name the file
        // had in the next-newer commit, which is `path` itself for the first one.
        let mut name = path.to_string();
        for (head, stat) in numstat_z(&out) {
            let f = |i: usize| head.get(i).cloned().unwrap_or_default();
            let (insertions, deletions) = match &stat {
                Some((a, d, p)) => {
                    name = p.clone();
                    (*a, *d)
                }
                None => (0, 0),
            };
            commits.push(HistoryCommit {
                sha: f(0),
                short: f(1),
                subject: f(2),
                author: f(3),
                date: f(4),
                insertions,
                deletions,
                path: name.clone(),
            });
        }
        let more = commits.len() > max;
        commits.truncate(max);
        Ok(HistoryPage { commits, more })
    }

    /// `Some((insertions, deletions))` when the working tree's `path` differs from HEAD
    /// (an untracked file counts as all insertions); `None` when they are identical.
    pub fn worktree_delta(&self, path: &str) -> Result<Option<(u32, u32)>, SourceError> {
        let out = self.run(&["diff", "--numstat", "-z", "HEAD", "--", path])?;
        if !out.is_empty() {
            // `add\tdel\tpath\0` (binary counts are `-`).
            let text = String::from_utf8_lossy(&out);
            let stat = text.split('\0').next().unwrap_or("");
            let mut parts = stat.splitn(3, '\t');
            let n = |s: Option<&str>| s.and_then(|s| s.parse().ok()).unwrap_or(0);
            return Ok(Some((n(parts.next()), n(parts.next()))));
        }
        let tracked = self.status(&["ls-files", "--error-unmatch", "--", path])?.0 == 0;
        if tracked {
            return Ok(None);
        }
        match worktree_file(self.repo(), path) {
            Some(file) => {
                let bytes = std::fs::read(file).unwrap_or_default();
                Ok(Some((bytes.split(|&b| b == b'\n').count() as u32, 0)))
            }
            None => Ok(None),
        }
    }

    /// The shas in `base..head`.
    pub fn commits_between(&self, base: &str, head: &str) -> Result<HashSet<String>, SourceError> {
        let out = self.text(&["rev-list", &format!("{base}..{head}")])?;
        Ok(out.split_whitespace().map(String::from).collect())
    }

    /// `path` at commit `sha`, or in the working tree when `sha` is `None`; `Ok(None)` when
    /// the file doesn't exist there.
    pub fn read_blob(&self, sha: Option<&str>, path: &str) -> Result<Option<Vec<u8>>, SourceError> {
        let Some(sha) = sha else {
            return Ok(match worktree_file(self.repo(), path) {
                Some(file) => {
                    Some(std::fs::read(file).map_err(|e| SourceError::Message(e.to_string()))?)
                }
                None => None,
            });
        };
        let spec = format!("{sha}:{path}");
        Ok(self.read_blobs(std::slice::from_ref(&spec))?.remove(&spec))
    }

    /// Blame for `path` at `sha` (the working tree when `None`).
    pub fn blame(&self, sha: Option<&str>, path: &str) -> Result<Blame, SourceError> {
        let mut args = vec!["blame", "--porcelain"];
        if let Some(sha) = sha {
            args.push(sha);
        }
        args.extend(["--", path]);
        let out = self.run(&args)?;
        let parsed = blame_porcelain(&out);
        let mut commits: Vec<BlameCommit> = Vec::new();
        let mut index: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut lines = Vec::with_capacity(parsed.len());
        let mut origin = Vec::with_capacity(parsed.len());
        for (line_sha, orig, filename) in parsed {
            let idx = match index.get(&line_sha) {
                Some(i) => *i,
                None => {
                    let uncommitted = line_sha.bytes().all(|b| b == b'0');
                    let i = commits.len() as u32;
                    commits.push(BlameCommit {
                        sha: if uncommitted {
                            WORKTREE_SHA.into()
                        } else {
                            line_sha.clone()
                        },
                        short: if uncommitted {
                            "work".into()
                        } else {
                            line_sha[..7.min(line_sha.len())].to_string()
                        },
                        subject: if uncommitted {
                            "Uncommitted changes".into()
                        } else {
                            String::new()
                        },
                        author: String::new(),
                        date: String::new(),
                        path: filename,
                    });
                    index.insert(line_sha, i);
                    i
                }
            };
            lines.push(idx);
            origin.push(orig);
        }
        // Subject, author and date in one call, for every real commit.
        let shas: Vec<&str> = commits
            .iter()
            .filter(|c| c.sha != WORKTREE_SHA)
            .map(|c| c.sha.as_str())
            .collect();
        if !shas.is_empty() {
            let input = shas.join("\n") + "\n";
            let out = self.run_input(
                &["log", "--no-walk", "--stdin", LOG_FORMAT],
                Some(input.as_bytes()),
            )?;
            for (head, _) in records(&String::from_utf8_lossy(&out)) {
                let f = |i: usize| head.get(i).cloned().unwrap_or_default();
                if let Some(i) = index.get(&f(0)) {
                    let c = &mut commits[*i as usize];
                    c.short = f(1);
                    c.subject = f(2);
                    c.author = f(3);
                    c.date = f(4);
                }
            }
        }
        Ok(Blame {
            commits,
            lines,
            origin,
        })
    }

    /// No tracked file has uncommitted changes (untracked files don't block a checkout).
    pub fn is_clean(&self) -> Result<bool, SourceError> {
        Ok(self
            .run(&["status", "--porcelain", "--untracked-files=no"])?
            .is_empty())
    }

    /// `path` has no uncommitted changes.
    pub fn path_is_clean(&self, path: &str) -> Result<bool, SourceError> {
        Ok(self.run(&["status", "--porcelain", "--", path])?.is_empty())
    }

    /// `path` exists at commit `sha`.
    pub fn has_path_at(&self, sha: &str, path: &str) -> bool {
        self.run(&["cat-file", "-e", &format!("{sha}:{path}")])
            .is_ok()
    }

    pub fn checkout_commit(&self, sha: &str) -> Result<(), SourceError> {
        self.run(&["checkout", "--quiet", sha]).map(|_| ())
    }

    pub fn checkout_file(&self, sha: &str, path: &str) -> Result<(), SourceError> {
        self.run(&["checkout", "--quiet", sha, "--", path])
            .map(|_| ())
    }
}
