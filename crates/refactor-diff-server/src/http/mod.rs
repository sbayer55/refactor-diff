//! The HTTP API: the SPA, its static files and the JSON routes the UI calls.
//!
//! Every route the Python server had, with the same paths, status codes and messages. Errors
//! are always JSON (`{"error": ...}`), including the 404 for unknown paths.

pub mod assets;
pub mod error;
pub mod extract;
pub mod routes;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use crate::app::AppState;
use error::ApiError;

/// The router for one server; the state is shared with every handler. A settings-only
/// server gets just the settings page, its API and the UI preferences.
pub fn router(state: Arc<AppState>) -> Router {
    if state.settings_only {
        return settings_routes(Router::new())
            .route("/", get(routes::settings::redirect_to_settings))
            .route("/api/config", get(routes::settings::settings_only_config))
            .route("/static/{*path}", get(assets::static_file))
            .fallback(not_found)
            .with_state(state);
    }
    settings_routes(Router::new())
        .route("/", get(assets::index))
        .route("/api/config", get(routes::config::config))
        .route("/api/sources", get(routes::config::list_sources))
        .route("/api/analyze", post(routes::analyze::run_analysis))
        .route("/api/report/{report_id}", get(routes::analyze::get_report))
        .route("/api/report/{report_id}/file", get(routes::files::get_file))
        .route(
            "/api/report/{report_id}/review",
            get(routes::review::get_review).post(routes::review::mark_review),
        )
        .route(
            "/api/report/{report_id}/summary.md",
            get(routes::summary::get_summary),
        )
        .route(
            "/api/report/{report_id}/commits",
            get(routes::summary::get_commits),
        )
        .route(
            "/api/report/{report_id}/pr/comment",
            post(routes::pr::post_pr_comment),
        )
        .route(
            "/api/report/{report_id}/pr/review-comment",
            post(routes::pr::post_review_comment),
        )
        .route(
            "/api/report/{report_id}/navigate",
            post(routes::navigate::navigate),
        )
        .route(
            "/api/report/{report_id}/source",
            get(routes::files::get_source),
        )
        .route("/api/library", get(routes::navigate::get_library))
        .route("/api/report/{report_id}/ai/menu", post(routes::ai::ai_menu))
        .route(
            "/api/report/{report_id}/ai/refs-count",
            post(routes::ai::ai_refs_count),
        )
        .route("/api/report/{report_id}/ai/ask", post(routes::ai::ai_ask))
        .route("/static/{*path}", get(assets::static_file))
        .fallback(not_found)
        .with_state(state)
}

/// The settings page, the AI settings API and UI preferences; shared by both kinds of server.
fn settings_routes(router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    router
        .route("/settings", get(routes::settings::settings_page))
        .route(
            "/api/settings",
            get(routes::settings::get_settings).post(routes::settings::save_settings),
        )
        .route("/api/settings/test", post(routes::settings::test_settings))
        .route("/api/prefs", post(routes::settings::save_prefs))
}

async fn not_found() -> ApiError {
    ApiError::NotFound("Not found".into())
}

/// A report's source on one side: `old` is the base, `new` the head (or working tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "old" => Some(Side::Old),
            "new" => Some(Side::New),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }

    /// `original` / `new`, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Side::Old => "original",
            Side::New => "new",
        }
    }

    /// The commit for this side; `None` means the working tree.
    pub fn sha(self, report: &refactor_diff_core::Report) -> Option<&str> {
        match self {
            Side::Old => Some(report.source.base_sha.as_str()),
            Side::New => report.source.head_sha.as_deref(),
        }
    }

    /// The UI names files by their new path; a renamed file's old side lives at `old_path`.
    pub fn path<'a>(self, report: &'a refactor_diff_core::Report, path: &'a str) -> &'a str {
        if self == Side::Old {
            if let Some(old) = report
                .files
                .iter()
                .find(|f| f.path == path)
                .and_then(|f| f.old_path.as_deref())
                .filter(|old| !old.is_empty())
            {
                return old;
            }
        }
        path
    }
}
