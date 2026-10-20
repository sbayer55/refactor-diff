//! Risk scoring of needs-review hunks with TypeSafe AI's Jev through Vercel AI Gateway.
//!
//! Jev is a decision model: given some state and a set of typed questions it returns
//! probabilities, never prose. Each residual hunk is sent once with every question in
//! [`questions`]; the answers are folded into a [`risk::HunkRisk`] that the UI uses to rank
//! the needs-review list, draw a risk chip on the hunk and fill the RISK section. Results are
//! cached per hunk fingerprint in [`store::JevStore`], and [`job::JevJob`] runs the requests
//! for one report in the background.
//!
//! Scores are a third category of annotation, kept apart from the report's refactor warnings
//! (`report.warnings`) and from code issues: they never enter the report JSON.

pub mod groups;
pub mod job;
pub mod questions;
pub mod risk;
pub mod state;
pub mod store;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ProviderError, TestResult, describe, http_client, status_error, timed};
use crate::settings::Settings;

pub const DEFAULT_MODEL: &str = "typesafe-ai/jev";
pub const DEFAULT_BASE_URL: &str = "https://ai-gateway.vercel.sh";
/// The whole call, including the one retry.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Pause before the retry; a `Retry-After` header (capped at [`MAX_RETRY_AFTER`]) wins.
pub const RETRY_DELAY: Duration = Duration::from_secs(1);
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);
const WHAT: &str = "Vercel AI Gateway";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionKind {
    Boolean,
    Choice,
    Score,
}

/// One question of a request: `{"type", "instructions", "criteria"?}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    #[serde(rename = "type")]
    pub kind: QuestionKind,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<Value>,
}

/// Questions by name, in the order they are sent.
pub type Questions = IndexMap<String, Question>;

/// One answer, as `/v1/evaluate` returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Boolean {
        probability: f64,
    },
    Choice {
        choice: String,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        #[serde(default)]
        probabilities: BTreeMap<String, f64>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(rename = "inputTokens", default)]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", default)]
    pub output_tokens: u64,
}

/// A successful evaluation: the answers plus what the gateway reported about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evaluation {
    pub answers: BTreeMap<String, Answer>,
    pub usage: Option<Usage>,
    /// `providerMetadata.gateway.cost`, in dollars as a decimal string.
    pub cost: Option<String>,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Answers questions about a piece of state; the real one speaks to the gateway.
pub trait JevEvaluator: Send + Sync {
    fn model(&self) -> &str;
    fn evaluate(
        &self,
        state: Value,
        questions: &Questions,
    ) -> BoxFuture<'_, Result<Evaluation, ProviderError>>;
    fn test(&self) -> BoxFuture<'_, TestResult>;
}

/// Builds the evaluator from the settings; the server keeps one so tests can swap it.
pub trait JevFactory: Send + Sync {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn JevEvaluator>, ProviderError>;
}

pub const NEEDS_KEY: &str = "Jev needs a Vercel AI Gateway API key. Open Settings to add one.";

/// The real client over one shared HTTP client.
pub struct DefaultJevFactory {
    client: reqwest::Client,
}

impl Default for DefaultJevFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultJevFactory {
    pub fn new() -> Self {
        Self {
            client: http_client(),
        }
    }
}

impl JevFactory for DefaultJevFactory {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn JevEvaluator>, ProviderError> {
        let jev = settings.jev();
        if !jev.configured() {
            return Err(ProviderError::new(NEEDS_KEY));
        }
        Ok(Arc::new(JevClient::new(
            self.client.clone(),
            jev.base_url,
            jev.api_key,
            jev.model,
            jev.zero_data_retention,
        )))
    }
}

/// `POST {base_url}/v1/evaluate` with a bearer key.
#[derive(Clone, Debug)]
pub struct JevClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    zero_data_retention: bool,
}

impl JevClient {
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        api_key: String,
        model: String,
        zero_data_retention: bool,
    ) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            zero_data_retention,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn body(&self, state: &Value, questions: &Questions) -> Value {
        json!({
            "model": self.model,
            "state": state,
            "questions": questions,
            "providerOptions": {
                "gateway": {
                    "only": ["typesafe-ai"],
                    "zeroDataRetention": self.zero_data_retention,
                }
            },
        })
    }

    async fn send(&self, body: &Value) -> Result<reqwest::Response, ProviderError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let res = self
                .client
                .post(format!("{}/v1/evaluate", self.base_url))
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(body)
                .send()
                .await
                .map_err(|e| describe(&e, WHAT))?;
            let status = res.status();
            if status.is_success() {
                return Ok(res);
            }
            if attempt == 1 && retryable(status) {
                tokio::time::sleep(retry_after(&res).unwrap_or(RETRY_DELAY)).await;
                continue;
            }
            let text = super::body_text(res).await;
            return Err(gateway_error(status, &text));
        }
    }

    async fn call(&self, state: Value, questions: &Questions) -> Result<Evaluation, ProviderError> {
        let body = self.body(&state, questions);
        let res = match tokio::time::timeout(REQUEST_TIMEOUT, self.send(&body)).await {
            Ok(res) => res?,
            Err(_) => return Err(ProviderError(format!("{WHAT} didn't respond in time."))),
        };
        let data: Value = res.json().await.map_err(|e| describe(&e, WHAT))?;
        parse_evaluation(&data)
    }
}

fn retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_after(res: &reqwest::Response) -> Option<Duration> {
    let secs: u64 = res
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_AFTER))
}

/// The gateway's `{"error": {"message"}}` when it sent one, else the raw body.
fn gateway_error(status: reqwest::StatusCode, body: &str) -> ProviderError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    let mut err = status_error(status, &message, WHAT);
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        err.0.push_str(" (check the API key)");
    }
    err
}

/// The answers, usage and cost out of a `/v1/evaluate` response.
pub fn parse_evaluation(data: &Value) -> Result<Evaluation, ProviderError> {
    let answers = data
        .get("answers")
        .filter(|a| a.is_object())
        .ok_or_else(|| ProviderError(format!("{WHAT} returned no answers.")))?;
    let answers: BTreeMap<String, Answer> = answers
        .as_object()
        .expect("checked above")
        .iter()
        .filter_map(|(name, a)| {
            serde_json::from_value::<Answer>(a.clone())
                .ok()
                .map(|a| (name.clone(), a))
        })
        .collect();
    let usage = serde_json::from_value::<Usage>(data["usage"].clone()).ok();
    let cost = data["providerMetadata"]["gateway"]["cost"]
        .as_str()
        .map(str::to_string);
    Ok(Evaluation {
        answers,
        usage,
        cost,
    })
}

impl JevEvaluator for JevClient {
    fn model(&self) -> &str {
        &self.model
    }

    fn evaluate(
        &self,
        state: Value,
        questions: &Questions,
    ) -> BoxFuture<'_, Result<Evaluation, ProviderError>> {
        let questions = questions.clone();
        Box::pin(async move { self.call(state, &questions).await })
    }

    fn test(&self) -> BoxFuture<'_, TestResult> {
        Box::pin(async move {
            timed(async {
                let mut questions = Questions::new();
                questions.insert(
                    "ok".into(),
                    Question {
                        kind: QuestionKind::Boolean,
                        instructions: "Is the state exactly the word ping?".into(),
                        criteria: None,
                    },
                );
                self.call(json!("ping"), &questions).await?;
                Ok(vec![self.model.clone()])
            })
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::defaults;

    #[test]
    fn answers_deserialize_from_the_documented_shapes() {
        let data = json!({
            "model": "typesafe-ai/jev",
            "answers": {
                "refund": {"type": "boolean", "probability": 0.98},
                "route": {"type": "choice", "choice": "billing", "probabilities": {"billing": 1, "shipping": 0}},
                "quality": {"type": "score", "score": 2.97, "probabilities": {"0": 0, "1": 0, "2": 0.02, "3": 0.98}},
                "odd": {"type": "refusal"}
            },
            "usage": {"inputTokens": 275, "outputTokens": 20},
            "providerMetadata": {"gateway": {"cost": "0.00001155"}}
        });
        let ev = parse_evaluation(&data).unwrap();
        assert_eq!(ev.answers.len(), 3);
        assert_eq!(ev.answers["refund"], Answer::Boolean { probability: 0.98 });
        match &ev.answers["route"] {
            Answer::Choice {
                choice,
                probabilities,
            } => {
                assert_eq!(choice, "billing");
                assert_eq!(probabilities["billing"], 1.0);
            }
            other => panic!("{other:?}"),
        }
        match &ev.answers["quality"] {
            Answer::Score {
                score,
                probabilities,
            } => {
                assert_eq!(*score, 2.97);
                assert_eq!(probabilities["3"], 0.98);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            ev.usage,
            Some(Usage {
                input_tokens: 275,
                output_tokens: 20
            })
        );
        assert_eq!(ev.cost.as_deref(), Some("0.00001155"));
        assert_eq!(
            parse_evaluation(&json!({"model": "x"})).unwrap_err().0,
            "Vercel AI Gateway returned no answers."
        );
    }

    #[test]
    fn answers_round_trip_through_serde() {
        let a = Answer::Score {
            score: 1.5,
            probabilities: BTreeMap::from([("0".into(), 0.5), ("1".into(), 0.5)]),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["type"], "score");
        assert_eq!(serde_json::from_value::<Answer>(v).unwrap(), a);
    }

    #[test]
    fn factory_needs_a_key() {
        let factory = DefaultJevFactory::new();
        let err = match factory.build(&Settings(defaults())) {
            Ok(_) => panic!("built without a key"),
            Err(e) => e,
        };
        assert_eq!(err.0, NEEDS_KEY);
        let mut v = defaults();
        v["jev"] = json!({"api_key": "k", "base_url": "http://x/"});
        let client = factory.build(&Settings(v)).unwrap();
        assert_eq!(client.model(), DEFAULT_MODEL);
    }

    #[test]
    fn request_body_shape() {
        let c = JevClient::new(
            http_client(),
            "http://x/".into(),
            "k".into(),
            "m".into(),
            true,
        );
        assert_eq!(c.base_url(), "http://x");
        let body = c.body(&json!({"hunk": "h"}), &questions::questions());
        assert_eq!(body["model"], "m");
        assert_eq!(body["state"]["hunk"], "h");
        assert_eq!(
            body["providerOptions"]["gateway"]["only"],
            json!(["typesafe-ai"])
        );
        assert_eq!(
            body["providerOptions"]["gateway"]["zeroDataRetention"],
            json!(true)
        );
        assert_eq!(body["questions"]["blast_radius"]["type"], "score");
        assert_eq!(body["questions"]["touches_auth"]["type"], "boolean");
        assert_eq!(body["questions"]["primary_concern"]["type"], "choice");
        assert_eq!(body["questions"]["change_role"]["type"], "choice");
        assert_eq!(
            body["questions"]["removes_guard_or_validation"]["type"],
            "boolean"
        );
        assert_eq!(body["questions"].as_object().unwrap().len(), 13);
        let body = c.body(&json!({"occurrences": []}), &questions::group_questions());
        assert_eq!(body["questions"]["same_edit"]["type"], "boolean");
        assert_eq!(body["questions"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn gateway_errors_are_readable() {
        let e = gateway_error(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error": {"message": "bad key", "type": "x"}}"#,
        );
        assert_eq!(
            e.0,
            "Vercel AI Gateway returned HTTP 401: bad key (check the API key)"
        );
        let e = gateway_error(reqwest::StatusCode::BAD_GATEWAY, "upstream down");
        assert_eq!(e.0, "Vercel AI Gateway returned HTTP 502: upstream down");
    }
}
