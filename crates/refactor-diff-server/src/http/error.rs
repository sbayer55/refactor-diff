//! API errors: every failure is a JSON `{"error": message}` with the status the UI expects.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::git::SourceError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 400.
    BadRequest(String),
    /// 404.
    NotFound(String),
    /// 400 with `"settings": true`: the UI opens the settings dialog.
    NeedsSettings(String),
    /// 500.
    Internal(String),
}

impl ApiError {
    pub fn unknown_report() -> Self {
        ApiError::NotFound("Unknown report; run the analysis again.".into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) | ApiError::NeedsSettings(_) => StatusCode::BAD_REQUEST,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ApiError::BadRequest(m)
            | ApiError::NotFound(m)
            | ApiError::NeedsSettings(m)
            | ApiError::Internal(m) => m,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": self.message()});
        if matches!(self, ApiError::NeedsSettings(_)) {
            body["settings"] = json!(true);
        }
        if let ApiError::Internal(message) = &self {
            tracing::error!(%message, "request failed");
        }
        (self.status(), Json(body)).into_response()
    }
}

/// A git/gh failure is the user's problem to fix (bad ref, no gh): 400 with its message.
impl From<SourceError> for ApiError {
    fn from(e: SourceError) -> Self {
        ApiError::BadRequest(e.to_string())
    }
}

/// Disk failures writing state are ours: 500.
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}
