//! `/api/config` and `/api/sources`: what the UI shows before an analysis.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::git::SourcesInfo;
use crate::http::error::ApiError;

/// The repository, the defaults the CLI flags pre-select, whether the UI is inside the
/// desktop app, and the stored UI preferences.
pub async fn config(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "repo": state.repo.to_string_lossy(),
        "defaults": state.defaults,
        "desktop": state.desktop,
        "prefs": state.prefs.load(),
    }))
}

/// Branches, the current one, a default base and the open PRs (when `gh` works).
pub async fn list_sources(
    State(state): State<Arc<AppState>>,
) -> Result<Json<SourcesInfo>, ApiError> {
    let info = run_blocking(move || state.git.list_sources(&*state.github)).await??;
    Ok(Json(info))
}
