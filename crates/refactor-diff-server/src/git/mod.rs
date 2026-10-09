//! Git and GitHub access: everything the server asks a repository through `git` and `gh`.

mod commits;
mod github;
mod parse;
mod sources;

pub use commits::{Commit, TouchingCommit, line_history_new, line_history_old};
pub use github::{GhCli, GitHub, PrInfo, ReviewCommentPayload};
pub use parse::{NameStatus, cat_file_batch, commit_records, name_status_z};
pub use sources::{HeadLookup, ResolvedSource, SourcesInfo, WORKTREE};

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use crate::exec::Tools;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum SourceError {
    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },
    #[error("gh {args} failed: {stderr}")]
    Gh { args: String, stderr: String },
    #[error("The GitHub CLI (gh) is not installed or not on PATH.")]
    GhMissing,
    #[error("Couldn't run {program}: {detail}")]
    Spawn { program: String, detail: String },
    #[error("{0}")]
    Message(String),
}

/// Runs `git -C <repo> ...`. Synchronous: callers run it on a blocking thread.
#[derive(Clone, Debug)]
pub struct Git {
    repo: PathBuf,
    tools: Arc<Tools>,
}

impl Git {
    pub fn new(repo: impl Into<PathBuf>, tools: Arc<Tools>) -> Self {
        Self {
            repo: repo.into(),
            tools,
        }
    }

    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn tools(&self) -> &Arc<Tools> {
        &self.tools
    }

    /// The repository's top-level directory for any path inside it.
    pub fn repo_root(path: &Path, tools: &Tools) -> Result<PathBuf, SourceError> {
        let git = Git {
            repo: path.to_path_buf(),
            tools: Arc::new(tools.clone()),
        };
        git.text(&["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
    }

    /// Stdout of a git command; a non-zero exit is an error carrying stderr.
    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>, SourceError> {
        self.run_input(args, None)
    }

    pub fn run_input(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, SourceError> {
        let mut cmd = self.tools.command("git");
        cmd.arg("-C").arg(&self.repo).args(args);
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| SourceError::Spawn {
            program: "git".into(),
            detail: e.to_string(),
        })?;
        if let Some(bytes) = input {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("piped stdin");
            // Write on a thread so a large input cannot deadlock against a filling stdout.
            let bytes = bytes.to_vec();
            let writer = std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
            let output = child.wait_with_output().map_err(|e| SourceError::Spawn {
                program: "git".into(),
                detail: e.to_string(),
            })?;
            let _ = writer.join();
            return finish(args, output);
        }
        let output = child.wait_with_output().map_err(|e| SourceError::Spawn {
            program: "git".into(),
            detail: e.to_string(),
        })?;
        finish(args, output)
    }

    /// Stdout as text, trimmed.
    pub fn text(&self, args: &[&str]) -> Result<String, SourceError> {
        Ok(String::from_utf8_lossy(&self.run(args)?).trim().to_string())
    }

    /// The exit status and stdout of a command, for commands whose failure is a result rather
    /// than an error (`git grep`).
    pub fn status(&self, args: &[&str]) -> Result<(i32, Vec<u8>), SourceError> {
        let output = self
            .tools
            .command("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SourceError::Spawn {
                program: "git".into(),
                detail: e.to_string(),
            })?;
        Ok((output.status.code().unwrap_or(-1), output.stdout))
    }

    pub fn rev(&self, r: &str) -> Result<String, SourceError> {
        self.text(&["rev-parse", "--verify", &format!("{r}^{{commit}}")])
    }

    pub fn has_commit(&self, sha: &str) -> bool {
        self.run(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .is_ok()
    }

    /// Remotes to try for fallbacks, most likely first.
    pub fn remotes(&self) -> Vec<String> {
        let all: Vec<String> = self
            .text(&["remote"])
            .map(|t| t.split_whitespace().map(String::from).collect())
            .unwrap_or_default();
        let preferred: Vec<String> = ["origin", "upstream"]
            .iter()
            .filter(|r| all.iter().any(|a| a == *r))
            .map(|r| r.to_string())
            .collect();
        if preferred.is_empty() {
            all.into_iter().take(1).collect()
        } else {
            preferred
        }
    }
}

fn finish(args: &[&str], output: std::process::Output) -> Result<Vec<u8>, SourceError> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(SourceError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}
