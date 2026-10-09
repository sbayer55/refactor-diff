//! The GitHub CLI, behind a trait so tests can fake it.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::SourceError;
use crate::exec::Tools;

/// What `gh pr view --json` returns, plus the fields the server reads.
#[derive(Clone, Debug, PartialEq)]
pub struct PrInfo {
    pub raw: Value,
    pub number: i64,
    pub title: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub base_ref_oid: String,
    pub head_ref_oid: String,
}

impl PrInfo {
    pub fn from_value(raw: Value) -> Result<Self, SourceError> {
        let s = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_string);
        Ok(Self {
            number: raw.get("number").and_then(Value::as_i64).unwrap_or(0),
            title: s("title").unwrap_or_default(),
            base_ref_name: s("baseRefName")
                .ok_or_else(|| SourceError::Message("gh returned no baseRefName".into()))?,
            head_ref_name: s("headRefName")
                .ok_or_else(|| SourceError::Message("gh returned no headRefName".into()))?,
            base_ref_oid: s("baseRefOid")
                .ok_or_else(|| SourceError::Message("gh returned no baseRefOid".into()))?,
            head_ref_oid: s("headRefOid")
                .ok_or_else(|| SourceError::Message("gh returned no headRefOid".into()))?,
            raw,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewCommentPayload {
    pub body: String,
    pub commit_id: String,
    pub path: String,
    pub line: i64,
    pub side: String,
}

pub trait GitHub: Send + Sync {
    fn available(&self) -> bool;
    fn pr_list(&self) -> Result<Vec<Value>, SourceError>;
    fn pr_view(&self, number: i64) -> Result<PrInfo, SourceError>;
    /// Post a comment on the PR's conversation; returns its URL.
    fn post_pr_comment(&self, number: i64, body: &str) -> Result<String, SourceError>;
    /// Post an inline review comment on one line of the PR's diff; returns its URL.
    fn post_review_comment(
        &self,
        number: i64,
        payload: &ReviewCommentPayload,
    ) -> Result<String, SourceError>;
}

/// The real `gh`, run inside the repository.
pub struct GhCli {
    repo: PathBuf,
    tools: Arc<Tools>,
}

impl GhCli {
    pub fn new(repo: PathBuf, tools: Arc<Tools>) -> Self {
        Self { repo, tools }
    }

    fn run(
        &self,
        args: &[&str],
        input: Option<&[u8]>,
        err_args: usize,
    ) -> Result<Vec<u8>, SourceError> {
        if !self.available() {
            return Err(SourceError::GhMissing);
        }
        let mut cmd = self.tools.command("gh");
        cmd.args(args).current_dir(&self.repo);
        cmd.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| SourceError::Spawn {
            program: "gh".into(),
            detail: e.to_string(),
        })?;
        if let Some(bytes) = input {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("piped stdin");
            let _ = stdin.write_all(bytes);
        }
        let output = child.wait_with_output().map_err(|e| SourceError::Spawn {
            program: "gh".into(),
            detail: e.to_string(),
        })?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(SourceError::Gh {
                args: args
                    .iter()
                    .take(err_args)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    fn json(&self, args: &[&str]) -> Result<Value, SourceError> {
        let out = self.run(args, None, args.len())?;
        serde_json::from_slice(&out)
            .map_err(|e| SourceError::Message(format!("invalid JSON from gh: {e}")))
    }
}

impl GitHub for GhCli {
    fn available(&self) -> bool {
        self.tools.which("gh").is_some()
    }

    fn pr_list(&self) -> Result<Vec<Value>, SourceError> {
        let v = self.json(&[
            "pr",
            "list",
            "--limit",
            "50",
            "--json",
            "number,title,headRefName,baseRefName,author",
        ])?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    fn pr_view(&self, number: i64) -> Result<PrInfo, SourceError> {
        let v = self.json(&[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "number,title,url,body,baseRefName,headRefName,baseRefOid,headRefOid",
        ])?;
        PrInfo::from_value(v)
    }

    fn post_pr_comment(&self, number: i64, body: &str) -> Result<String, SourceError> {
        let out = self.run(
            &["pr", "comment", &number.to_string(), "--body-file", "-"],
            Some(body.as_bytes()),
            2,
        )?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    fn post_review_comment(
        &self,
        number: i64,
        payload: &ReviewCommentPayload,
    ) -> Result<String, SourceError> {
        let input = serde_json::to_vec(payload).expect("payload serializes");
        let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments");
        let out = self.run(
            &["api", "--method", "POST", &endpoint, "--input", "-"],
            Some(&input),
            2,
        )?;
        let text = String::from_utf8_lossy(&out);
        Ok(serde_json::from_str::<Value>(text.trim())
            .ok()
            .and_then(|v| {
                v.get("html_url")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default())
    }
}
