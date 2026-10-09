//! Claude through the Messages API (`POST /v1/messages`), streamed as Server-Sent Events.
//!
//! The request carries `output_config: {"effort": "medium"}` so answers stay short; thinking
//! is left at the model's default (adaptive on current models). Like the official SDK with
//! `max_retries=1`, a request is retried once on connection errors and on 408/409/429/5xx,
//! but only before any of the stream has been read.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};

use super::{
    CLAUDE_MODELS, DeltaStream, MAX_TOKENS, Message, Provider, ProviderError, TestResult, sse,
    timed,
};

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct ClaudeProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl ClaudeProvider {
    pub fn new(
        client: reqwest::Client,
        api_key: String,
        model: String,
        base_url: Option<String>,
    ) -> Self {
        Self {
            client,
            api_key,
            model,
            base_url: base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        self.client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .json(body)
    }

    /// Send `body`, retrying once like the SDK; a non-success status becomes the user-facing
    /// error for it.
    async fn send(&self, body: &Value) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let res = match self.request(body).send().await {
                Ok(res) => res,
                Err(e) => {
                    if attempt == 1 && (e.is_connect() || e.is_timeout() || e.is_request()) {
                        tokio::time::sleep(RETRY_BACKOFF).await;
                        continue;
                    }
                    return Err(self.transport_error(&e));
                }
            };
            let status = res.status();
            if status.is_success() {
                return Ok(res);
            }
            if attempt == 1 && retryable(status) {
                tokio::time::sleep(RETRY_BACKOFF).await;
                continue;
            }
            let body = super::body_text(res).await;
            return Err(self.status_error(status, &body));
        }
    }

    fn transport_error(&self, e: &reqwest::Error) -> ProviderError {
        if e.is_connect() || e.is_timeout() || e.is_request() {
            ProviderError::new("Couldn't reach the Claude API. Check your network or base URL.")
        } else {
            ProviderError(format!("Claude request failed: {e}"))
        }
    }

    fn status_error(&self, status: reqwest::StatusCode, body: &str) -> ProviderError {
        match status.as_u16() {
            401 => ProviderError::new("Claude rejected the API key. Check it in Settings."),
            403 => ProviderError::new("The Claude API key doesn't have access to this model."),
            404 => ProviderError(format!(
                "Claude doesn't know the model '{}'. Check it in Settings.",
                self.model
            )),
            429 => ProviderError::new("Claude is rate limiting requests; try again in a moment."),
            code => {
                let message = serde_json::from_str::<Value>(body)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                    .unwrap_or_else(|| body.trim().to_string());
                ProviderError(format!("Claude returned an error ({code}): {message}"))
            }
        }
    }

    async fn probe(&self) -> Result<Vec<String>, ProviderError> {
        let body = json!({
            "model": self.model,
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "Reply with the single word ok."}],
        });
        let res = self.send(&body).await?;
        // Drain the body so the connection can be reused; its content doesn't matter.
        let _ = res.bytes().await;
        Ok(CLAUDE_MODELS.iter().map(|m| m.to_string()).collect())
    }
}

fn retryable(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error()
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &str {
        "claude"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, system: String, messages: Vec<Message>) -> DeltaStream {
        let this = self.clone();
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": messages,
            "stream": true,
            "output_config": {"effort": "medium"},
        });
        Box::pin(async_stream::stream! {
            let res = match this.send(&body).await {
                Ok(res) => res,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            let mut events = std::pin::pin!(sse::events(res));
            let mut stop_reason: Option<String> = None;
            while let Some(event) = events.next().await {
                let event = match event {
                    Ok(ev) => ev,
                    Err(eventsource_stream::EventStreamError::Transport(e)) => {
                        yield Err(this.transport_error(&e));
                        return;
                    }
                    Err(e) => {
                        yield Err(ProviderError(format!("Claude request failed: {e}")));
                        return;
                    }
                };
                let data: Value = match serde_json::from_str(&event.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match data["type"].as_str().unwrap_or("") {
                    "content_block_delta" => {
                        if data["delta"]["type"] == "text_delta" {
                            if let Some(text) = data["delta"]["text"].as_str().filter(|t| !t.is_empty()) {
                                yield Ok(text.to_string());
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                            stop_reason = Some(reason.to_string());
                        }
                    }
                    "error" => {
                        let message = data["error"]["message"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| event.data.clone());
                        yield Err(ProviderError(format!("Claude request failed: {message}")));
                        return;
                    }
                    "message_stop" => break,
                    _ => {} // message_start, content_block_start/stop, ping
                }
            }
            match stop_reason.as_deref() {
                Some("refusal") => {
                    yield Err(ProviderError::new("Claude declined to answer this request."));
                }
                Some("max_tokens") => {
                    yield Ok("\n\n*(answer cut off: the response hit the length limit)*".to_string());
                }
                _ => {}
            }
        })
    }

    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>> {
        Box::pin(timed(self.probe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_defaults_and_strips_the_slash() {
        let p = ClaudeProvider::new(super::super::http_client(), "k".into(), "m".into(), None);
        assert_eq!(p.base_url(), DEFAULT_BASE_URL);
        let p = ClaudeProvider::new(
            super::super::http_client(),
            "k".into(),
            "m".into(),
            Some("http://proxy/".into()),
        );
        assert_eq!(p.base_url(), "http://proxy");
    }

    #[test]
    fn status_messages() {
        let p = ClaudeProvider::new(super::super::http_client(), "k".into(), "m".into(), None);
        let msg = |code: u16, body: &str| {
            p.status_error(reqwest::StatusCode::from_u16(code).unwrap(), body)
                .0
        };
        assert_eq!(
            msg(401, ""),
            "Claude rejected the API key. Check it in Settings."
        );
        assert_eq!(
            msg(403, ""),
            "The Claude API key doesn't have access to this model."
        );
        assert_eq!(
            msg(404, ""),
            "Claude doesn't know the model 'm'. Check it in Settings."
        );
        assert_eq!(
            msg(429, ""),
            "Claude is rate limiting requests; try again in a moment."
        );
        assert_eq!(
            msg(
                500,
                r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#
            ),
            "Claude returned an error (500): boom"
        );
        assert_eq!(
            msg(502, " bad gateway "),
            "Claude returned an error (502): bad gateway"
        );
    }
}
