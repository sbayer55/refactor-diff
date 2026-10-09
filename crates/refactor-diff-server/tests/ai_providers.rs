//! The model providers against a mock HTTP server (the port of `tests/test_ai_providers.py`).

use futures::StreamExt;
use refactor_diff_server::ai::{
    CLAUDE_MODELS, ClaudeProvider, DeltaStream, MAX_TOKENS, Message, OllamaProvider,
    OpenAIProvider, Provider, ProviderError, http_client,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn collect(mut stream: DeltaStream) -> Result<String, ProviderError> {
    let mut out = String::new();
    while let Some(item) = stream.next().await {
        out.push_str(&item?);
    }
    Ok(out)
}

fn user(text: &str) -> Vec<Message> {
    vec![Message::user(text)]
}

async fn requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap_or_default()
}

fn header(req: &wiremock::Request, name: &str) -> Option<String> {
    req.headers
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
}

/// A URL nothing listens on.
const REFUSED: &str = "http://127.0.0.1:1";

// --- Ollama --------------------------------------------------------------------------------------

#[tokio::test]
async fn ollama_streams_ndjson() {
    let server = MockServer::start().await;
    let lines = [
        json!({"message": {"role": "assistant", "content": "Hel"}, "done": false}),
        json!({"message": {"role": "assistant", "content": "lo"}, "done": false}),
        json!({"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop"}),
    ];
    let body = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    let p = OllamaProvider::new(
        http_client(),
        format!("{}/", server.uri()),
        "m".into(),
        4096,
    );
    assert_eq!(
        collect(p.stream("sys".into(), user("hi"))).await.unwrap(),
        "Hello"
    );
    let reqs = requests(&server).await;
    assert_eq!(reqs[0].url.path(), "/api/chat");
    let body: Value = reqs[0].body_json().unwrap();
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "sys"})
    );
    assert_eq!(
        body["messages"][1],
        json!({"role": "user", "content": "hi"})
    );
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["options"], json!({"num_ctx": 4096}));
    assert_eq!(body["model"], json!("m"));
}

#[tokio::test]
async fn ollama_errors_are_readable() {
    let p = OllamaProvider::new(http_client(), REFUSED.into(), "m".into(), 32768);
    let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
    assert_eq!(err.0, "Couldn't connect to Ollama. Is it running?");
    let res = p.test().await;
    assert!(!res.ok && res.error.as_deref().unwrap().contains("Couldn't connect"));
    assert!(res.latency_ms.is_none() && res.models.is_empty());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_string("model 'm' not found"))
        .mount(&server)
        .await;
    let p = OllamaProvider::new(http_client(), server.uri(), "m".into(), 32768);
    let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
    assert_eq!(err.0, "Ollama returned HTTP 404: model 'm' not found");

    let server = MockServer::start().await;
    let body = json!({"error": "model requires more system memory"}).to_string() + "\n";
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    let p = OllamaProvider::new(http_client(), server.uri(), "m".into(), 32768);
    let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
    assert_eq!(err.0, "Ollama: model requires more system memory");
}

#[tokio::test]
async fn ollama_test_lists_models() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"models": [{"name": "qwen3-coder:30b"}, {"name": "llama4"}]}),
            ),
        )
        .mount(&server)
        .await;
    let p = OllamaProvider::new(http_client(), server.uri(), "qwen3-coder:30b".into(), 32768);
    let res = p.test().await;
    assert!(res.ok && res.error.is_none());
    assert_eq!(res.models, ["qwen3-coder:30b", "llama4"]);
    assert!(res.latency_ms.is_some());
    let p = OllamaProvider::new(http_client(), server.uri(), "mistral".into(), 32768);
    let res = p.test().await;
    assert_eq!(
        res.error.as_deref(),
        Some("Ollama is running but has no model 'mistral'. Pull it with `ollama pull mistral`.")
    );
    assert!(!res.ok);
}

// --- OpenAI-compatible ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_streams_sse() {
    let server = MockServer::start().await;
    let chunks = [
        json!({"choices": [{"delta": {"role": "assistant"}}]}),
        json!({"choices": [{"delta": {"content": "Hel"}}]}),
        json!({"choices": [{"delta": {"content": "lo"}}]}),
        json!({"choices": []}),
    ];
    let body = chunks
        .iter()
        .map(|c| format!("data: {c}\n\n"))
        .collect::<String>()
        + "data: [DONE]\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let p = OpenAIProvider::new(
        http_client(),
        format!("{}/v1", server.uri()),
        "key".into(),
        "m".into(),
    );
    assert_eq!(
        collect(p.stream("sys".into(), user("hi"))).await.unwrap(),
        "Hello"
    );
    let reqs = requests(&server).await;
    assert_eq!(
        header(&reqs[0], "authorization").as_deref(),
        Some("Bearer key")
    );
    let body: Value = reqs[0].body_json().unwrap();
    assert_eq!(body["model"], json!("m"));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "sys"})
    );

    // No key: no Authorization header; an error payload is reported.
    let server = MockServer::start().await;
    let body = format!("data: {}\n\n", json!({"error": {"message": "bad model"}}));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let p = OpenAIProvider::new(http_client(), server.uri(), String::new(), "m".into());
    let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
    assert_eq!(err.0, r#"Endpoint error: {"message":"bad model"}"#);
    assert!(header(&requests(&server).await[0], "authorization").is_none());
}

#[tokio::test]
async fn openai_test_falls_back_to_a_completion() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"choices": [{"message": {"content": "ok"}}]})),
        )
        .mount(&server)
        .await;
    let p = OpenAIProvider::new(
        http_client(),
        format!("{}/v1", server.uri()),
        String::new(),
        "m".into(),
    );
    let res = p.test().await;
    assert!(res.ok && res.models.is_empty());
    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(paths, ["/v1/models", "/v1/chat/completions"]);
    let body: Value = requests(&server).await[1].body_json().unwrap();
    assert_eq!(body["max_tokens"], json!(8));

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"id": "gpt-x"}, "junk", {"id": "gpt-y"}]})),
        )
        .mount(&server)
        .await;
    let p = OpenAIProvider::new(
        http_client(),
        format!("{}/v1", server.uri()),
        String::new(),
        "m".into(),
    );
    assert_eq!(p.test().await.models, ["gpt-x", "gpt-y"]);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
        .mount(&server)
        .await;
    let p = OpenAIProvider::new(
        http_client(),
        format!("{}/v1", server.uri()),
        String::new(),
        "m".into(),
    );
    let res = p.test().await;
    assert_eq!(
        res.error.as_deref(),
        Some(r#"the endpoint returned HTTP 401: {"error":"bad key"}"#)
    );
}

// --- Claude --------------------------------------------------------------------------------------

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect()
}

fn claude_stream(parts: &[&str], stop_reason: &str) -> String {
    let mut events = vec![
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "content": [], "model": "m", "stop_reason": null, "usage": {"input_tokens": 1, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "ping"}),
    ];
    for part in parts {
        events.push(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": part}}));
    }
    events.push(json!({"type": "content_block_stop", "index": 0}));
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": stop_reason, "stop_sequence": null}, "usage": {"output_tokens": 2}}));
    events.push(json!({"type": "message_stop"}));
    sse(&events)
}

fn claude(server: &MockServer, model: &str) -> ClaudeProvider {
    ClaudeProvider::new(
        http_client(),
        "k".into(),
        model.into(),
        Some(format!("{}/", server.uri())),
    )
}

async fn mount_stream(server: &MockServer, body: String) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(server)
        .await;
}

#[tokio::test]
async fn claude_streams_and_flags_cut_off_answers() {
    let server = MockServer::start().await;
    mount_stream(&server, claude_stream(&["a", "b"], "end_turn")).await;
    let p = claude(&server, "claude-opus-5-5");
    assert_eq!(
        collect(p.stream("sys".into(), user("q"))).await.unwrap(),
        "ab"
    );
    let reqs = requests(&server).await;
    assert_eq!(reqs[0].url.path(), "/v1/messages");
    assert_eq!(header(&reqs[0], "x-api-key").as_deref(), Some("k"));
    assert_eq!(
        header(&reqs[0], "anthropic-version").as_deref(),
        Some("2023-06-01")
    );
    let body: Value = reqs[0].body_json().unwrap();
    assert_eq!(body["system"], json!("sys"));
    assert_eq!(body["model"], json!("claude-opus-5-5"));
    assert_eq!(body["max_tokens"], json!(MAX_TOKENS));
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["output_config"], json!({"effort": "medium"}));
    assert_eq!(body["messages"], json!([{"role": "user", "content": "q"}]));

    let server = MockServer::start().await;
    mount_stream(&server, claude_stream(&["a"], "max_tokens")).await;
    let text = collect(claude(&server, "m").stream("s".into(), vec![]))
        .await
        .unwrap();
    assert_eq!(
        text,
        "a\n\n*(answer cut off: the response hit the length limit)*"
    );

    let server = MockServer::start().await;
    mount_stream(&server, claude_stream(&[], "refusal")).await;
    let err = collect(claude(&server, "m").stream("s".into(), vec![]))
        .await
        .unwrap_err();
    assert_eq!(err.0, "Claude declined to answer this request.");
}

#[tokio::test]
async fn claude_translates_http_errors() {
    let cases = [
        (401, "Claude rejected the API key. Check it in Settings."),
        (403, "The Claude API key doesn't have access to this model."),
        (
            404,
            "Claude doesn't know the model 'm'. Check it in Settings.",
        ),
        (
            429,
            "Claude is rate limiting requests; try again in a moment.",
        ),
        (
            400,
            "Claude returned an error (400): messages: roles must alternate",
        ),
    ];
    for (status, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "type": "error",
                "error": {"type": "x", "message": "messages: roles must alternate"},
            })))
            .mount(&server)
            .await;
        let p = claude(&server, "m");
        let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
        assert_eq!(err.0, expected, "status {status}");
        let res = p.test().await;
        assert!(!res.ok);
        assert_eq!(res.error.as_deref(), Some(expected), "status {status}");
    }
    // Rate limiting and server errors are retried once, like the SDK.
    let server = MockServer::start().await;
    let reqs = requests(&server).await;
    assert!(reqs.is_empty());
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(529).set_body_string("overloaded"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_stream(&server, claude_stream(&["ok"], "end_turn")).await;
    assert_eq!(
        collect(claude(&server, "m").stream("s".into(), vec![]))
            .await
            .unwrap(),
        "ok"
    );
    assert_eq!(requests(&server).await.len(), 2);

    let p = ClaudeProvider::new(http_client(), "k".into(), "m".into(), Some(REFUSED.into()));
    let err = collect(p.stream("s".into(), vec![])).await.unwrap_err();
    assert_eq!(
        err.0,
        "Couldn't reach the Claude API. Check your network or base URL."
    );
}

#[tokio::test]
async fn claude_reports_stream_errors_and_tests_the_connection() {
    let server = MockServer::start().await;
    let body = sse(&[
        json!({"type": "message_start", "message": {}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "partial"}}),
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    ]);
    mount_stream(&server, body).await;
    let mut stream = claude(&server, "m").stream("s".into(), vec![]);
    assert_eq!(stream.next().await.unwrap().unwrap(), "partial");
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().0,
        "Claude request failed: Overloaded"
    );
    assert!(stream.next().await.is_none());

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
            "content": [{"type": "text", "text": "ok"}], "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 1},
        })))
        .mount(&server)
        .await;
    let res = claude(&server, "m").test().await;
    assert!(res.ok && res.error.is_none() && res.latency_ms.is_some());
    assert_eq!(res.models, CLAUDE_MODELS);
    let body: Value = requests(&server).await[0].body_json().unwrap();
    assert_eq!(body["max_tokens"], json!(8));
    assert_eq!(body["stream"], Value::Null);
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "Reply with the single word ok."}])
    );
}
