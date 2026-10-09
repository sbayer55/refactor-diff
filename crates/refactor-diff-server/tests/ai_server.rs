//! The Ask endpoints and the settings test through the HTTP API, with a fake provider (the
//! port of `tests/test_ai_server.py`).

mod common;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use common::{TestServer, fixture_repo};
use pretty_assertions::assert_eq;
use refactor_diff_server::ai::{
    DeltaStream, Message, Provider, ProviderError, ProviderFactory, TestResult,
};
use refactor_diff_server::settings::Settings;
use serde_json::{Value, json};

fn rename_repo(tmp: &Path) -> std::path::PathBuf {
    fixture_repo("rename_project", &tmp.join("repo"))
}

/// A provider that replays canned parts and records what it was asked.
struct FakeProvider {
    parts: Vec<String>,
    fail: Option<String>,
    calls: Mutex<Vec<(String, Vec<Message>)>>,
}

impl FakeProvider {
    fn new(parts: &[&str], fail: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            parts: parts.iter().map(|p| p.to_string()).collect(),
            fail: fail.map(String::from),
            calls: Mutex::new(vec![]),
        })
    }

    fn default_parts() -> Arc<Self> {
        Self::new(&["Hello ", "`L6` world"], None)
    }

    fn calls(&self) -> Vec<(String, Vec<Message>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Provider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake-1"
    }

    fn stream(&self, system: String, messages: Vec<Message>) -> DeltaStream {
        self.calls.lock().unwrap().push((system, messages));
        let mut items: Vec<Result<String, ProviderError>> =
            self.parts.iter().cloned().map(Ok).collect();
        if let Some(fail) = &self.fail {
            items.push(Err(ProviderError(fail.clone())));
        }
        Box::pin(futures::stream::iter(items))
    }

    fn test(&self) -> Pin<Box<dyn Future<Output = TestResult> + Send + '_>> {
        Box::pin(async {
            TestResult {
                ok: true,
                error: None,
                latency_ms: Some(1),
                models: vec!["fake-1".into()],
            }
        })
    }
}

/// Hands out one provider and records the settings it was built from.
struct FakeFactory {
    provider: Arc<FakeProvider>,
    seen: Mutex<Option<Value>>,
}

impl FakeFactory {
    fn new(provider: Arc<FakeProvider>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            seen: Mutex::new(None),
        })
    }
}

impl ProviderFactory for FakeFactory {
    fn build(&self, settings: &Settings) -> Result<Arc<dyn Provider>, ProviderError> {
        *self.seen.lock().unwrap() = Some(settings.as_value()["ai"].clone());
        Ok(self.provider.clone())
    }
}

/// `event: x\ndata: {...}` blocks as `(event, data)` pairs.
fn events(text: &str) -> Vec<(String, Value)> {
    text.trim()
        .split("\n\n")
        .map(|block| {
            let mut event = String::new();
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    event = v.to_string();
                } else if let Some(v) = line.strip_prefix("data: ") {
                    data = v.to_string();
                }
            }
            (
                event,
                serde_json::from_str(&data).expect("event data is JSON"),
            )
        })
        .collect()
}

/// Analyze main...feature and return the report id and the api.py hunk.
async fn analyzed(server: &TestServer) -> (String, Value) {
    let report = server
        .analyze(json!({"base": "main", "head": "feature"}))
        .await;
    let hunk = report["hunks"]
        .as_object()
        .unwrap()
        .values()
        .find(|h| h["path"] == "api.py")
        .unwrap()
        .clone();
    (report["id"].as_str().unwrap().to_string(), hunk)
}

// --- settings ------------------------------------------------------------------------------------

#[tokio::test]
async fn settings_test_uses_unsaved_values() {
    let tmp = tempfile::tempdir().unwrap();
    let factory = FakeFactory::new(FakeProvider::default_parts());
    let server = TestServer::with_providers(&rename_repo(tmp.path()), factory.clone());
    let res = server
        .post(
            "/api/settings/test",
            json!({"provider": "ollama", "config": {"host": "http://h", "model": "m"}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json(),
        json!({"ok": true, "error": null, "latency_ms": 1, "models": ["fake-1"]})
    );
    let seen = factory.seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen["provider"], "ollama");
    assert_eq!(seen["providers"]["ollama"]["model"], "m");
    assert_eq!(seen["providers"]["ollama"]["host"], "http://h");
}

// --- menu ----------------------------------------------------------------------------------------

#[tokio::test]
async fn ai_menu_describes_the_location() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let (id, hunk) = analyzed(&server).await;
    let url = format!("/api/report/{id}/ai/menu");
    let res = server.post(&url, json!({"hunk_id": hunk["id"]})).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let data = res.json();
    assert_eq!(
        data["focus"],
        json!({"path": "api.py", "side": "new", "line": 6, "qualname": "handle"})
    );
    let ids: Vec<&str> = data["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(&ids[..3], ["explain", "function", "compare"]);
    assert!(ids.contains(&"break"));
    assert_eq!(data["provider"]["provider"], "claude");
    assert_eq!(data["provider"]["configured"], false);
    assert_eq!(
        data["context"],
        json!({"function": true, "pr": true, "references": false})
    );
    let rows: Vec<&Value> = data["tasks"].as_array().unwrap().iter().collect();
    let explain = rows.iter().find(|r| r["id"] == "explain").unwrap();
    assert_eq!(explain["hint"], "2 unexplained lines");
    assert_eq!(explain["unavailable"], Value::Null);
    // The server has a navigator, so the reference tasks are offered.
    let uses = rows.iter().find(|r| r["id"] == "uses").unwrap();
    assert_eq!(uses["unavailable"], Value::Null);
    let why = rows.iter().find(|r| r["id"] == "why").unwrap();
    assert_eq!(why["hint"], "1 commit");

    let res = server
        .post(&url, json!({"hunk_id": hunk["id"], "side": "n", "line": 1}))
        .await;
    assert_eq!(res.json()["focus"]["qualname"], Value::Null);
    let res = server.post(&url, json!({"hunk_id": "nope"})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Unknown hunk; run the analysis again.");
    let res = server
        .post(&url, json!({"hunk_id": hunk["id"], "side": "x", "line": 1}))
        .await;
    assert_eq!(res.error(), "side must be old or new.");
    let res = server.post("/api/report/zzz/ai/menu", json!({})).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refs_count_needs_a_def() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let (id, hunk) = analyzed(&server).await;
    let res = server
        .post(
            &format!("/api/report/{id}/ai/refs-count"),
            json!({"hunk_id": hunk["id"], "side": "n", "line": 1}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json(), json!({"count": null}));
    let res = server
        .post(
            &format!("/api/report/{id}/ai/refs-count"),
            json!({"hunk_id": hunk["id"], "side": "n", "line": 99}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Line 99 isn't part of that hunk.");
}

// --- ask -----------------------------------------------------------------------------------------

#[tokio::test]
async fn ask_streams_events() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = FakeProvider::default_parts();
    let server =
        TestServer::with_providers(&rename_repo(tmp.path()), FakeFactory::new(provider.clone()));
    let (id, hunk) = analyzed(&server).await;
    let res = server
        .post(
            &format!("/api/report/{id}/ai/ask"),
            json!({"task": "explain", "hunk_id": hunk["id"]}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert!(
        res.content_type.starts_with("text/event-stream"),
        "{}",
        res.content_type
    );
    let evs = events(&res.text());
    assert_eq!(evs[0].0, "meta");
    assert_eq!(evs[0].1["provider"], "fake");
    assert_eq!(evs[0].1["model"], "fake-1");
    assert_eq!(evs[0].1["task"], "explain");
    assert_eq!(
        evs[0].1["focus"],
        json!({"path": "api.py", "side": "new", "line": 6})
    );
    assert_eq!(evs[0].1["pieces"], json!(["function", "hunk", "patterns"])); // no PR here
    let deltas: Vec<&Value> = evs[1..evs.len() - 1]
        .iter()
        .map(|(_, d)| &d["text"])
        .collect();
    assert_eq!(deltas, [&json!("Hello "), &json!("`L6` world")]);
    let (last, data) = evs.last().unwrap();
    assert_eq!(last, "done");
    assert!(data["elapsed_ms"].as_u64().is_some());

    let calls = provider.calls();
    let (system, messages) = &calls[0];
    assert!(system.contains("Task: Explain what this hunk changes"));
    assert_eq!(messages[0].role, "user");
    let user = &messages[0].content;
    assert!(user.contains("## Hunk (api.py"), "{user}");
    assert!(user.contains("## Enclosing def handle — after"), "{user}");
    assert!(!user.contains("## Commits")); // explain doesn't ask for history
}

#[tokio::test]
async fn ask_custom_prompt_and_follow_ups() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = FakeProvider::default_parts();
    let server =
        TestServer::with_providers(&rename_repo(tmp.path()), FakeFactory::new(provider.clone()));
    let (id, hunk) = analyzed(&server).await;
    let url = format!("/api/report/{id}/ai/ask");
    let history = json!([
        {"role": "assistant", "content": "Earlier answer"},
        {"role": "user", "content": "And now?"},
    ]);
    let body = json!({
        "task": "custom",
        "hunk_id": hunk["id"],
        "side": "n",
        "line": 6,
        "prompt": "Is the 404 right?",
        "pieces": {"function": false, "pr": true},
        "history": history,
    });
    let res = server.post(&url, body.clone()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let calls = provider.calls();
    let msgs = &calls[0].1;
    assert!(msgs[0].content.ends_with("## Question\nIs the 404 right?"));
    assert!(!msgs[0].content.contains("## Enclosing"));
    assert!(msgs[0].content.contains("## Pull request"));
    assert_eq!(
        serde_json::to_value(&msgs[1..]).unwrap(),
        history,
        "follow-up turns are sent after the context"
    );
    let evs = events(&res.text());
    assert_eq!(evs[0].1["task"], "custom");
    assert_eq!(evs[0].1["pieces"], json!(["hunk", "patterns"])); // pr requested but absent

    let mut bad = body.clone();
    bad["prompt"] = json!("   ");
    let res = server.post(&url, bad).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "The prompt is empty.");
    let mut bad = body.clone();
    bad["history"] = json!([{"role": "user", "content": "x"}]);
    let res = server.post(&url, bad).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Malformed follow-up history.");
}

#[tokio::test]
async fn ask_rejects_what_it_cannot_do() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let (id, hunk) = analyzed(&server).await;
    let url = format!("/api/report/{id}/ai/ask");
    // Not configured: the error tells the UI to open Settings.
    let res = server
        .post(&url, json!({"task": "explain", "hunk_id": hunk["id"]}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json(),
        json!({"error": "Claude needs an API key. Open Settings to add one.", "settings": true})
    );
    let saved = server
        .post(
            "/api/settings",
            json!({"ai": {"providers": {"claude": {"api_key": "k"}}}}),
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK);
    let res = server
        .post(&url, json!({"task": "nope", "hunk_id": hunk["id"]}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Unknown task.");
    // A task that needs a def, asked on the import line.
    let res = server
        .post(
            &url,
            json!({"task": "function", "hunk_id": hunk["id"], "side": "n", "line": 1}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        res.error(),
        "How does this function work isn't available here: the line isn't inside a function or class."
    );
    // References need navigation, which this server has, so `uses` is allowed here; the
    // focus is validated first.
    let res = server
        .post(
            &url,
            json!({"task": "uses", "hunk_id": hunk["id"], "side": "n", "line": 99}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Line 99 isn't part of that hunk.");
    let res = server
        .post(&url, json!({"task": "explain", "hunk_id": "nope"}))
        .await;
    assert_eq!(res.error(), "Unknown hunk; run the analysis again.");
    let res = server
        .post("/api/report/zzz/ai/ask", json!({"task": "explain"}))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ask_reports_provider_errors_in_the_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = FakeProvider::new(&["partial"], Some("Ollama fell over"));
    let server =
        TestServer::with_providers(&rename_repo(tmp.path()), FakeFactory::new(provider.clone()));
    let (id, hunk) = analyzed(&server).await;
    let res = server
        .post(
            &format!("/api/report/{id}/ai/ask"),
            json!({"task": "review", "hunk_id": hunk["id"]}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let evs = events(&res.text());
    let names: Vec<&str> = evs.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names, ["meta", "delta", "error"]);
    assert_eq!(evs[1].1, json!({"text": "partial"}));
    assert_eq!(evs[2].1, json!({"message": "Ollama fell over"}));
    assert!(provider.calls()[0].0.contains("Verdict:"));
}
