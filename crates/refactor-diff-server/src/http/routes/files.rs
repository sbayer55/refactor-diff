//! Whole files: `/api/report/{id}/file` (a changed file's diff) and
//! `/api/report/{id}/source` (any file at one side's revision).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use refactor_diff_core::{FileDiff, file_diff, split_lines};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::http::Side;
use crate::http::error::ApiError;

#[derive(Debug, Default, Deserialize)]
pub struct FileQuery {
    #[serde(default)]
    pub path: String,
}

/// Whole-file diff of one changed file in a report (`?path=`).
pub async fn get_file(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Result<Json<FileDiff>, ApiError> {
    let report = state.report(&report_id)?;
    file_diff(&report, &query.path)
        .map(Json)
        .map_err(|e| ApiError::NotFound(e.to_string()))
}

#[derive(Debug, Default, Deserialize)]
pub struct SourceQuery {
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub path: String,
}

/// Any repository file at one side's revision (`?side=old|new&path=`).
pub async fn get_source(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<SourceQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let side = query
        .side
        .as_deref()
        .and_then(Side::parse)
        .ok_or_else(|| ApiError::BadRequest("side must be old or new.".into()))?;
    let sha = side.sha(&report).map(str::to_string);
    let path = query.path;
    let st = state.clone();
    let texts = {
        let path = path.clone();
        run_blocking(move || st.git.read_files(sha.as_deref(), &[path.as_str()])).await??
    };
    let Some(text) = texts.get(&path) else {
        return Err(ApiError::NotFound(format!(
            "{path} doesn't exist in the {} version.",
            side.name()
        )));
    };
    Ok(Json(json!({
        "path": path,
        "side": side.as_str(),
        "lines": split_lines(text),
    })))
}
