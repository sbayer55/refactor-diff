//! UI preferences, the settings page and the settings-only server (the desktop app's
//! Settings window): the API parts of the Python tests `test_server.py` added for them.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

#[test]
fn prefs_round_trip_through_config() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let repo = fixture_repo("rename_project", &dir.path().join("repo"));
        let server = TestServer::new(&repo);
        let cfg = server.get("/api/config").await.json();
        assert_eq!(cfg["prefs"], json!({}));
        assert_eq!(cfg["desktop"], json!(false));
        let res = server
            .post(
                "/api/prefs",
                json!({"changes": {"refactor-diff:layout": "split"}}),
            )
            .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        assert_eq!(
            res.json(),
            json!({"prefs": {"refactor-diff:layout": "split"}})
        );
        assert!(server.state_dir.join("ui.json").exists());
        assert_eq!(
            server.get("/api/config").await.json()["prefs"],
            json!({"refactor-diff:layout": "split"})
        );
        let bad = server
            .post("/api/prefs", json!({"changes": {"x": "y"}}))
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            bad.error(),
            "Preference keys must start with 'refactor-diff:'."
        );
        let not_object = server.post("/api/prefs", json!({"changes": ["a"]})).await;
        assert_eq!(
            not_object.error(),
            "Expected an object of preference changes."
        );
    });
}

#[test]
fn desktop_flag_reaches_config_and_settings_page_is_served() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let repo = fixture_repo("rename_project", &dir.path().join("repo"));
        let server = TestServer::with_config(&repo, |c| c.desktop = true);
        assert_eq!(
            server.get("/api/config").await.json()["desktop"],
            json!(true)
        );
        let page = server.get("/settings").await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.content_type.starts_with("text/html"));
        assert!(page.text().contains("settings.js"));
    });
}

#[test]
fn settings_only_server() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        // The repo is ignored: a directory that is not a repository builds fine.
        let server = TestServer::with_config(dir.path(), |c| {
            c.settings_only = true;
            c.desktop = true;
        });
        assert_eq!(
            server.get("/api/config").await.json(),
            json!({"desktop": true, "settings_only": true})
        );
        let root = server.get("/").await;
        assert!(root.status.is_redirection(), "{}", root.status);
        assert_eq!(root.location.as_deref(), Some("/settings"));
        assert!(server.get("/settings").await.text().contains("settings.js"));
        assert_eq!(
            server.get("/static/settings.js").await.status,
            StatusCode::OK
        );
        let view = server.get("/api/settings").await.json();
        assert_eq!(view["ai"]["provider"], json!("claude"));
        let saved = server
            .post("/api/settings", json!({"ai": {"provider": "ollama"}}))
            .await
            .json();
        assert_eq!(saved["ai"]["provider"], json!("ollama"));
        let prefs = server
            .post("/api/prefs", json!({"changes": {"refactor-diff:a": "1"}}))
            .await;
        assert_eq!(prefs.status, StatusCode::OK);
        assert_eq!(
            server.get("/api/report/x").await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            server.get("/api/sources").await.status,
            StatusCode::NOT_FOUND
        );
    });
}
