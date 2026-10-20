//! `/api/report/{id}/xref?concept=<stem>`: cross-reference checks for what a change left
//! behind (dangling concept names, orphaned translation keys, payloads that lose a field).
//! Code issues, separate from the report's refactor warnings.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;
use crate::xref::{self, Xref};

#[derive(Debug, Default, Deserialize)]
pub struct XrefQuery {
    #[serde(default)]
    pub concept: Option<String>,
}

/// The report's xref checks for `?concept=` (default: the report's strongest concept; with
/// none, only the checks that don't need one). Cached per report and concept.
pub async fn get_xref(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<XrefQuery>,
) -> Result<Json<Xref>, ApiError> {
    let report = state.report(&report_id)?;
    let concept = query
        .concept
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| report.concepts.first().map(|c| c.stem.clone()));
    if let Some(c) = &concept {
        if c.len() < 2
            || c.len() > 64
            || !c
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return Err(ApiError::BadRequest(
                "concept must be 2-64 letters, digits, `_` or `-`.".into(),
            ));
        }
    }
    let result =
        run_blocking(move || xref::cached(&state.git, &report, concept.as_deref())).await?;
    Ok(Json((*result).clone()))
}
