//! Ollama's native `/api/chat` endpoint: NDJSON chunks until one says `done`.

use std::future::Future;
use std::pin::Pin;

use futures::StreamExt;
use serde_json::{Value, json};

use super::{
    DeltaStream, Message, Provider, ProviderError, TestResult, describe, sse, status_error, timed,
};

pub const DEFAULT_NUM_CTX: i64 = 32768;
const WHAT: &str = "Ollama";

#[derive(Clone, Debug)]
pub struct OllamaProvider {
    client: reqwest::Client,
    host: String,
    model: String,
    num_ctx: i64,
}

impl OllamaProvider {
    pub fn new(client: reqwest::Client, host: String, model: String, num_ctx: i64) -> Self {
        Self {
            client,
            host: host.trim_end_matches('/').to_string(),
            model,
            num_ctx,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn num_ctx(&self) -> i64 {
        self.num_ctx
    }

    async fn probe(&self) -> Result<Vec<String>, ProviderError> {
        let res = self
            .client
            .get(format!("{}/api/tags", self.host))
            .send()
            .await
            .map_err(|e| describe(&e, WHAT))?;
        let status = res.status();
        if status.is_client_error() || status.is_server_error() {
            return Err(status_error(status, &super::body_text(res).await, WHAT));
        }
        let data: Value = res.json().await.map_err(|e| describe(&e, WHAT))?;
        let models: Vec<String> = data["models"]
            .as_array()
            .map(|ms| {
                ms.iter()
                    .map(|m| m["name"].as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !self.model.is_empty() && !models.contains(&self.model) {
            return Err(ProviderError(format!(
                "Ollama is running but has no model '{m}'. Pull it with `ollama pull {m}`.",
                m = self.model
            )));
        }
        Ok(models)
    }
}

impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self, system: String, messages: Vec<Message>) -> DeltaStream {
        let client = self.client.clone();
        let url = format!("{}/api/chat", self.host);
        let mut turns = vec![Message::new("system", system)];
        turns.extend(messages);
        let body = json!({
            "model": self.model,
            "messages": turns,
            "stream": true,
            "options": {"num_ctx": self.num_ctx},
        });
        Box::pin(async_stream::stream! {
            let res = match client.post(&url).json(&body).send().await {
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
                if line.trim().is_empty() {
                    continue;
                }
                let chunk: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        yield Err(ProviderError(format!("Request to {WHAT} failed: {e}")));
                        return;
                    }
                };
                if crate::settings::truthy(&chunk["error"]) {
                    yield Err(ProviderError(format!("Ollama: {}", plain(&chunk["error"]))));
                    return;
                }
                if let Some(text) = chunk["message"]["content"].as_str().filter(|t| !t.is_empty()) {
                    yield Ok(text.to_string());
                }
                if crate::settings::truthy(&chunk["done"]) {
                    break;
                }
            }
        })
    }

    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>> {
        Box::pin(timed(self.probe()))
    }
}

/// A JSON value as Python's `str()` would print it in an f-string: strings bare, the rest
/// as JSON.
pub(crate) fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
