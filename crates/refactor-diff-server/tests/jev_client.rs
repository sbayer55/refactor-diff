//! The Jev client against a mock Vercel AI Gateway.

use refactor_diff_server::ai::http_client;
use refactor_diff_server::ai::jev::questions::questions;
use refactor_diff_server::ai::jev::{Answer, JevClient, JevEvaluator};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A URL nothing listens on.
const REFUSED: &str = "http://127.0.0.1:1";

fn answers() -> Value {
    json!({
        "model": "typesafe-ai/jev",
        "answers": {
            "hardcoded_secret": {"type": "boolean", "probability": 0.96},
            "touches_auth": {"type": "boolean", "probability": 0.99},
            "blast_radius": {"type": "score", "score": 3.0, "probabilities": {"3": 1}},
            "reviewer_effort": {"type": "score", "score": 2.0, "probabilities": {"2": 1}},
            "primary_concern": {"type": "choice", "choice": "secret", "probabilities": {"secret": 0.69, "auth": 0.31}}
        },
        "usage": {"inputTokens": 275, "outputTokens": 20},
        "providerMetadata": {"gateway": {"cost": "0.00001155"}}
    })
}

fn client(base: &str, zdr: bool) -> JevClient {
    JevClient::new(
        http_client(),
        base.to_string(),
        "vck_test".into(),
        "typesafe-ai/jev".into(),
        zdr,
    )
}

#[tokio::test]
async fn evaluate_posts_the_documented_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/evaluate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answers()))
        .mount(&server)
        .await;
    let c = client(&format!("{}/", server.uri()), true);
    let ev = c
        .evaluate(json!({"path": "api/auth.py", "hunk": "+ x"}), &questions())
        .await
        .unwrap();
    assert_eq!(ev.answers.len(), 5);
    assert_eq!(
        ev.answers["hardcoded_secret"],
        Answer::Boolean { probability: 0.96 }
    );
    assert_eq!(ev.usage.unwrap().input_tokens, 275);
    assert_eq!(ev.cost.as_deref(), Some("0.00001155"));

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(
        req.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer vck_test"
    );
    assert!(
        req.headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body: Value = req.body_json().unwrap();
    assert_eq!(body["model"], "typesafe-ai/jev");
    assert_eq!(body["state"]["path"], "api/auth.py");
    assert_eq!(body["state"]["hunk"], "+ x");
    let q = body["questions"].as_object().unwrap();
    assert_eq!(q.len(), 10);
    assert_eq!(q["touches_auth"]["type"], "boolean");
    assert_eq!(q["blast_radius"]["type"], "score");
    assert_eq!(q["primary_concern"]["type"], "choice");
    assert!(q["primary_concern"]["criteria"]["insufficient_evidence"].is_string());
    assert_eq!(
        body["providerOptions"]["gateway"],
        json!({"only": ["typesafe-ai"], "zeroDataRetention": true})
    );
}

#[tokio::test]
async fn rate_limits_are_retried_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/evaluate"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/evaluate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(answers()))
        .mount(&server)
        .await;
    let c = client(&server.uri(), false);
    let ev = c.evaluate(json!("s"), &questions()).await.unwrap();
    assert_eq!(ev.answers.len(), 5);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn errors_are_readable() {
    let c = client(REFUSED, false);
    let err = c.evaluate(json!("s"), &questions()).await.unwrap_err();
    assert_eq!(
        err.0,
        "Couldn't connect to Vercel AI Gateway. Is it running?"
    );
    let res = c.test().await;
    assert!(!res.ok);
    assert!(res.error.as_deref().unwrap().contains("Couldn't connect"));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "bad key", "type": "auth"}})),
        )
        .mount(&server)
        .await;
    let c = client(&server.uri(), false);
    let err = c.evaluate(json!("s"), &questions()).await.unwrap_err();
    assert_eq!(
        err.0,
        "Vercel AI Gateway returned HTTP 401: bad key (check the API key)"
    );
    // A 401 is not retried.
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"model": "x"})))
        .mount(&server)
        .await;
    let c = client(&server.uri(), false);
    let err = c.evaluate(json!("s"), &questions()).await.unwrap_err();
    assert_eq!(err.0, "Vercel AI Gateway returned no answers.");
}

#[tokio::test]
async fn test_probe_reports_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/evaluate"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"answers": {"ok": {"type": "boolean", "probability": 0.99}}}),
            ),
        )
        .mount(&server)
        .await;
    let c = client(&server.uri(), false);
    let res = c.test().await;
    assert!(res.ok, "{res:?}");
    assert_eq!(res.models, vec!["typesafe-ai/jev"]);
    assert!(res.latency_ms.is_some());
    let body: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(body["state"], "ping");
    assert_eq!(body["questions"]["ok"]["type"], "boolean");
}
