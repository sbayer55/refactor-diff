//! The Jev scoring endpoints through the HTTP API, with a fake evaluator.

mod common;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use common::{TestServer, fixture_repo};
use pretty_assertions::assert_eq;
use refactor_diff_server::ai::jev::{
    Evaluation, JevEvaluator, JevFactory, NEEDS_KEY, Questions, parse_evaluation,
};
use refactor_diff_server::ai::{ProviderError, TestResult};
use refactor_diff_server::settings::Settings;
use serde_json::{Value, json};

fn rename_repo(tmp: &Path) -> std::path::PathBuf {
    fixture_repo("rename_project", &tmp.join("repo"))
}

/// Answers every hunk the same way and records the states it was given.
struct FakeJev {
    answers: Value,
    fail: Option<String>,
    states: Mutex<Vec<Value>>,
}

impl FakeJev {
    fn new(answers: Value, fail: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            answers,
            fail: fail.map(String::from),
            states: Mutex::new(vec![]),
        })
    }

    fn risky() -> Arc<Self> {
        Self::new(
            json!({
                "hardcoded_secret": {"type": "boolean", "probability": 0.96},
                "touches_auth": {"type": "boolean", "probability": 0.99},
                "breaks_api": {"type": "boolean", "probability": 0.2},
                "blast_radius": {"type": "score", "score": 3.0, "probabilities": {"3": 1}},
                "reviewer_effort": {"type": "score", "score": 2.0, "probabilities": {"2": 1}},
                "primary_concern": {"type": "choice", "choice": "secret", "probabilities": {"secret": 0.69, "auth": 0.31}}
            }),
            None,
        )
    }

    fn calls(&self) -> usize {
        self.states.lock().unwrap().len()
    }
}

impl JevEvaluator for FakeJev {
    fn model(&self) -> &str {
        "fake-jev"
    }

    fn evaluate(
        &self,
        state: Value,
        _questions: &Questions,
    ) -> Pin<Box<dyn Future<Output = Result<Evaluation, ProviderError>> + Send + '_>> {
        self.states.lock().unwrap().push(state);
        let out = match &self.fail {
            Some(msg) => Err(ProviderError(msg.clone())),
            None => parse_evaluation(&json!({"answers": self.answers})),
        };
        Box::pin(async move { out })
    }

    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>> {
        Box::pin(async {
            TestResult {
                ok: true,
                error: None,
                latency_ms: Some(1),
                models: vec!["fake-jev".into()],
            }
        })
    }
}

/// Builds the fake only when a key is set, like the real factory.
struct FakeJevFactory {
    jev: Arc<FakeJev>,
}

impl FakeJevFactory {
    fn new(jev: Arc<FakeJev>) -> Arc<Self> {
        Arc::new(Self { jev })
    }
}

impl JevFactory for FakeJevFactory {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn JevEvaluator>, ProviderError> {
        if !settings.jev().configured() {
            return Err(ProviderError::new(NEEDS_KEY));
        }
        Ok(self.jev.clone())
    }
}

async fn configure(server: &TestServer, auto: bool) {
    let res = server
        .post(
            "/api/settings",
            json!({"jev": {"api_key": "vck_test", "auto": auto}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
}

async fn analyzed(server: &TestServer) -> Value {
    server
        .analyze(json!({"base": "main", "head": "feature"}))
        .await
}

/// Poll `GET /jev` until the job is no longer running.
async fn wait_done(server: &TestServer, id: &str) -> Value {
    for _ in 0..500 {
        let data = server.get(&format!("/api/report/{id}/jev")).await.json();
        if data["status"]["state"] != "running" {
            return data;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("scoring never finished");
}

#[tokio::test]
async fn scoring_needs_a_key() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::risky();
    let server = TestServer::with_jev(&rename_repo(tmp.path()), FakeJevFactory::new(jev), None);
    let report = analyzed(&server).await;
    let id = report["id"].as_str().unwrap();
    let before = server.get(&format!("/api/report/{id}/jev")).await.json();
    assert_eq!(before["status"], json!({"state": "idle"}));
    assert_eq!(before["hunks"], json!({}));
    assert_eq!(before["configured"], false);
    assert_eq!(before["auto"], false);
    assert_eq!(before["question_set"], "1");
    let res = server
        .post(&format!("/api/report/{id}/jev/score"), json!({}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), NEEDS_KEY);
    assert_eq!(res.json()["settings"], true);
}

#[tokio::test]
async fn scores_every_residual_hunk_and_caches() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::risky();
    let server = TestServer::with_jev(
        &rename_repo(tmp.path()),
        FakeJevFactory::new(jev.clone()),
        None,
    );
    configure(&server, false).await;
    let report = analyzed(&server).await;
    let id = report["id"].as_str().unwrap();
    let residual: Vec<&str> = report["residual_hunk_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!residual.is_empty());

    let res = server
        .post(&format!("/api/report/{id}/jev/score"), json!({}))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["status"]["state"], "running");
    assert_eq!(res.json()["configured"], true);

    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["state"], "done", "{done}");
    assert_eq!(done["status"]["total"], residual.len());
    assert_eq!(done["status"]["scored"], residual.len());
    assert_eq!(done["status"]["cached"], 0);
    assert_eq!(done["status"]["failed"], 0);
    assert_eq!(done["failures"], json!([]));
    let hunks = done["hunks"].as_object().unwrap();
    assert_eq!(hunks.len(), residual.len());
    for hid in &residual {
        let r = &hunks[*hid];
        assert_eq!(r["hunk_id"], *hid);
        assert_eq!(r["fingerprint"], report["hunks"][hid]["fingerprint"]);
        assert_eq!(r["path"], report["hunks"][hid]["path"]);
        assert_eq!(r["flagged"], true);
        assert_eq!(r["level"], "high");
        assert_eq!(r["primary_concern"], "secret");
        assert_eq!(r["confidence"], 0.69);
        assert_eq!(r["runner_up"], json!({"name": "auth", "p": 0.31}));
        assert_eq!(r["blast_label"], "core");
        assert_eq!(r["effort_label"], "expert");
        assert_eq!(r["concerns"].as_array().unwrap().len(), 7);
        assert_eq!(r["concerns"][0], json!({"name": "touches_auth", "p": 0.99}));
        assert_eq!(r["question_set"], "1");
        assert!(r["risk"].as_f64().unwrap() > 0.9);
    }
    assert_eq!(jev.calls(), residual.len());
    let states = jev.states.lock().unwrap().clone();
    let api = states.iter().find(|s| s["path"] == "api.py").unwrap();
    assert_eq!(api["category"], "source");
    assert_eq!(api["language"], "python");
    assert_eq!(api["adds_tests"], false);
    assert_eq!(api["docs_only"], false);
    assert!(api["hunk"].as_str().unwrap().contains("+"));
    assert!(api["changed_test_files"].is_array());
    assert!(api["patterns"].is_array());
    assert!(api["enclosing"]["qualname"].is_string(), "{api}");

    // The cache file exists and a second run sends nothing.
    let cache_dir = server.state_dir.join("jev");
    assert!(std::fs::read_dir(&cache_dir).unwrap().next().is_some());
    let res = server
        .post(&format!("/api/report/{id}/jev/score"), json!({}))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["state"], "done");
    assert_eq!(done["status"]["cached"], residual.len());
    assert_eq!(done["status"]["total"], 0);
    assert_eq!(done["hunks"].as_object().unwrap().len(), residual.len());
    assert_eq!(jev.calls(), residual.len());

    // `force` ignores the cache.
    let res = server
        .post(
            &format!("/api/report/{id}/jev/score"),
            json!({"force": true}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["cached"], 0);
    assert_eq!(jev.calls(), residual.len() * 2);

    // Another server over the same state dir reads the cache without a job.
    let other = TestServer::with_jev(
        &tmp.path().join("repo"),
        FakeJevFactory::new(FakeJev::risky()),
        Some(&server.state_dir),
    );
    let report2 = analyzed(&other).await;
    assert_eq!(report2["id"], report["id"]);
    let data = other.get(&format!("/api/report/{id}/jev")).await.json();
    assert_eq!(data["status"], json!({"state": "idle"}));
    assert_eq!(data["hunks"].as_object().unwrap().len(), residual.len());
    assert_eq!(data["hunks"][residual[0]]["primary_concern"], "secret");
}

#[tokio::test]
async fn auto_scoring_runs_after_analysis() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::risky();
    let server = TestServer::with_jev(
        &rename_repo(tmp.path()),
        FakeJevFactory::new(jev.clone()),
        None,
    );
    configure(&server, true).await;
    let report = analyzed(&server).await;
    // The analysis response is the plain report plus its review state.
    assert!(report.get("jev").is_none());
    let id = report["id"].as_str().unwrap();
    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["state"], "done", "{done}");
    assert_eq!(done["auto"], true);
    assert_eq!(
        done["hunks"].as_object().unwrap().len(),
        report["residual_hunk_ids"].as_array().unwrap().len()
    );
    assert!(jev.calls() > 0);
}

#[tokio::test]
async fn an_auth_failure_stops_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::new(
        json!({}),
        Some("Vercel AI Gateway returned HTTP 401: bad key (check the API key)"),
    );
    let server = TestServer::with_jev(
        &rename_repo(tmp.path()),
        FakeJevFactory::new(jev.clone()),
        None,
    );
    configure(&server, false).await;
    let report = analyzed(&server).await;
    let id = report["id"].as_str().unwrap();
    let res = server
        .post(&format!("/api/report/{id}/jev/score"), json!({}))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["state"], "error", "{done}");
    assert!(
        done["status"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP 401")
    );
    assert_eq!(done["hunks"], json!({}));
    assert!(!done["failures"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn other_failures_are_recorded_per_hunk() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::new(json!({}), Some("Vercel AI Gateway didn't respond in time."));
    let server = TestServer::with_jev(
        &rename_repo(tmp.path()),
        FakeJevFactory::new(jev.clone()),
        None,
    );
    configure(&server, false).await;
    let report = analyzed(&server).await;
    let id = report["id"].as_str().unwrap();
    let total = report["residual_hunk_ids"].as_array().unwrap().len();
    server
        .post(&format!("/api/report/{id}/jev/score"), json!({}))
        .await;
    let done = wait_done(&server, id).await;
    assert_eq!(done["status"]["state"], "done", "{done}");
    assert_eq!(done["status"]["failed"], total);
    assert_eq!(done["status"]["scored"], 0);
    assert_eq!(done["failures"].as_array().unwrap().len(), total);
    assert_eq!(
        done["failures"][0]["error"],
        "Vercel AI Gateway didn't respond in time."
    );
}

#[tokio::test]
async fn settings_test_reaches_the_jev_client() {
    let tmp = tempfile::tempdir().unwrap();
    let jev = FakeJev::risky();
    let server = TestServer::with_jev(&rename_repo(tmp.path()), FakeJevFactory::new(jev), None);
    let res = server
        .post(
            "/api/settings/test",
            json!({"provider": "jev", "config": {"api_key": ""}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["ok"], false);
    assert_eq!(res.json()["error"], NEEDS_KEY);
    let res = server
        .post(
            "/api/settings/test",
            json!({"provider": "jev", "config": {"api_key": "vck_x"}}),
        )
        .await;
    assert_eq!(
        res.json(),
        json!({"ok": true, "error": null, "latency_ms": 1, "models": ["fake-jev"]})
    );
    // The settings view carries the section with the key masked.
    configure(&server, true).await;
    let view = server.get("/api/settings").await.json();
    assert_eq!(view["jev"]["api_key"], "••••test");
    assert_eq!(view["jev"]["configured"], true);
    assert_eq!(view["jev"]["auto"], true);
    assert_eq!(view["jev"]["model"], "typesafe-ai/jev");
}
