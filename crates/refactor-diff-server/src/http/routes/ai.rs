//! The Ask menu: `/api/report/{id}/ai/menu`, `/ai/refs-count` and `/ai/ask` (an SSE stream).

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::header;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::ai::context::{ContextBuilder, ContextError, Focus, Function, render};
use crate::ai::nav_adapter::NavigatorSource;
use crate::ai::tasks::{self, Piece, Task};
use crate::ai::{Message, Provider};
use crate::app::{AppState, run_blocking};
use crate::http::error::ApiError;
use crate::http::extract::{ApiJson, int_or_empty, str_or_empty, truthy};
use crate::settings::public_view;

/// `{"hunk_id", "side", "line"}`: the location the menu was opened at.
#[derive(Debug, Default, Deserialize)]
pub struct FocusBody {
    #[serde(default)]
    pub hunk_id: Value,
    #[serde(default)]
    pub side: Value,
    #[serde(default)]
    pub line: Value,
}

/// A validated focus request (the hunk and side are checked by the context builder).
#[derive(Debug, PartialEq, Eq)]
pub struct FocusRequest {
    pub hunk_id: String,
    pub side: Option<String>,
    pub line: Option<i64>,
}

fn focus_of(body: &FocusBody) -> Result<FocusRequest, ApiError> {
    let line = int_or_empty(&body.line)
        .map_err(|_| ApiError::BadRequest("line must be an integer.".into()))?;
    Ok(FocusRequest {
        hunk_id: str_or_empty(&body.hunk_id).to_string(),
        side: body.side.as_str().map(str::to_string),
        line,
    })
}

impl From<ContextError> for ApiError {
    fn from(e: ContextError) -> Self {
        ApiError::BadRequest(e.0)
    }
}

/// A context builder for a report, with the server's navigator, and the validated focus.
fn locate(
    state: &Arc<AppState>,
    report_id: &str,
    body: &FocusBody,
) -> Result<(ContextBuilder, Focus), ApiError> {
    let report = state.report(report_id)?;
    let request = focus_of(body)?;
    let builder = ContextBuilder::new(
        report,
        state.git.clone(),
        Some(NavigatorSource::source(Arc::clone(state))),
    );
    let focus = builder.focus(&request.hunk_id, request.side.as_deref(), request.line)?;
    Ok((builder, focus))
}

/// The enclosing def/class, computed off the async threads (it parses the file).
async fn function_piece(
    builder: &ContextBuilder,
    focus: &Focus,
) -> Result<Option<Function>, ApiError> {
    let (b, f) = (builder.clone(), focus.clone());
    run_blocking(move || b.function_piece(&f)).await
}

/// What the Ask menu shows for a location: the tasks with hints and availability.
pub async fn ai_menu(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<FocusBody>,
) -> Result<Json<Value>, ApiError> {
    let (builder, focus) = locate(&state, &report_id, &body)?;
    let (rows, func) = {
        let (b, f) = (builder.clone(), focus.clone());
        run_blocking(move || {
            let func = b.function_piece(&f);
            (tasks::menu_with(&b, &f, func.as_ref()), func)
        })
        .await?
    };
    let settings = state.settings.load();
    let active = public_view(&settings)["ai"]["active"].clone();
    Ok(Json(json!({
        "focus": {
            "path": focus.path,
            "side": focus.side,
            "line": focus.line,
            "qualname": func.map(|f| f.qualname),
        },
        "tasks": rows,
        "provider": active,
        "context": settings.as_value()["ai"]["context"],
    })))
}

/// How many references the enclosing def has (slow: runs code navigation).
pub async fn ai_refs_count(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<FocusBody>,
) -> Result<Json<Value>, ApiError> {
    let (builder, focus) = locate(&state, &report_id, &body)?;
    let func = function_piece(&builder, &focus).await?;
    let count = match builder.references_piece(&focus, func.as_ref()).await {
        Some(Ok(refs)) => Some(refs.total),
        _ => None,
    };
    Ok(Json(json!({"count": count})))
}

#[derive(Debug, Default, Deserialize)]
pub struct AskBody {
    #[serde(default)]
    pub task: Value,
    #[serde(flatten)]
    pub focus: FocusBody,
    #[serde(default)]
    pub prompt: Value,
    #[serde(default)]
    pub pieces: Value,
    #[serde(default)]
    pub history: Value,
}

/// Run a task (or a custom prompt) for a location and stream the answer as SSE.
pub async fn ai_ask(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<AskBody>,
) -> Result<Response, ApiError> {
    state.report(&report_id)?;
    let Some(task) = tasks::by_id(str_or_empty(&body.task)) else {
        return Err(ApiError::BadRequest("Unknown task.".into()));
    };
    let (builder, focus) = locate(&state, &report_id, &body.focus)?;
    let mut pieces: BTreeSet<Piece> = task.pieces();
    let mut prompt = String::new();
    if task.is_custom() {
        prompt = str_or_empty(&body.prompt).trim().to_string();
        if prompt.is_empty() {
            return Err(ApiError::BadRequest("The prompt is empty.".into()));
        }
        if let Some(want) = body.pieces.as_object() {
            pieces.extend(
                [Piece::Function, Piece::References, Piece::Pr]
                    .into_iter()
                    .filter(|p| want.get(p.as_str()).is_some_and(truthy)),
            );
        }
    }
    let history = if truthy(&body.history) {
        body.history.clone()
    } else {
        Value::Array(vec![])
    };
    if !valid_history(&history) {
        return Err(ApiError::BadRequest("Malformed follow-up history.".into()));
    }
    let history: Vec<Message> = serde_json::from_value(history)
        .map_err(|_| ApiError::BadRequest("Malformed follow-up history.".into()))?;
    let func = function_piece(&builder, &focus).await?;
    let why = {
        let (b, f) = (builder.clone(), focus.clone());
        run_blocking(move || task.unavailable(&b, &f, func.as_ref())).await?
    };
    if let Some(why) = why {
        return Err(ApiError::BadRequest(format!(
            "{} isn't available here: {why}.",
            task.label
        )));
    }
    let provider = state
        .providers
        .build(&state.settings.load())
        .map_err(|e| ApiError::NeedsSettings(e.0))?;
    let stream = answer(
        Arc::clone(&state),
        builder,
        focus,
        task,
        pieces,
        prompt,
        history,
        provider,
    );
    Ok((
        [
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Sse::new(stream),
    )
        .into_response())
}

/// The events of one answer: `meta`, then `delta`s, then `done` or `error`. Stops early when
/// the server shuts down.
#[allow(clippy::too_many_arguments)]
fn answer(
    state: Arc<AppState>,
    builder: ContextBuilder,
    focus: Focus,
    task: &'static Task,
    pieces: BTreeSet<Piece>,
    prompt: String,
    history: Vec<Message>,
    provider: Arc<dyn Provider>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> + Send + 'static {
    async_stream::stream! {
        let t0 = Instant::now();
        let shutdown = state.shutdown.clone();
        let ctx = tokio::select! {
            _ = shutdown.cancelled() => return,
            ctx = builder.collect(&focus, &pieces) => ctx,
        };
        let mut user = render(&ctx);
        if !prompt.is_empty() {
            user.push_str(&format!("\n\n## Question\n{prompt}"));
        }
        yield Ok(event("meta", &json!({
            "provider": provider.name(),
            "model": provider.model(),
            "task": task.id,
            "focus": {"path": focus.path, "side": focus.side, "line": focus.line},
            "pieces": ctx.sent_pieces(&pieces),
        })));
        let mut messages = vec![Message::user(user)];
        messages.extend(history);
        let mut deltas = provider.stream(tasks::system_prompt(task), messages);
        loop {
            let next = tokio::select! {
                _ = shutdown.cancelled() => return,
                next = deltas.next() => next,
            };
            match next {
                Some(Ok(text)) => yield Ok(event("delta", &json!({"text": text}))),
                Some(Err(e)) => {
                    yield Ok(event("error", &json!({"message": e.0})));
                    return;
                }
                None => break,
            }
        }
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        yield Ok(event("done", &json!({"elapsed_ms": elapsed_ms})));
    }
}

/// One server-sent event, as the UI parses it: `event: <name>` plus one JSON `data:` line.
fn event(name: &str, data: &Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

/// One server-sent event as text (what [`event`] serializes to).
pub fn sse(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

/// Follow-up turns: alternating assistant/user, starting with assistant, ending with user.
pub fn valid_history(history: &Value) -> bool {
    let Some(turns) = history.as_array() else {
        return false;
    };
    for (i, turn) in turns.iter().enumerate() {
        let Some(turn) = turn.as_object() else {
            return false;
        };
        if !turn.get("content").is_some_and(Value::is_string) {
            return false;
        }
        let expected = if i % 2 == 0 { "assistant" } else { "user" };
        if turn.get("role").and_then(Value::as_str) != Some(expected) {
            return false;
        }
    }
    turns.len() % 2 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_validation_matches_python() {
        assert!(valid_history(&json!([])));
        assert!(valid_history(&json!([
            {"role": "assistant", "content": "a"},
            {"role": "user", "content": "b"},
        ])));
        assert!(!valid_history(
            &json!([{"role": "assistant", "content": "a"}])
        ));
        assert!(!valid_history(&json!([
            {"role": "user", "content": "a"},
            {"role": "assistant", "content": "b"},
        ])));
        assert!(!valid_history(
            &json!([{"role": "assistant"}, {"role": "user", "content": "b"}])
        ));
        assert!(!valid_history(&json!("nope")));
        assert!(!valid_history(&json!([1, 2])));
    }

    #[test]
    fn sse_format() {
        assert_eq!(
            sse("delta", &json!({"text": "hi"})),
            "event: delta\ndata: {\"text\":\"hi\"}\n\n"
        );
    }
}
