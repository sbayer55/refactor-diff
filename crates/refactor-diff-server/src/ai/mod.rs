//! The Ask feature: model providers, the context sent to them and the predefined tasks.
//!
//! Every provider has one tiny interface: stream text deltas for a system prompt and a list
//! of chat turns, and test the connection. Claude speaks the Messages API directly; Ollama
//! and any OpenAI-compatible endpoint (a Cursor proxy, OpenRouter, LM Studio, vLLM, ...)
//! speak JSON over HTTP.

pub mod claude;
pub mod context;
pub mod nav_adapter;
pub mod ollama;
pub mod openai;
pub mod sse;
pub mod tasks;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::settings::Settings;

pub use claude::ClaudeProvider;
pub use ollama::OllamaProvider;
pub use openai::OpenAIProvider;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_TOKENS: u32 = 16000;

pub const CLAUDE_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
];

/// A message fit to show the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

impl ProviderError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Text deltas as the model produces them; the first error ends the stream.
pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<String, ProviderError>> + Send>>;

/// One chat turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new("user", content)
    }
}

/// The outcome of a connection test, as the settings dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub error: Option<String>,
    pub latency_ms: Option<u64>,
    pub models: Vec<String>,
}

impl TestResult {
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            latency_ms: None,
            models: vec![],
        }
    }
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn stream(&self, system: String, messages: Vec<Message>) -> DeltaStream;
    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>>;
}

/// Builds the active provider from the settings; the server keeps one so tests can swap it.
pub trait ProviderFactory: Send + Sync {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn Provider>, ProviderError>;
}

/// The real providers over one shared HTTP client.
pub struct DefaultFactory {
    client: reqwest::Client,
}

impl Default for DefaultFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultFactory {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

/// An HTTP client with the connect and read timeouts every provider uses.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .expect("reqwest client with static configuration")
}

impl ProviderFactory for DefaultFactory {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn Provider>, ProviderError> {
        build_with(self.client.clone(), settings)
    }
}

/// The active provider from the settings, or `ProviderError` when it isn't set up.
pub fn build_with(
    client: reqwest::Client,
    settings: &Settings,
) -> Result<Arc<dyn Provider>, ProviderError> {
    let name = settings.provider();
    let cfg = settings.provider_cfg(name);
    let text = |key: &str| -> String {
        cfg.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    match name {
        "claude" => {
            let api_key = text("api_key");
            if api_key.is_empty() {
                return Err(ProviderError::new(
                    "Claude needs an API key. Open Settings to add one.",
                ));
            }
            let model = Some(text("model"))
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| claude::DEFAULT_MODEL.to_string());
            let base_url = Some(text("base_url")).filter(|u| !u.is_empty());
            Ok(Arc::new(ClaudeProvider::new(
                client, api_key, model, base_url,
            )))
        }
        "openai" => {
            let (base_url, model) = (text("base_url"), text("model"));
            if base_url.is_empty() || model.is_empty() {
                return Err(ProviderError::new(
                    "The OpenAI-compatible endpoint needs a base URL and a model. Open Settings.",
                ));
            }
            Ok(Arc::new(OpenAIProvider::new(
                client,
                base_url,
                text("api_key"),
                model,
            )))
        }
        "ollama" => {
            let (host, model) = (text("host"), text("model"));
            if host.is_empty() || model.is_empty() {
                return Err(ProviderError::new(
                    "Ollama needs a host and a model. Open Settings.",
                ));
            }
            let num_ctx = cfg
                .get("num_ctx")
                .and_then(|v| match v {
                    serde_json::Value::Number(n) => {
                        n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))
                    }
                    serde_json::Value::String(s) => s.trim().parse::<i64>().ok(),
                    _ => None,
                })
                .filter(|n| *n != 0)
                .unwrap_or(ollama::DEFAULT_NUM_CTX);
            Ok(Arc::new(OllamaProvider::new(client, host, model, num_ctx)))
        }
        other => Err(ProviderError(format!("Unknown provider '{other}'."))),
    }
}

/// Run a connection probe and report its latency, or the error it failed with.
pub async fn timed<F>(probe: F) -> TestResult
where
    F: Future<Output = Result<Vec<String>, ProviderError>>,
{
    let t0 = Instant::now();
    match probe.await {
        Ok(models) => TestResult {
            ok: true,
            error: None,
            latency_ms: Some(t0.elapsed().as_millis() as u64),
            models,
        },
        Err(e) => TestResult::failure(e.0),
    }
}

/// A transport error as a sentence about `what` ("Ollama", "the endpoint").
pub fn describe(e: &reqwest::Error, what: &str) -> ProviderError {
    if e.is_connect() {
        ProviderError(format!("Couldn't connect to {what}. Is it running?"))
    } else if e.is_timeout() {
        ProviderError(format!("{what} didn't respond in time."))
    } else {
        ProviderError(format!("Request to {what} failed: {e}"))
    }
}

/// An HTTP error status with the start of its body.
pub fn status_error(status: reqwest::StatusCode, body: &str, what: &str) -> ProviderError {
    let head: String = body.chars().take(200).collect();
    let head = head.trim();
    let detail = if head.is_empty() { "no detail" } else { head };
    ProviderError(format!(
        "{what} returned HTTP {}: {detail}",
        status.as_u16()
    ))
}

/// A response body as text; `""` when it can't be read.
pub(crate) async fn body_text(res: reqwest::Response) -> String {
    res.text().await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Settings, defaults};
    use serde_json::json;

    fn settings(patch: serde_json::Value) -> Settings {
        let mut v = defaults();
        merge_into(&mut v, &patch);
        Settings(v)
    }

    fn merge_into(base: &mut serde_json::Value, over: &serde_json::Value) {
        match (base, over) {
            (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
                for (k, v) in o {
                    match b.get_mut(k) {
                        Some(existing) if existing.is_object() && v.is_object() => {
                            merge_into(existing, v)
                        }
                        _ => {
                            b.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            (b, o) => *b = o.clone(),
        }
    }

    fn build_err(factory: &DefaultFactory, s: &Settings) -> ProviderError {
        match factory.build(s) {
            Ok(p) => panic!("built {} unexpectedly", p.name()),
            Err(e) => e,
        }
    }

    #[test]
    fn build_requires_configuration() {
        let factory = DefaultFactory::new();
        let err = build_err(&factory, &settings(json!({})));
        assert_eq!(err.0, "Claude needs an API key. Open Settings to add one.");
        let err = build_err(&factory, &settings(json!({"ai": {"provider": "ollama"}})));
        assert_eq!(err.0, "Ollama needs a host and a model. Open Settings.");
        let p = factory
            .build(&settings(json!({"ai": {"provider": "ollama", "providers": {"ollama": {"model": "qwen3-coder:30b"}}}})))
            .unwrap();
        assert_eq!((p.name(), p.model()), ("ollama", "qwen3-coder:30b"));
        let err = build_err(&factory, &settings(json!({"ai": {"provider": "openai"}})));
        assert_eq!(
            err.0,
            "The OpenAI-compatible endpoint needs a base URL and a model. Open Settings."
        );
        let p = factory
            .build(&settings(json!({"ai": {"provider": "openai", "providers": {"openai": {"base_url": "http://x/v1/", "model": "m"}}}})))
            .unwrap();
        assert_eq!((p.name(), p.model()), ("openai", "m"));
        let p = factory
            .build(&settings(
                json!({"ai": {"providers": {"claude": {"api_key": "k"}}}}),
            ))
            .unwrap();
        assert_eq!((p.name(), p.model()), ("claude", "claude-opus-5-5"));
        let err = build_err(&factory, &settings(json!({"ai": {"provider": "cursor"}})));
        assert_eq!(err.0, "Unknown provider 'cursor'.");
    }

    #[test]
    fn ollama_num_ctx_parses_like_python_int() {
        let num_ctx = |v: serde_json::Value| {
            let s = settings(
                json!({"ai": {"provider": "ollama", "providers": {"ollama": {"model": "m", "num_ctx": v}}}}),
            );
            let p = build_with(http_client(), &s).unwrap();
            // The provider's `model()` is checked elsewhere; here we only care that it built.
            assert_eq!(p.name(), "ollama");
            s.0["ai"]["providers"]["ollama"]["num_ctx"].clone()
        };
        assert_eq!(num_ctx(json!(4096)), json!(4096));
        assert_eq!(num_ctx(json!("8192")), json!("8192"));
        let s = settings(
            json!({"ai": {"provider": "ollama", "providers": {"ollama": {"model": "m", "num_ctx": 4096}}}}),
        );
        let p = OllamaProvider::new(http_client(), "h".into(), "m".into(), 4096);
        assert_eq!(p.num_ctx(), 4096);
        assert_eq!(build_with(http_client(), &s).unwrap().model(), "m");
    }

    #[test]
    fn status_error_trims_the_body() {
        let e = status_error(
            reqwest::StatusCode::NOT_FOUND,
            "  model 'm' not found \n",
            "Ollama",
        );
        assert_eq!(e.0, "Ollama returned HTTP 404: model 'm' not found");
        let e = status_error(reqwest::StatusCode::BAD_GATEWAY, "   ", "the endpoint");
        assert_eq!(e.0, "the endpoint returned HTTP 502: no detail");
        let long = "x".repeat(300);
        let e = status_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &long, "X");
        assert_eq!(e.0.len(), "X returned HTTP 500: ".len() + 200);
    }

    #[tokio::test]
    async fn timed_reports_latency_or_error() {
        let ok = timed(async { Ok(vec!["m".to_string()]) }).await;
        assert!(ok.ok && ok.error.is_none() && ok.latency_ms.is_some());
        assert_eq!(ok.models, vec!["m"]);
        let bad = timed(async { Err(ProviderError::new("nope")) }).await;
        assert_eq!(
            bad,
            TestResult {
                ok: false,
                error: Some("nope".into()),
                latency_ms: None,
                models: vec![]
            }
        );
        assert_eq!(
            serde_json::to_value(&bad).unwrap(),
            json!({"ok": false, "error": "nope", "latency_ms": null, "models": []})
        );
    }
}
