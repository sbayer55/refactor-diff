//! One file per area of the API; see [`super::router`] for the paths.

pub mod ai;
pub mod analyze;
pub mod config;
pub mod files;
pub mod navigate;
pub mod pr;
pub mod review;
pub mod settings;
pub mod summary;

use refactor_diff_core::Report;
use serde_json::Value;

use crate::app::AppState;
use crate::review::{Fingerprints, Review};

/// Hunk fingerprints of a report, in hunk order, with each hunk's path.
pub fn hunks_of(report: &Report) -> Fingerprints {
    report
        .hunks
        .values()
        .map(|h| (h.fingerprint.clone(), h.path.clone()))
        .collect()
}

/// The report's review state (marks and the delta against the previous head).
pub fn review_of(state: &AppState, report: &Report) -> Review {
    state
        .reviews
        .review(&report.source.identity, &hunks_of(report))
}

/// The report's JSON plus its `review`, as `/api/analyze` and `/api/report/{id}` return it.
pub fn with_review(state: &AppState, report: &Report) -> Value {
    let mut value = serde_json::to_value(report).expect("report serializes");
    value["review"] = serde_json::to_value(review_of(state, report)).expect("review serializes");
    value
}
