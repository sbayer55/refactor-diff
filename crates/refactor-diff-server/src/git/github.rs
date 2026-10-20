//! The GitHub CLI, behind a trait so tests can fake it.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::SourceError;
use super::conversation::{CONVERSATION_QUERY, CommentKind, Conversation, Cursors, ReviewPayload};
use crate::exec::Tools;

/// At most this many pages of threads/comments are fetched for one conversation.
const MAX_PAGES: usize = 20;

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
    /// The PR's comments, reviews and review threads.
    fn pr_conversation(&self, number: i64) -> Result<Conversation, SourceError>;
    /// Reply in the thread of review comment `comment_id`; returns the reply's URL.
    fn reply_to_comment(
        &self,
        number: i64,
        comment_id: i64,
        body: &str,
    ) -> Result<String, SourceError>;
    /// Resolve or unresolve a review thread (by GraphQL node id).
    fn set_thread_resolved(&self, thread_id: &str, resolved: bool) -> Result<(), SourceError>;
    fn edit_comment(&self, kind: CommentKind, id: i64, body: &str) -> Result<(), SourceError>;
    fn delete_comment(&self, kind: CommentKind, id: i64) -> Result<(), SourceError>;
    /// Submit a review (with its inline comments); returns its URL.
    fn submit_review(&self, number: i64, payload: &ReviewPayload) -> Result<String, SourceError>;
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

    /// `gh api --method <method> <endpoint> --input -` with a JSON body; the response's
    /// `html_url`, or `""`.
    fn rest(
        &self,
        method: &str,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<String, SourceError> {
        let input = body.map(|b| serde_json::to_vec(b).expect("body serializes"));
        let mut args = vec!["api", "--method", method, endpoint];
        if input.is_some() {
            args.extend(["--input", "-"]);
        }
        let out = self.run(&args, input.as_deref(), 4)?;
        Ok(html_url(&out))
    }

    /// `gh api graphql` with `-f`/`-F` fields; GraphQL errors become errors.
    fn graphql(&self, fields: &[(&str, &str, String)]) -> Result<Value, SourceError> {
        let mut args: Vec<String> = vec!["api".into(), "graphql".into()];
        for (flag, key, value) in fields {
            args.push((*flag).into());
            args.push(format!("{key}={value}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.run(&args, None, 2)?;
        let v: Value = serde_json::from_slice(&out)
            .map_err(|e| SourceError::Message(format!("invalid JSON from gh: {e}")))?;
        if let Some(errors) = v.get("errors").and_then(Value::as_array) {
            let msgs: Vec<&str> = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str))
                .collect();
            return Err(SourceError::Gh {
                args: "api graphql".into(),
                stderr: msgs.join("; "),
            });
        }
        Ok(v)
    }
}

fn html_url(out: &[u8]) -> String {
    serde_json::from_slice::<Value>(out)
        .ok()
        .and_then(|v| {
            v.get("html_url")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
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
        Ok(html_url(&out))
    }

    fn pr_conversation(&self, number: i64) -> Result<Conversation, SourceError> {
        let mut cursors = Cursors::default();
        let mut all: Option<Conversation> = None;
        for _ in 0..MAX_PAGES {
            let mut fields = vec![
                ("-f", "query", CONVERSATION_QUERY.to_string()),
                ("-F", "owner", "{owner}".to_string()),
                ("-F", "repo", "{repo}".to_string()),
                ("-F", "number", number.to_string()),
            ];
            if let Some(c) = &cursors.threads {
                fields.push(("-f", "threads", c.clone()));
            }
            if let Some(c) = &cursors.comments {
                fields.push(("-f", "comments", c.clone()));
            }
            let (page, next) = Conversation::from_page(&self.graphql(&fields)?)?;
            match &mut all {
                None => all = Some(page),
                Some(conv) => conv.extend(page),
            }
            if next.done() {
                break;
            }
            cursors = next.after(&cursors);
        }
        Ok(all.unwrap_or_default())
    }

    fn reply_to_comment(
        &self,
        number: i64,
        comment_id: i64,
        body: &str,
    ) -> Result<String, SourceError> {
        let endpoint =
            format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments/{comment_id}/replies");
        self.rest(
            "POST",
            &endpoint,
            Some(&serde_json::json!({ "body": body })),
        )
    }

    fn set_thread_resolved(&self, thread_id: &str, resolved: bool) -> Result<(), SourceError> {
        let op = if resolved {
            "resolveReviewThread"
        } else {
            "unresolveReviewThread"
        };
        let query = format!(
            "mutation($id: ID!) {{ {op}(input: {{threadId: $id}}) {{ thread {{ id }} }} }}"
        );
        self.graphql(&[("-f", "query", query), ("-f", "id", thread_id.to_string())])?;
        Ok(())
    }

    fn edit_comment(&self, kind: CommentKind, id: i64, body: &str) -> Result<(), SourceError> {
        self.rest(
            "PATCH",
            &kind.endpoint(id),
            Some(&serde_json::json!({ "body": body })),
        )?;
        Ok(())
    }

    fn delete_comment(&self, kind: CommentKind, id: i64) -> Result<(), SourceError> {
        self.rest("DELETE", &kind.endpoint(id), None)?;
        Ok(())
    }

    fn submit_review(&self, number: i64, payload: &ReviewPayload) -> Result<String, SourceError> {
        let endpoint = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
        let body = serde_json::to_value(payload).expect("payload serializes");
        self.rest("POST", &endpoint, Some(&body))
    }
}
