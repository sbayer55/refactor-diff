//! The HTTP API, end to end against fixture repositories (ports of `tests/test_server.py`
//! and the API parts of `tests/test_state.py` / `tests/test_settings.py`).

mod common;

use std::path::Path;

use axum::http::StatusCode;
use common::{FakeGitHub, TestServer, commit, fingerprints, fixture_repo, git};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn rename_repo(tmp: &Path) -> std::path::PathBuf {
    fixture_repo("rename_project", &tmp.join("repo"))
}

const REFS: fn() -> Value = || json!({"base": "main", "head": "feature"});

#[tokio::test]
async fn index_and_static() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let index = server.get("/").await;
    assert_eq!(index.status, StatusCode::OK);
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    assert!(index.text().contains("refactor-diff"));
    assert!(index.text().contains("id=\"palette\""));
    let js = server.get("/static/app.js").await;
    assert_eq!(js.status, StatusCode::OK);
    assert_eq!(js.content_type, "text/javascript; charset=utf-8");
    let css = server.get("/static/app.css").await;
    assert_eq!(css.status, StatusCode::OK);
    assert_eq!(css.content_type, "text/css; charset=utf-8");
    let missing = server.get("/static/nope.js").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.error(), "Not found");
    let unknown = server.get("/api/nope").await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.json(), json!({"error": "Not found"}));
}

#[tokio::test]
async fn config_reports_the_repo_and_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let server = TestServer::new(&repo);
    let cfg = server.get("/api/config").await.json();
    assert_eq!(cfg["defaults"], json!({}));
    let reported = std::fs::canonicalize(cfg["repo"].as_str().unwrap()).unwrap();
    assert_eq!(reported, std::fs::canonicalize(&repo).unwrap());
}

#[tokio::test]
async fn sources_lists_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let res = server.get("/api/sources").await;
    assert_eq!(res.status, StatusCode::OK);
    let data = res.json();
    let branches: Vec<&str> = data["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_str().unwrap())
        .collect();
    assert!(branches.contains(&"main") && branches.contains(&"feature"));
    assert_eq!(data["default_base"], "main");
    assert_eq!(data["current"], "feature");
    assert_eq!(data["prs"], json!([]));
    assert_eq!(data["pr_error"], "gh CLI not found");
}

#[tokio::test]
async fn analyze_and_fetch_report() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let data = server.analyze(REFS()).await;
    assert_eq!(data["stats"]["residual_units"], 1);
    assert_eq!(data["source"]["identity"], "refs:main:feature");
    assert_eq!(data["source"]["min_count"], 2);
    assert!(data["review"].is_object());
    let id = data["id"].as_str().unwrap();
    let again = server.get(&format!("/api/report/{id}")).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["id"], id);
    assert_eq!(again.json(), data);
}

#[tokio::test]
async fn analyze_coerces_numbers_like_python() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let data = server
        .analyze(json!({"base": "main", "head": "feature", "pr": "", "min_count": "3"}))
        .await;
    assert_eq!(data["source"]["min_count"], 3);
    // `min_count or 2`: a zero means the default; max(1, ...) floors it.
    let data = server
        .analyze(json!({"base": "main", "head": "feature", "min_count": 0}))
        .await;
    assert_eq!(data["source"]["min_count"], 2);
    let data = server
        .analyze(json!({"base": "main", "head": "feature", "min_count": -5}))
        .await;
    assert_eq!(data["source"]["min_count"], 1);
    let res = server
        .post("/api/analyze", json!({"base": "main", "pr": "seven"}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "PR number and min count must be integers.");
    let res = server.post_raw("/api/analyze", "{not json").await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.error().starts_with("The request body must be JSON"));
}

#[tokio::test]
async fn analyze_bad_ref_is_400() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let res = server.post("/api/analyze", json!({"base": "nope"})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.error().contains("nope"), "{}", res.error());
    let res = server.post("/api/analyze", json!({})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Choose a base ref to compare against.");
}

#[tokio::test]
async fn unknown_report_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    for path in [
        "/api/report/nope",
        "/api/report/nope/file?path=api.py",
        "/api/report/nope/review",
        "/api/report/nope/summary.md",
        "/api/report/nope/commits",
        "/api/report/nope/source?side=old&path=api.py",
        "/api/report/nope/issues?side=old&path=api.py",
        "/api/report/nope/folds?side=old&path=api.py",
        "/api/report/nope/jev",
    ] {
        let res = server.get(path).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            res.error(),
            "Unknown report; run the analysis again.",
            "{path}"
        );
    }
    for path in [
        "/api/report/nope/review",
        "/api/report/nope/pr/comment",
        "/api/report/nope/pr/review-comment",
        "/api/report/nope/pr/reply",
        "/api/report/nope/pr/review",
        "/api/report/nope/navigate",
        "/api/report/nope/ai/menu",
        "/api/report/nope/ai/refs-count",
        "/api/report/nope/ai/ask",
        "/api/report/nope/jev/score",
    ] {
        let res = server.post(path, json!({})).await;
        assert_eq!(res.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            res.error(),
            "Unknown report; run the analysis again.",
            "{path}"
        );
    }
}

fn pr_info(repo: &Path) -> Value {
    json!({
        "number": 7,
        "title": "Rename get_user",
        "url": "https://example.test/pr/7",
        "baseRefName": "main",
        "headRefName": "feature",
        "baseRefOid": git(repo, &["rev-parse", "main"]),
        "headRefOid": git(repo, &["rev-parse", "feature"]),
    })
}

#[tokio::test]
async fn pr_source_uses_gh() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let server = TestServer::with_github(&repo, FakeGitHub::with_pr(pr_info(&repo)));
    let data = server.analyze(json!({"pr": 7})).await;
    assert_eq!(data["source"]["pr"]["number"], 7);
    assert_eq!(data["source"]["label"], "#7 Rename get_user");
    assert_eq!(data["source"]["identity"], "pr:7");
    assert_eq!(data["stats"]["residual_units"], 1);
    assert_eq!(data["review"]["identity"], "pr:7");

    let res = server.post("/api/analyze", json!({"pr": 8})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.error().contains("no pull requests found for 8"));
}

#[tokio::test]
async fn pr_comments_are_posted_through_github() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let github = FakeGitHub::with_pr(pr_info(&repo));
    let server = TestServer::with_github(&repo, github.clone());
    let report = server.analyze(json!({"pr": 7})).await;
    let id = report["id"].as_str().unwrap();

    let res = server
        .post(
            &format!("/api/report/{id}/pr/comment"),
            json!({"body": "  "}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "The comment is empty.");
    let res = server
        .post(
            &format!("/api/report/{id}/pr/comment"),
            json!({"body": "LGTM"}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json()["url"],
        "https://example.test/pr/7#issuecomment-1"
    );
    assert_eq!(
        *github.comments.lock().unwrap(),
        vec![(7, "LGTM".to_string())]
    );

    let res = server
        .post(
            &format!("/api/report/{id}/pr/review-comment"),
            json!({"body": "x"}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "A comment and a hunk are required.");
    let hunk = report["hunks"]
        .as_object()
        .unwrap()
        .values()
        .find(|h| h["path"] == "api.py")
        .unwrap();
    let hunk_id = hunk["id"].as_str().unwrap();
    let res = server
        .post(
            &format!("/api/report/{id}/pr/review-comment"),
            json!({"body": "why?", "hunk_id": hunk_id}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let posted = res.json();
    assert_eq!(posted["path"], "api.py");
    assert_eq!(posted["side"], "RIGHT");
    // The anchor is the first added line of a unit that still needs review (else of any
    // changed line).
    let changed: Vec<&Value> = hunk["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["type"] != " ")
        .collect();
    let residual: Vec<&Value> = changed
        .iter()
        .copied()
        .filter(|l| {
            l["unit"]
                .as_str()
                .is_some_and(|u| report["units"][u]["explained"] == false)
        })
        .collect();
    let pool = if residual.is_empty() {
        &changed
    } else {
        &residual
    };
    let anchor = pool.iter().find(|l| l["type"] == "+").unwrap_or(&pool[0]);
    assert_eq!(posted["line"], anchor["new_no"]);
    {
        let recorded = github.review_comments.lock().unwrap();
        let (number, payload) = &recorded[0];
        assert_eq!(*number, 7);
        assert_eq!(payload.body, "why?");
        assert_eq!(payload.commit_id, report["source"]["head_sha"]);
        assert_eq!(payload.side, "RIGHT");
    }

    let res = server
        .post(
            &format!("/api/report/{id}/pr/review-comment"),
            json!({"body": "old side", "hunk_id": hunk_id, "side": "LEFT", "line": "x"}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "line must be an integer.");
}

#[tokio::test]
async fn comments_need_a_pull_request() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let id = server.analyze(REFS()).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    for route in [
        "pr/comment",
        "pr/review-comment",
        "pr/reply",
        "pr/resolve",
        "pr/comment/edit",
        "pr/comment/delete",
        "pr/draft/comment",
        "pr/draft/comment/delete",
        "pr/draft/discard",
        "pr/review",
    ] {
        let res = server
            .post(&format!("/api/report/{id}/{route}"), json!({"body": "x"}))
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{route}");
        assert_eq!(res.error(), "This report isn't a pull request.", "{route}");
    }
    for route in ["pr/conversation", "pr/draft"] {
        let res = server.get(&format!("/api/report/{id}/{route}")).await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{route}");
    }
}

/// The first hunk of `path` in a report, and its first added line's number.
fn hunk_in<'a>(report: &'a Value, path: &str) -> (&'a str, i64) {
    let hunk = report["hunks"]
        .as_object()
        .unwrap()
        .values()
        .find(|h| h["path"] == path)
        .unwrap();
    let added = hunk["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["type"] == "+")
        .unwrap();
    (
        hunk["id"].as_str().unwrap(),
        added["new_no"].as_i64().unwrap(),
    )
}

#[tokio::test]
async fn pr_conversation_maps_threads_to_hunks() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let github = FakeGitHub::with_pr(pr_info(&repo));
    let server = TestServer::with_github(&repo, github.clone());
    let report = server.analyze(json!({"pr": 7})).await;
    let id = report["id"].as_str().unwrap();
    let (hunk_id, line) = hunk_in(&report, "api.py");
    *github.conversation.lock().unwrap() = serde_json::from_value(json!({
        "number": 7, "url": "u", "viewer": "me", "viewer_is_author": false, "author": "them",
        "review_decision": null, "head_sha": "x", "comments": [], "reviews": [],
        "pending_review": null,
        "threads": [
            {"id": "T1", "path": "api.py", "line": line, "original_line": line, "start_line": null,
             "side": "RIGHT", "outdated": false, "resolved": false, "resolved_by": null,
             "can_resolve": true, "can_unresolve": false, "can_reply": true, "comments": []},
            {"id": "T2", "path": "api.py", "line": null, "original_line": 1, "start_line": null,
             "side": "RIGHT", "outdated": true, "resolved": false, "resolved_by": null,
             "can_resolve": true, "can_unresolve": false, "can_reply": true, "comments": []},
            {"id": "T3", "path": "elsewhere.py", "line": 3, "original_line": 3, "start_line": null,
             "side": "RIGHT", "outdated": false, "resolved": true, "resolved_by": "me",
             "can_resolve": false, "can_unresolve": true, "can_reply": true, "comments": []}
        ]
    }))
    .unwrap();
    let res = server
        .get(&format!("/api/report/{id}/pr/conversation"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let conv = res.json();
    let hunks: Vec<&Value> = conv["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["hunk_id"])
        .collect();
    assert_eq!(hunks, [&json!(hunk_id), &Value::Null, &Value::Null]);
    assert_eq!(conv["draft"]["comments"], json!([]));
}

#[tokio::test]
async fn thread_and_comment_actions_go_through_github() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let github = FakeGitHub::with_pr(pr_info(&repo));
    let server = TestServer::with_github(&repo, github.clone());
    let id = server.analyze(json!({"pr": 7})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let post = |route: &str, body: Value| {
        let path = format!("/api/report/{id}/pr/{route}");
        let server = &server;
        async move { server.post(&path, body).await }
    };

    let res = post("reply", json!({"comment_id": 21, "body": " "})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = post("reply", json!({"comment_id": "21", "body": "done"})).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        *github.replies.lock().unwrap(),
        vec![(7, 21, "done".to_string())]
    );

    let res = post("resolve", json!({"thread_id": "T1", "resolved": true})).await;
    assert_eq!(res.status, StatusCode::OK);
    post("resolve", json!({"thread_id": "T1", "resolved": false})).await;
    assert_eq!(
        *github.resolved.lock().unwrap(),
        vec![("T1".to_string(), true), ("T1".to_string(), false)]
    );
    assert_eq!(
        post("resolve", json!({})).await.error(),
        "A thread is required."
    );

    let res = post(
        "comment/edit",
        json!({"kind": "review", "id": 5, "body": "better"}),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let res = post(
        "comment/edit",
        json!({"kind": "nope", "id": 5, "body": "x"}),
    )
    .await;
    assert_eq!(res.error(), "kind must be \"issue\" or \"review\".");
    let res = post("comment/delete", json!({"kind": "issue", "id": 9})).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(github.edits.lock().unwrap().len(), 1);
    assert_eq!(github.deletes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn pending_review_is_kept_and_submitted() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let repo = rename_repo(tmp.path());
    let github = FakeGitHub::with_pr(pr_info(&repo));
    let server = TestServer::with_github_state(&repo, github.clone(), &state);
    let report = server.analyze(json!({"pr": 7})).await;
    let id = report["id"].as_str().unwrap();
    let (hunk_id, _) = hunk_in(&report, "api.py");
    let url = |route: &str| format!("/api/report/{id}/pr/{route}");

    let res = server
        .post(
            &url("draft/comment"),
            json!({"body": "one", "hunk_id": hunk_id}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let draft = res.json();
    // Same anchor as a single review comment on the hunk.
    let line = draft["comments"][0]["line"].as_i64().unwrap();
    let single = server
        .post(
            &url("review-comment"),
            json!({"body": "x", "hunk_id": hunk_id}),
        )
        .await
        .json();
    assert_eq!(single["line"], line);
    assert_eq!(draft["comments"][0]["side"], "RIGHT");
    assert_eq!(draft["head_sha"], report["source"]["head_sha"]);
    server
        .post(
            &url("draft/comment"),
            json!({"body": "two", "hunk_id": hunk_id}),
        )
        .await;
    let res = server
        .post(&url("draft/comment"), json!({"id": "d1", "body": "uno"}))
        .await;
    assert_eq!(res.json()["comments"][0]["body"], "uno");
    let res = server
        .post(&url("draft/comment"), json!({"id": "d9", "body": "x"}))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    let res = server
        .post(&url("draft/comment/delete"), json!({"id": "d2"}))
        .await;
    assert_eq!(res.json()["comments"].as_array().unwrap().len(), 1);

    // Another server on the same state still has the draft.
    let again = TestServer::with_github_state(&repo, github.clone(), &state);
    let id2 = again.analyze(json!({"pr": 7})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = again.get(&format!("/api/report/{id2}/pr/draft")).await;
    assert_eq!(res.json()["comments"][0]["body"], "uno");

    let res = server.post(&url("review"), json!({"event": "MERGE"})).await;
    assert_eq!(
        res.error(),
        "event must be COMMENT, APPROVE or REQUEST_CHANGES."
    );
    let res = server
        .post(&url("review"), json!({"event": "REQUEST_CHANGES"}))
        .await;
    assert!(res.error().contains("needs a comment"), "{}", res.error());

    github.conversation.lock().unwrap().viewer_is_author = true;
    let res = server
        .post(&url("review"), json!({"event": "APPROVE"}))
        .await;
    assert!(
        res.error().contains("your own pull request"),
        "{}",
        res.error()
    );
    github.conversation.lock().unwrap().viewer_is_author = false;
    github.conversation.lock().unwrap().pending_review = Some(Default::default());
    let res = server
        .post(&url("review"), json!({"event": "APPROVE"}))
        .await;
    assert!(res.error().contains("pending review"), "{}", res.error());
    github.conversation.lock().unwrap().pending_review = None;
    assert!(github.reviews.lock().unwrap().is_empty());

    let res = server
        .post(&url("review"), json!({"event": "APPROVE", "body": "nice"}))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["comments"], 1);
    {
        let reviews = github.reviews.lock().unwrap();
        let (number, payload) = &reviews[0];
        assert_eq!(*number, 7);
        assert_eq!(payload.event, "APPROVE");
        assert_eq!(payload.body, "nice");
        assert_eq!(payload.commit_id, report["source"]["head_sha"]);
        assert_eq!(payload.comments[0].body, "uno");
        assert_eq!(payload.comments[0].line, line);
    }
    let res = server.get(&url("draft")).await;
    assert_eq!(
        res.json()["comments"],
        json!([]),
        "submitting clears the draft"
    );

    let res = server
        .post(&url("review"), json!({"event": "COMMENT"}))
        .await;
    assert!(res.error().contains("Nothing to submit"), "{}", res.error());
    server
        .post(
            &url("draft/comment"),
            json!({"body": "z", "hunk_id": hunk_id}),
        )
        .await;
    let res = server.post(&url("draft/discard"), json!({})).await;
    assert_eq!(res.json()["comments"], json!([]));
}

#[tokio::test]
async fn file_diff_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let server = TestServer::new(&repo);
    let report = server.analyze(REFS()).await;
    let id = report["id"].as_str().unwrap();
    let res = server
        .get(&format!("/api/report/{id}/file?path=api.py"))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let data = res.json();
    let lines = data["lines"].as_array().unwrap();
    let old: Vec<&str> = lines
        .iter()
        .filter(|l| l["t"] != "+")
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    let new: Vec<&str> = lines
        .iter()
        .filter(|l| l["t"] != "-")
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    let main_api = git(&repo, &["show", "main:api.py"]);
    assert_eq!(old, main_api.lines().collect::<Vec<_>>());
    let new_api = std::fs::read_to_string(repo.join("api.py")).unwrap();
    assert_eq!(new, new_api.lines().collect::<Vec<_>>());
    assert_eq!(old.len() as u64, data["old_lines"].as_u64().unwrap());
    assert_eq!(new.len() as u64, data["new_lines"].as_u64().unwrap());

    // Changed lines point at the report's units and carry their highlights.
    let changed: Vec<&Value> = lines.iter().filter(|l| l["t"] != " ").collect();
    assert!(!changed.is_empty());
    let units = report["units"].as_object().unwrap();
    assert!(
        changed
            .iter()
            .all(|l| units.contains_key(l["unit"].as_str().unwrap_or("")))
    );
    let first_add = changed.iter().find(|l| l["t"] == "+").unwrap();
    assert_eq!(first_add["text"], "from users import fetch_user");
    assert_eq!(first_add["hl"], json!([[18, 28]]));
}

#[tokio::test]
async fn file_diff_unknown_path_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let id = server.analyze(REFS()).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = server
        .get(&format!("/api/report/{id}/file?path=nope.py"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.error(), "That file is not part of this diff.");
}

#[tokio::test]
async fn source_endpoint_reads_either_side() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = rename_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = server.analyze(REFS()).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = server
        .get(&format!("/api/report/{id}/source?side=old&path=api.py"))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let data = res.json();
    assert_eq!(data["path"], "api.py");
    assert_eq!(data["side"], "old");
    let main_api = git(&repo, &["show", "main:api.py"]);
    assert_eq!(data["lines"], json!(main_api.lines().collect::<Vec<_>>()));

    let res = server
        .get(&format!(
            "/api/report/{id}/source?side=sideways&path=api.py"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "side must be old or new.");
    let res = server
        .get(&format!("/api/report/{id}/source?side=new&path=nope.py"))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.error(), "nope.py doesn't exist in the new version.");
    let res = server
        .get(&format!("/api/report/{id}/source?side=old&path=nope.py"))
        .await;
    assert_eq!(
        res.error(),
        "nope.py doesn't exist in the original version."
    );
}

#[tokio::test]
async fn summary_and_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let report = server.analyze(REFS()).await;
    let id = report["id"].as_str().unwrap();
    let res = server.get(&format!("/api/report/{id}/summary.md")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.content_type, "text/markdown; charset=utf-8");
    assert!(res.text().starts_with("# "), "{}", res.text());
    assert!(res.text().contains("main...feature"));

    let res = server.get(&format!("/api/report/{id}/commits")).await;
    assert_eq!(res.status, StatusCode::OK);
    let commits = res.json()["commits"].as_array().unwrap().clone();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["subject"], "after");
    assert_eq!(commits[0]["sha"], report["source"]["head_sha"]);
}

fn padded_base() -> String {
    let pad = (0..10)
        .map(|i| format!("line_{i} = {i}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    format!("{pad}\n\ndef f():\n    return 1\n\n\ndef g():\n    return 2\n\n\n{pad}")
}

#[tokio::test]
async fn issues_are_served_per_side() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    commit(
        &repo,
        &[("a.py", Some("def f():\n    return 1\n"))],
        "before",
    );
    git(&repo, &["checkout", "-qb", "feature"]);
    let after = "import os\n\ndef f():\n    tmp = 1\n    return 1\n    print()\n";
    commit(&repo, &[("a.py", Some(after))], "after");

    let server = TestServer::new(&repo);
    let data = server.analyze(REFS()).await;
    let id = data["id"].as_str().unwrap();
    let old = server
        .get(&format!("/api/report/{id}/issues?side=old&path=a.py"))
        .await
        .json();
    assert_eq!(old, json!({"path": "a.py", "side": "old", "issues": []}));
    let new = server
        .get(&format!("/api/report/{id}/issues?side=new&path=a.py"))
        .await
        .json();
    let issues = new["issues"].as_array().unwrap();
    let codes: Vec<(&str, &str)> = issues
        .iter()
        .map(|i| (i["code"].as_str().unwrap(), i["severity"].as_str().unwrap()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("unused-import", "info"),
            ("unused-variable", "warning"),
            ("unreachable", "warning"),
        ]
    );
    assert_eq!(issues[1]["start"], json!({"line": 4, "col": 4}));
    assert_eq!(issues[1]["end"], json!({"line": 4, "col": 7}));
    // Refactor warnings stay separate: code issues never reach the report.
    assert_eq!(data["warnings"], json!([]));

    let bad = server
        .get(&format!("/api/report/{id}/issues?side=mid&path=a.py"))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let missing = server
        .get(&format!("/api/report/{id}/issues?side=old&path=nope.py"))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn folds_are_served_per_side() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    commit(&repo, &[("a.py", Some("x = 1\n"))], "before");
    git(&repo, &["checkout", "-qb", "feature"]);
    let after = "def f():\n    if x:\n        return 1\n    return 2\n";
    commit(&repo, &[("a.py", Some(after))], "after");

    let server = TestServer::new(&repo);
    let data = server.analyze(REFS()).await;
    let id = data["id"].as_str().unwrap();
    let old = server
        .get(&format!("/api/report/{id}/folds?side=old&path=a.py"))
        .await
        .json();
    assert_eq!(old, json!({"path": "a.py", "side": "old", "folds": []}));
    let new = server
        .get(&format!("/api/report/{id}/folds?side=new&path=a.py"))
        .await
        .json();
    assert_eq!(
        new["folds"],
        json!([{"start": 1, "end": 4}, {"start": 2, "end": 3}])
    );

    let bad = server
        .get(&format!("/api/report/{id}/folds?side=mid&path=a.py"))
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let missing = server
        .get(&format!("/api/report/{id}/folds?side=old&path=nope.py"))
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn review_marks_survive_new_commits_via_the_api() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let base = padded_base();
    commit(&repo, &[("a.py", Some(&base))], "before");
    git(&repo, &["checkout", "-qb", "feature"]);
    commit(
        &repo,
        &[("a.py", Some(&base.replace("return 2", "return 3")))],
        "after",
    );

    let server = TestServer::new(&repo);
    let data = server.analyze(REFS()).await;
    let hunks = fingerprints(&data);
    assert_eq!(hunks.len(), 1);
    let fp = hunks.keys().next().unwrap().clone();
    assert_eq!(
        data["review"],
        json!({
            "identity": "refs:main:feature",
            "groups": [],
            "hunks": [],
            "delta": {"prev_head": null, "new": [], "changed_reviewed": []},
        })
    );
    let id = data["id"].as_str().unwrap();
    let res = server
        .post(
            &format!("/api/report/{id}/review"),
            json!({"hunks": {"add": [fp]}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(res.json()["hunks"], json!([fp]));
    assert_eq!(
        server.get(&format!("/api/report/{id}/review")).await.json()["hunks"],
        json!([fp])
    );

    // A new commit that only shifts the hunk down keeps the mark; its new siblings are "new".
    let shifted = format!(
        "import os\n\n{}\n\nX = 1\n",
        base.replace("return 2", "return 3")
    );
    commit(&repo, &[("a.py", Some(&shifted))], "edit");
    let data2 = server.analyze(REFS()).await;
    let fps2 = fingerprints(&data2);
    assert!(fps2.contains_key(&fp) && fps2.len() == 3);
    assert_eq!(data2["review"]["hunks"], json!([fp]));
    let mut new: Vec<String> = data2["review"]["delta"]["new"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    new.sort();
    let mut expected: Vec<String> = fps2.keys().filter(|k| **k != fp).cloned().collect();
    expected.sort();
    assert_eq!(new, expected);
    assert_eq!(
        data2["review"]["delta"]["prev_head"],
        data["source"]["head_sha"]
    );

    // Editing the reviewed hunk itself drops the mark and reports it as changed.
    let edited = format!(
        "import os\n\n{}\n\nX = 1\n",
        base.replace("return 2", "return 4")
    );
    commit(&repo, &[("a.py", Some(&edited))], "edit");
    let data3 = server.analyze(REFS()).await;
    assert_eq!(data3["review"]["hunks"], json!([]));
    assert_eq!(
        data3["review"]["delta"]["changed_reviewed"],
        json!([{"fingerprint": fp, "path": "a.py"}])
    );

    // Another server process reads the same state (and knows nothing about our reports).
    let other = TestServer::with_state_dir(&repo, &server.state_dir);
    let id3 = data3["id"].as_str().unwrap();
    assert_eq!(
        other.get(&format!("/api/report/{id3}/review")).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(server.state_dir.join("reviews").exists());
    // Re-analyzing the same head keeps reporting the same delta (against the head before it).
    let data4 = other.analyze(REFS()).await;
    assert_eq!(
        data4["review"]["delta"]["prev_head"],
        data2["source"]["head_sha"]
    );
    assert_eq!(data4["review"]["delta"], data3["review"]["delta"]);
}

#[tokio::test]
async fn bad_review_body_is_400() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let id = server.analyze(REFS()).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = server
        .post(&format!("/api/report/{id}/review"), json!({"hunks": ["x"]}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "hunks must be {add: [], remove: []}.");
    let res = server
        .post(
            &format!("/api/report/{id}/review"),
            json!({"groups": {"add": "g1"}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "groups must be {add: [], remove: []}.");
    // Group marks are free-form ids; an empty change is fine. Removals apply before additions.
    let res = server
        .post(
            &format!("/api/report/{id}/review"),
            json!({"groups": {"add": ["g1", "g2"], "remove": ["g1"]}, "hunks": {}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["groups"], json!(["g1", "g2"]));
    let res = server
        .post(
            &format!("/api/report/{id}/review"),
            json!({"groups": {"remove": ["g1"]}}),
        )
        .await;
    assert_eq!(res.json()["groups"], json!(["g2"]));
}

#[tokio::test]
async fn settings_round_trip_masks_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let view = server.get("/api/settings").await.json();
    assert_eq!(view["ai"]["provider"], "claude");
    assert_eq!(view["ai"]["providers"]["claude"]["api_key"], "");
    assert_eq!(view["ai"]["active"]["configured"], false);

    let res = server
        .post(
            "/api/settings",
            json!({"ai": {"providers": {"claude": {"api_key": "sk-ant-secret-1234"}}}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let saved = res.json();
    assert_eq!(saved["ai"]["providers"]["claude"]["api_key"], "••••1234");
    assert_eq!(saved["ai"]["providers"]["claude"]["configured"], true);
    assert!(!res.text().contains("sk-ant"));
    let on_disk = std::fs::read_to_string(server.state_dir.join("settings.json")).unwrap();
    assert!(on_disk.contains("sk-ant-secret-1234"));

    // Posting the masked value back keeps the stored key.
    let res = server
        .post(
            "/api/settings",
            json!({"ai": {"provider": "ollama", "providers": {"claude": {"api_key": "••••1234", "model": "x"}}}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["ai"]["provider"], "ollama");
    assert_eq!(res.json()["ai"]["providers"]["claude"]["model"], "x");
    let on_disk = std::fs::read_to_string(server.state_dir.join("settings.json")).unwrap();
    assert!(on_disk.contains("sk-ant-secret-1234"));
    assert_eq!(server.get("/api/settings").await.json(), res.json());

    let res = server
        .post("/api/settings", json!({"ai": {"provider": "cursor"}}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Unknown provider 'cursor'.");
    let res = server.post("/api/settings", json!({"nothing": 1})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        res.error(),
        "Expected an object with an 'ai' or 'jev' section."
    );
}

#[tokio::test]
async fn settings_test_reports_failure_without_a_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    // Without a factory override the real build runs and reports the missing key.
    let res = server
        .post(
            "/api/settings/test",
            json!({"provider": "claude", "config": {}}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK);
    let data = res.json();
    assert_eq!(data["ok"], false);
    assert_eq!(data["latency_ms"], Value::Null);
    assert_eq!(data["models"], json!([]));
    assert!(data["error"].as_str().unwrap().contains("API key"));

    let res = server
        .post("/api/settings/test", json!({"provider": "cursor"}))
        .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["ok"], false);
    assert_eq!(res.json()["error"], "Unknown provider 'cursor'.");
}

#[tokio::test]
async fn navigation_and_ai_validate_the_request() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let id = server.analyze(REFS()).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let nav = format!("/api/report/{id}/navigate");
    let res = server
        .post(
            &nav,
            json!({"action": "definition", "side": "new", "line": "x", "col": 1}),
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "line and col must be integers.");
    let res = server
        .post(
            &nav,
            json!({"action": "jump", "side": "new", "line": 1, "col": 1}),
        )
        .await;
    assert_eq!(res.error(), "Unknown action or side.");
    let res = server
        .post(
            &nav,
            json!({"action": "definition", "side": "new", "line": 1, "col": 1, "path": "api.py"}),
        )
        .await;
    // A well-formed request reaches the navigator (its answers are covered by the
    // navigation tests).
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());

    let menu = format!("/api/report/{id}/ai/menu");
    let res = server.post(&menu, json!({"line": "x"})).await;
    assert_eq!(res.error(), "line must be an integer.");
    let res = server.post(&menu, json!({"line": ""})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Unknown hunk; run the analysis again.");

    let ask = format!("/api/report/{id}/ai/ask");
    let res = server.post(&ask, json!({"task": "nope"})).await;
    assert_eq!(res.error(), "Unknown task.");
    let res = server.post(&ask, json!({"task": "explain"})).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "Unknown hunk; run the analysis again.");

    // A library file no navigation result pointed at is a 404, as in Python.
    let res = server.get("/api/library?path=x.py").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serves_over_tcp_and_cleans_up_on_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let TestServer { app, .. } = server;
    let snapshot_dir = app.state().snapshots.dir().to_path_buf();
    assert!(snapshot_dir.is_dir());
    let handle = app.handle();
    let listener = refactor_diff_server::bind_local(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let serving = tokio::spawn(app.serve(listener, std::future::pending()));

    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /api/config HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.contains("\"defaults\":{}"), "{text}");

    assert!(!handle.is_stopped());
    handle.shutdown();
    handle.stopped().await;
    serving.await.unwrap().unwrap();
    assert!(!snapshot_dir.exists(), "snapshot dir survived shutdown");
}

#[tokio::test]
async fn analyze_with_a_concept_is_a_separate_report() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::new(&rename_repo(tmp.path()));
    let plain = server.analyze(REFS()).await;
    let blank = server
        .analyze(json!({"base": "main", "head": "feature", "concept": "  "}))
        .await;
    assert_eq!(blank["id"], plain["id"]);
    let foo = server
        .analyze(json!({"base": "main", "head": "feature", "concept": "Foo"}))
        .await;
    assert_ne!(foo["id"], plain["id"]);
    let lower = server
        .analyze(json!({"base": "main", "head": "feature", "concept": " foo "}))
        .await;
    assert_eq!(lower["id"], foo["id"]);
}

#[tokio::test]
async fn config_exposes_the_default_concept() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::with_config(&rename_repo(tmp.path()), |c| {
        c.defaults.concept = Some("program".into());
    });
    let cfg = server.get("/api/config").await.json();
    assert_eq!(cfg["defaults"], json!({"concept": "program"}));
}
