//! The report's pull request on GitHub: posting single comments (`pr/comment`,
//! `pr/review-comment`), reading the conversation, acting on threads and comments, the local
//! pending review (`pr/draft/...`) and submitting it (`pr/review`).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use refactor_diff_core::{Hunk, Report, anchor_line};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::git::{CommentKind, ReviewCommentPayload, ReviewPayload, ReviewPayloadComment};
use crate::http::Side;
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, py_int, str_or_empty, truthy};
use crate::pr_draft::{Draft, DraftComment};

/// The PR number of a report, if it came from one.
fn pr_of(report: &Report) -> Result<i64, ApiError> {
    report
        .source
        .pr
        .as_ref()
        .and_then(|pr| pr.get("number"))
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::BadRequest("This report isn't a pull request.".into()))
}

#[derive(Debug, Default, Deserialize)]
pub struct CommentBody {
    #[serde(default)]
    pub body: Value,
}

/// Post `{"body"}` as a comment on the report's pull request.
pub async fn post_pr_comment(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<CommentBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let number = pr_of(&report)?;
    let text = body
        .body
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ApiError::BadRequest("The comment is empty.".into()))?
        .to_string();
    let url = run_blocking(move || state.github.post_pr_comment(number, &text)).await??;
    Ok(Json(json!({"url": url})))
}

#[derive(Debug, Default, Deserialize)]
pub struct ReviewCommentBody {
    #[serde(default)]
    pub body: Value,
    #[serde(default)]
    pub hunk_id: Value,
    #[serde(default)]
    pub line: Value,
    #[serde(default)]
    pub side: Value,
}

/// Where a comment on `hunk` goes: its first changed line unless `line`/`side` override it.
/// Returns `(path, line, side)` with the path on that side.
fn review_target(
    report: &Report,
    hunk: &Hunk,
    line: &Value,
    side: &Value,
) -> Result<(String, i64, String), ApiError> {
    let first = anchor_line(report, hunk);
    let side = if truthy(side) {
        match side {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    } else if first.new_no.is_some_and(|n| n != 0) {
        "RIGHT".to_string()
    } else {
        "LEFT".to_string()
    };
    let right = side == "RIGHT";
    let line = if truthy(line) {
        py_int(line)
    } else if right {
        first.new_no.map(i64::from)
    } else {
        first.old_no.map(i64::from)
    }
    .ok_or_else(|| ApiError::BadRequest("line must be an integer.".into()))?;
    let path = if right {
        hunk.path.clone()
    } else {
        Side::Old.path(report, &hunk.path).to_string()
    };
    Ok((path, line, side))
}

/// Post `{"body", "hunk_id"}` as an inline review comment on the hunk's first changed line
/// (`"line"`/`"side"` override it).
pub async fn post_review_comment(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<ReviewCommentBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let number = pr_of(&report)?;
    let text = body.body.as_str().filter(|s| !s.trim().is_empty());
    let hunk = report.hunks.get(str_or_empty(&body.hunk_id));
    let (Some(text), Some(hunk)) = (text, hunk) else {
        return Err(ApiError::BadRequest(
            "A comment and a hunk are required.".into(),
        ));
    };
    let (path, line, side) = review_target(&report, hunk, &body.line, &body.side)?;
    let payload = ReviewCommentPayload {
        body: text.to_string(),
        commit_id: report.source.head_sha.clone().unwrap_or_default(),
        path: path.clone(),
        line,
        side: side.clone(),
    };
    let url = run_blocking(move || state.github.post_review_comment(number, &payload)).await??;
    Ok(Json(
        json!({"url": url, "path": path, "line": line, "side": side}),
    ))
}

/// A non-blank `body` field.
fn body_text(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// The report hunk a thread's line falls in: `path` is the new path, or the old one on `LEFT`.
fn thread_hunk(report: &Report, path: &str, side: &str, line: i64) -> Option<String> {
    let left = side == "LEFT";
    report
        .hunks
        .values()
        .filter(|h| h.path == path || (left && Side::Old.path(report, &h.path) == path))
        .find(|h| {
            h.lines.iter().any(|l| {
                let n = if left { l.old_no } else { l.new_no };
                n.map(i64::from) == Some(line)
            })
        })
        .map(|h| h.id.clone())
}

/// The PR's comments, reviews and threads, each thread with the report hunk it lands in.
pub async fn get_conversation(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let number = pr_of(&report)?;
    let st = state.clone();
    let mut conv = run_blocking(move || st.github.pr_conversation(number)).await??;
    for t in &mut conv.threads {
        t.hunk_id = t
            .line
            .filter(|_| !t.outdated)
            .and_then(|line| thread_hunk(&report, &t.path, &t.side, line));
    }
    let mut value = serde_json::to_value(&conv).expect("conversation serializes");
    value["draft"] = draft_json(&state.drafts.load(number));
    Ok(Json(value))
}

fn draft_json(draft: &Draft) -> Value {
    json!({"head_sha": draft.head_sha, "comments": draft.comments})
}

#[derive(Debug, Default, Deserialize)]
pub struct ReplyBody {
    #[serde(default)]
    pub comment_id: Value,
    #[serde(default)]
    pub body: Value,
}

/// Reply `{"body"}` in the thread of review comment `comment_id`.
pub async fn reply(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<ReplyBody>,
) -> Result<Json<Value>, ApiError> {
    let number = pr_of(&*state.report(&report_id)?)?;
    let (Some(id), Some(text)) = (py_int(&body.comment_id), body_text(&body.body)) else {
        return Err(ApiError::BadRequest(
            "A reply and the comment it answers are required.".into(),
        ));
    };
    let url = run_blocking(move || state.github.reply_to_comment(number, id, &text)).await??;
    Ok(Json(json!({"url": url})))
}

#[derive(Debug, Default, Deserialize)]
pub struct ResolveBody {
    #[serde(default)]
    pub thread_id: Value,
    #[serde(default)]
    pub resolved: Value,
}

/// Resolve (`"resolved": true`) or unresolve a review thread.
pub async fn resolve(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<ResolveBody>,
) -> Result<Json<Value>, ApiError> {
    pr_of(&*state.report(&report_id)?)?;
    let id = str_or_empty(&body.thread_id).to_string();
    if id.is_empty() {
        return Err(ApiError::BadRequest("A thread is required.".into()));
    }
    let resolved = truthy(&body.resolved);
    run_blocking(move || state.github.set_thread_resolved(&id, resolved)).await??;
    Ok(Json(json!({"resolved": resolved})))
}

#[derive(Debug, Default, Deserialize)]
pub struct CommentRef {
    #[serde(default)]
    pub kind: Value,
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub body: Value,
}

fn comment_ref(body: &CommentRef) -> Result<(CommentKind, i64), ApiError> {
    let kind = CommentKind::parse(str_or_empty(&body.kind))
        .ok_or_else(|| ApiError::BadRequest("kind must be \"issue\" or \"review\".".into()))?;
    let id =
        py_int(&body.id).ok_or_else(|| ApiError::BadRequest("id must be an integer.".into()))?;
    Ok((kind, id))
}

/// Replace the body of one of your comments: `{"kind", "id", "body"}`.
pub async fn edit_comment(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<CommentRef>,
) -> Result<Json<Value>, ApiError> {
    pr_of(&*state.report(&report_id)?)?;
    let (kind, id) = comment_ref(&body)?;
    let text = body_text(&body.body)
        .ok_or_else(|| ApiError::BadRequest("The comment is empty.".into()))?;
    run_blocking(move || state.github.edit_comment(kind, id, &text)).await??;
    Ok(Json(json!({"ok": true})))
}

/// Delete one of your comments: `{"kind", "id"}`.
pub async fn delete_comment(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<CommentRef>,
) -> Result<Json<Value>, ApiError> {
    pr_of(&*state.report(&report_id)?)?;
    let (kind, id) = comment_ref(&body)?;
    run_blocking(move || state.github.delete_comment(kind, id)).await??;
    Ok(Json(json!({"ok": true})))
}

/// The pending review.
pub async fn get_draft(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let number = pr_of(&*state.report(&report_id)?)?;
    Ok(Json(draft_json(&state.drafts.load(number))))
}

#[derive(Debug, Default, Deserialize)]
pub struct DraftCommentBody {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub body: Value,
    #[serde(default)]
    pub hunk_id: Value,
    #[serde(default)]
    pub line: Value,
    #[serde(default)]
    pub side: Value,
}

/// Add `{"body", "hunk_id", "line"?, "side"?}` to the pending review, or with `"id"` change
/// that comment's body.
pub async fn draft_comment(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<DraftCommentBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let number = pr_of(&report)?;
    let text = body_text(&body.body)
        .ok_or_else(|| ApiError::BadRequest("The comment is empty.".into()))?;
    let id = str_or_empty(&body.id);
    let draft = if !id.is_empty() {
        state
            .drafts
            .edit(number, id, &text)?
            .ok_or_else(|| ApiError::NotFound("That pending comment is gone.".into()))?
    } else {
        let hunk = report
            .hunks
            .get(str_or_empty(&body.hunk_id))
            .ok_or_else(|| ApiError::BadRequest("A comment and a hunk are required.".into()))?;
        let (path, line, side) = review_target(&report, hunk, &body.line, &body.side)?;
        let comment = DraftComment {
            path,
            line,
            side,
            hunk_id: Some(hunk.id.clone()),
            body: text,
            ..Default::default()
        };
        state
            .drafts
            .add(number, report.source.head_sha.as_deref(), comment)?
    };
    Ok(Json(draft_json(&draft)))
}

#[derive(Debug, Default, Deserialize)]
pub struct DraftRef {
    #[serde(default)]
    pub id: Value,
}

/// Drop `{"id"}` from the pending review.
pub async fn draft_delete(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<DraftRef>,
) -> Result<Json<Value>, ApiError> {
    let number = pr_of(&*state.report(&report_id)?)?;
    let draft = state.drafts.remove(number, str_or_empty(&body.id))?;
    Ok(Json(draft_json(&draft)))
}

/// Throw the pending review away.
pub async fn draft_discard(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let number = pr_of(&*state.report(&report_id)?)?;
    state.drafts.discard(number)?;
    Ok(Json(draft_json(&Draft::default())))
}

#[derive(Debug, Default, Deserialize)]
pub struct SubmitBody {
    #[serde(default)]
    pub event: Value,
    #[serde(default)]
    pub body: Value,
}

/// Submit the pending review as `{"event": "COMMENT" | "APPROVE" | "REQUEST_CHANGES", "body"}`.
pub async fn submit_review(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<SubmitBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let number = pr_of(&report)?;
    let event = str_or_empty(&body.event).to_string();
    if !matches!(event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES") {
        return Err(ApiError::BadRequest(
            "event must be COMMENT, APPROVE or REQUEST_CHANGES.".into(),
        ));
    }
    let text = body_text(&body.body).unwrap_or_default();
    let draft = state.drafts.load(number);
    if event == "REQUEST_CHANGES" && text.is_empty() {
        return Err(ApiError::BadRequest(
            "Say what needs to change: requesting changes needs a comment.".into(),
        ));
    }
    if event == "COMMENT" && text.is_empty() && draft.comments.is_empty() {
        return Err(ApiError::BadRequest(
            "Nothing to submit: write a comment or add inline comments first.".into(),
        ));
    }
    let st = state.clone();
    let conv = run_blocking(move || st.github.pr_conversation(number)).await??;
    if conv.viewer_is_author && event != "COMMENT" {
        return Err(ApiError::BadRequest(
            "GitHub doesn't let you approve or request changes on your own pull request.".into(),
        ));
    }
    if conv.pending_review.is_some() {
        return Err(ApiError::BadRequest(
            "You have a pending review started on GitHub; submit or discard it there first.".into(),
        ));
    }
    let payload = ReviewPayload {
        commit_id: draft
            .head_sha
            .clone()
            .or_else(|| report.source.head_sha.clone())
            .unwrap_or_default(),
        body: text,
        event: event.clone(),
        comments: draft
            .comments
            .iter()
            .map(|c| ReviewPayloadComment {
                path: c.path.clone(),
                line: c.line,
                side: c.side.clone(),
                body: c.body.clone(),
            })
            .collect(),
    };
    let st = state.clone();
    let url = run_blocking(move || st.github.submit_review(number, &payload)).await??;
    state.drafts.discard(number)?;
    Ok(Json(
        json!({"url": url, "event": event, "comments": draft.comments.len()}),
    ))
}
