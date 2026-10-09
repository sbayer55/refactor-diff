//! `/api/settings` (read, update) and `/api/settings/test` (try a provider with unsaved
//! values from the dialog).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, str_or_empty};
use crate::settings::{apply_update, public_view, resolve_test_config};

/// The settings with secrets masked.
pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(public_view(&state.settings.load()))
}

/// Merge `{"ai": {...}}` over the stored settings (masked secrets keep their stored value).
pub async fn save_settings(
    State(state): State<Arc<AppState>>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let merged = apply_update(&state.settings.load(), &body)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let saved = run_blocking(move || state.settings.save(merged.as_value())).await??;
    Ok(Json(public_view(&saved)))
}

#[derive(Debug, Default, Deserialize)]
pub struct TestBody {
    #[serde(default)]
    pub provider: Value,
    #[serde(default)]
    pub config: Option<Value>,
}

/// Try the provider with `{"provider", "config"}` from the dialog (unsaved values). Always
/// 200: `{"ok", "error", "latency_ms", "models"}`.
pub async fn test_settings(
    State(state): State<Arc<AppState>>,
    ApiJson(body): ApiJson<TestBody>,
) -> Json<Value> {
    let failed = |error: String| {
        Json(json!({"ok": false, "error": error, "latency_ms": null, "models": []}))
    };
    let resolved = match resolve_test_config(
        &state.settings.load(),
        str_or_empty(&body.provider),
        body.config.as_ref().filter(|c| !c.is_null()),
    ) {
        Ok(settings) => settings,
        Err(e) => return failed(e.to_string()),
    };
    let provider = match state.providers.build(&resolved) {
        Ok(provider) => provider,
        Err(e) => return failed(e.0),
    };
    let result = provider.test().await;
    Json(serde_json::to_value(result).expect("a test result serializes"))
}
