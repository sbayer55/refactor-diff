//! `/api/report/{id}/review`: reviewed marks on groups and hunks.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::Value;

use super::review_of;
use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;
use crate::http::extract::ApiJson;
use crate::review::{MarkChange, Review};

pub async fn get_review(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
) -> Result<Json<Review>, ApiError> {
    let report = state.report(&report_id)?;
    Ok(Json(review_of(&state, &report)))
}

/// `{"groups": {"add": [], "remove": []}, "hunks": {...}}`; either field may be omitted.
#[derive(Debug, Default, Deserialize)]
pub struct MarkBody {
    #[serde(default)]
    pub groups: Value,
    #[serde(default)]
    pub hunks: Value,
}

/// Add/remove reviewed marks and return the new review state.
pub async fn mark_review(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<MarkBody>,
) -> Result<Json<Review>, ApiError> {
    let report = state.report(&report_id)?;
    let groups = parse_change("groups", &body.groups)?;
    let hunks = parse_change("hunks", &body.hunks)?;
    let identity = report.source.identity.clone();
    let st = state.clone();
    run_blocking(move || st.reviews.mark(&identity, groups.as_ref(), hunks.as_ref())).await??;
    Ok(Json(review_of(&state, &report)))
}

/// `None` when absent; otherwise an object whose `add`/`remove` (if present) are lists.
fn parse_change(field: &str, value: &Value) -> Result<Option<MarkChange>, ApiError> {
    if value.is_null() {
        return Ok(None);
    }
    let error = || ApiError::BadRequest(format!("{field} must be {{add: [], remove: []}}."));
    let obj = value.as_object().ok_or_else(error)?;
    for key in ["add", "remove"] {
        if obj.get(key).is_some_and(|v| !v.is_array()) {
            return Err(error());
        }
    }
    serde_json::from_value::<MarkChange>(value.clone())
        .map(Some)
        .map_err(|_| error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn change_validation_matches_python() {
        assert_eq!(parse_change("hunks", &json!(null)).unwrap(), None);
        assert_eq!(
            parse_change("hunks", &json!({"add": ["a"]})).unwrap(),
            Some(MarkChange {
                add: vec!["a".into()],
                remove: vec![],
            })
        );
        assert_eq!(
            parse_change("hunks", &json!({})).unwrap(),
            Some(MarkChange::default())
        );
        let err = parse_change("hunks", &json!(["x"])).unwrap_err();
        assert_eq!(
            err,
            ApiError::BadRequest("hunks must be {add: [], remove: []}.".into())
        );
        assert!(parse_change("groups", &json!({"add": "x"})).is_err());
    }
}
