//! `/api/report/{id}/summary.md` and `/api/report/{id}/commits`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use refactor_diff_core::{ReviewMarks, markdown_summary};
use serde_json::{Value, json};

use super::review_of;
use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;

/// The review summary as Markdown (what "Copy as Markdown" copies).
pub async fn get_summary(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Response, ApiError> {
    let report = state.report(&report_id)?;
    let review = review_of(&state, &report);
    let marks = ReviewMarks::new(review.groups, review.hunks);
    let text = markdown_summary(&report, Some(&marks));
    Ok((
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        text,
    )
        .into_response())
}

/// The commits between base and head, oldest first (empty for the working tree).
pub async fn get_commits(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let commits = run_blocking(move || {
        state
            .git
            .list_commits(&report.source.base_sha, report.source.head_sha.as_deref())
    })
    .await??;
    Ok(Json(json!({"commits": commits})))
}
