//! Code navigation: `/api/report/{id}/navigate` (go to definition / find references) and
//! `/api/library` (a library file a result pointed at).
//!

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::Value;

use crate::app::{AppState, run_blocking};
use crate::http::Side;
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, py_int, str_or_empty};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Definition,
    References,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "definition" => Some(Action::Definition),
            "references" => Some(Action::References),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Definition => "definition",
            Action::References => "references",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct NavigateBody {
    #[serde(default)]
    pub action: Value,
    #[serde(default)]
    pub side: Value,
    #[serde(default)]
    pub line: Value,
    #[serde(default)]
    pub col: Value,
    #[serde(default)]
    pub path: Value,
}

/// A validated navigation request: where to look and what for.
#[derive(Debug, PartialEq, Eq)]
pub struct NavigateQuery {
    pub action: Action,
    pub side: Side,
    /// The path on that side (a renamed file's old side lives at its old path).
    pub path: String,
    pub line: i64,
    pub col: i64,
}

/// Go to definition / find references for the name at (line, col) on one side.
pub async fn navigate(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<NavigateBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let (Some(line), Some(col)) = (py_int(&body.line), py_int(&body.col)) else {
        return Err(ApiError::BadRequest(
            "line and col must be integers.".into(),
        ));
    };
    let action = body.action.as_str().and_then(Action::parse);
    let side = body.side.as_str().and_then(Side::parse);
    let (Some(action), Some(side)) = (action, side) else {
        return Err(ApiError::BadRequest("Unknown action or side.".into()));
    };
    let query = NavigateQuery {
        action,
        side,
        path: side.path(&report, str_or_empty(&body.path)).to_string(),
        line,
        col,
    };
    if query.line < 1 || query.col < 0 {
        return Err(ApiError::BadRequest(format!(
            "Line {} is outside {}.",
            query.line, query.path
        )));
    }
    let sha = side.sha(&report).map(str::to_string);
    let nav = &state.navigator;
    let (line, col) = (query.line as u32, query.col as u32);
    let locations = match action {
        Action::Definition => {
            nav.definitions(sha.as_deref(), &query.path, line, col)
                .await
        }
        Action::References => nav.references(sha.as_deref(), &query.path, line, col).await,
    }
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let environment = nav
        .describe_environment(&query.path)
        .await
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(Json(serde_json::json!({
        "action": action.as_str(),
        "side": side.as_str(),
        "environment": environment,
        "locations": locations,
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct LibraryQuery {
    #[serde(default)]
    pub path: String,
}

/// A library file (installed package or stub) that a navigation result pointed at.
pub async fn get_library(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<Value>, ApiError> {
    let path = query.path;
    let state_for_read = Arc::clone(&state);
    let read_path = path.clone();
    let text = run_blocking(move || state_for_read.navigator.library_source(&read_path))
        .await?
        .map_err(|e| ApiError::NotFound(e.to_string()))?;
    Ok(Json(
        serde_json::json!({"path": path, "lines": refactor_diff_core::split_lines(&text)}),
    ))
}
