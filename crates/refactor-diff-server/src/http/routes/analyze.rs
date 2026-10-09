//! `POST /api/analyze` and `GET /api/report/{id}`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use refactor_diff_core::{Report, analyze};
use serde::Deserialize;
use serde_json::Value;

use super::{hunks_of, with_review};
use crate::app::{AppState, run_blocking};
use crate::git::HeadLookup;
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, int_or_empty, py_int, truthy};

/// `{"base", "head", "pr", "min_count"}`; every field optional, numbers may be strings.
#[derive(Debug, Default, Deserialize)]
pub struct AnalyzeBody {
    #[serde(default)]
    pub base: Value,
    #[serde(default)]
    pub head: Value,
    #[serde(default)]
    pub pr: Value,
    #[serde(default)]
    pub min_count: Value,
}

/// Resolve the source, load the diff, analyze it and remember the report; the response is
/// the report with its review state.
pub async fn run_analysis(
    State(state): State<Arc<AppState>>,
    ApiJson(body): ApiJson<AnalyzeBody>,
) -> Result<Json<Value>, ApiError> {
    let ints_error = || ApiError::BadRequest("PR number and min count must be integers.".into());
    let pr = int_or_empty(&body.pr).map_err(|_| ints_error())?;
    // `max(1, int(body.get("min_count") or 2))`
    let min_count = if truthy(&body.min_count) {
        py_int(&body.min_count).ok_or_else(ints_error)?
    } else {
        2
    };
    let min_count = usize::try_from(min_count.max(1)).unwrap_or(1);
    let base = body.base.as_str().map(str::to_string);
    let head = body.head.as_str().map(str::to_string);

    let st = state.clone();
    let report = run_blocking(move || -> Result<Report, ApiError> {
        let source = st
            .git
            .resolve(base.as_deref(), head.as_deref(), pr, &*st.github)?;
        let changes = st.git.load_changes(&source)?;
        let head = HeadLookup {
            git: &st.git,
            head_sha: source.head_sha.as_deref(),
        };
        let report = analyze(source.to_core(), changes, min_count, &head);
        st.reviews.record_analysis(
            &report.source.identity,
            report.source.head_sha.as_deref(),
            &hunks_of(&report),
        )?;
        Ok(report)
    })
    .await??;
    let report = state.insert_report(report);
    Ok(Json(with_review(&state, &report)))
}

/// A cached report with its review state.
pub async fn get_report(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    Ok(Json(with_review(&state, &report)))
}
