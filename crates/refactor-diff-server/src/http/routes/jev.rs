//! `POST /api/report/{id}/jev/score` and `GET /api/report/{id}/jev`: risk scores for the
//! needs-review hunks, computed by Jev through Vercel AI Gateway. Separate from the report
//! JSON (pinned by goldens), from the refactor warnings and from code issues.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use crate::ai::jev::job;
use crate::ai::jev::questions::QUESTION_SET_VERSION;
use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, truthy};

/// Start scoring the report's needs-review hunks (`{"force": true}` ignores the cache and
/// restarts a running job). 400 with `settings: true` when Jev has no API key.
pub async fn score(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let force = truthy(&body["force"]);
    let job = job::start(state.clone(), report, force)?;
    Ok(Json(decorate(&state, job.snapshot())))
}

/// The job's status and the scores so far; with no job, the cached scores for the report's
/// needs-review hunks.
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let job = state
        .jev
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&report.id)
        .cloned();
    let snapshot = match job {
        Some(job) => job.snapshot(),
        None => {
            let st = state.clone();
            let cached = run_blocking(move || st.jev_store.load()).await?;
            let hunks: serde_json::Map<String, Value> = report
                .residual_hunk_ids
                .iter()
                .filter_map(|id| {
                    let hunk = report.hunks.get(id)?;
                    let mut r = cached.get(&hunk.fingerprint)?.clone();
                    r.hunk_id = id.clone();
                    r.path = hunk.path.clone();
                    Some((id.clone(), serde_json::to_value(r).ok()?))
                })
                .collect();
            json!({"status": {"state": "idle"}, "hunks": hunks, "failures": []})
        }
    };
    Ok(Json(decorate(&state, snapshot)))
}

fn decorate(state: &AppState, mut snapshot: Value) -> Value {
    let jev = state.settings.load().jev();
    snapshot["configured"] = Value::Bool(jev.configured());
    snapshot["auto"] = Value::Bool(jev.auto);
    snapshot["question_set"] = Value::String(QUESTION_SET_VERSION.into());
    snapshot
}
