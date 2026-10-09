//! Any OpenAI-compatible `/chat/completions` endpoint: SSE `data:` payloads until `[DONE]`.

use std::future::Future;
use std::pin::Pin;

use futures::StreamExt;
use serde_json::{Value, json};

use super::ollama::plain;
use super::{
    DeltaStream, Message, Provider, ProviderError, TestResult, describe, sse, status_error, timed,
};

const WHAT: &str = "the endpoint";

#[derive(Clone, Debug)]
pub struct OpenAIProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAIProvider {
    pub fn new(client: reqwest::Client, base_url: String, api_key: String, model: String) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            req
        } else {
            req.header("Authorization", format!("Bearer {}", self.api_key))
        }
    }

    async fn probe(&self) -> Result<Vec<String>, ProviderError> {
        let res = self
            .request(self.client.get(format!("{}/models", self.base_url)))
            .send()
            .await
            .map_err(|e| describe(&e, WHAT))?;
        if res.status() == reqwest::StatusCode::OK {
            let data: Value = res.json().await.map_err(|e| describe(&e, WHAT))?;
            let models = data["data"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| m.is_object())
                        .map(|m| m["id"].as_str().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            return Ok(models);
        }
        let body = json!({
            "model": self.model,
            "max_tokens": 8,
            "messages": [{"role": "user", "content": "Reply with the single word ok."}],
        });
        let res = self
            .request(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| describe(&e, WHAT))?;
        let status = res.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(status_error(status, &super::body_text(res).await, WHAT));
        }
        Ok(vec![])
    }
}

impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, system: String, messages: Vec<Message>) -> DeltaStream {
        let mut turns = vec![Message::new("system", system)];
        turns.extend(messages);
        let body = json!({
            "model": self.model,
            "messages": turns,
            "stream": true,
        });
        let req = self
            .request(
                self.client
                    .post(format!("{}/chat/completions", self.base_url)),
            )
            .json(&body);
        Box::pin(async_stream::stream! {
            let res = match req.send().await {
                Ok(res) => res,
                Err(e) => {
                    yield Err(describe(&e, WHAT));
                    return;
                }
            };
            let status = res.status();
            if status.is_client_error() || status.is_server_error() {
                yield Err(status_error(status, &super::body_text(res).await, WHAT));
                return;
            }
            let mut lines = std::pin::pin!(sse::lines(res));
            while let Some(line) = lines.next().await {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        yield Err(describe(&e, WHAT));
                        return;
                    }
                };
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    break;
                }
                let chunk: Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(e) => {
                        yield Err(ProviderError(format!("Request to {WHAT} failed: {e}")));
                        return;
                    }
                };
                if crate::settings::truthy(&chunk["error"]) {
                    yield Err(ProviderError(format!("Endpoint error: {}", plain(&chunk["error"]))));
                    return;
                }
                let text = chunk["choices"]
                    .as_array()
                    .and_then(|c| c.first())
                    .and_then(|c| c["delta"]["content"].as_str())
                    .unwrap_or("");
                if !text.is_empty() {
                    yield Ok(text.to_string());
                }
            }
        })
    }

    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>> {
        Box::pin(timed(self.probe()))
    }
}
