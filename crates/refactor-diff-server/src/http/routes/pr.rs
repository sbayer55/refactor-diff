//! Posting to the report's pull request: `/api/report/{id}/pr/comment` and
//! `/api/report/{id}/pr/review-comment`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use refactor_diff_core::{Report, anchor_line};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::git::ReviewCommentPayload;
use crate::http::Side;
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, py_int, str_or_empty, truthy};

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
    let first = anchor_line(&report, hunk);
    let side = if truthy(&body.side) {
        match &body.side {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    } else if first.new_no.is_some_and(|n| n != 0) {
        "RIGHT".to_string()
    } else {
        "LEFT".to_string()
    };
    let right = side == "RIGHT";
    let line = if truthy(&body.line) {
        py_int(&body.line)
    } else if right {
        first.new_no.map(i64::from)
    } else {
        first.old_no.map(i64::from)
    }
    .ok_or_else(|| ApiError::BadRequest("line must be an integer.".into()))?;
    let path = if right {
        hunk.path.clone()
    } else {
        Side::Old.path(&report, &hunk.path).to_string()
    };
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
