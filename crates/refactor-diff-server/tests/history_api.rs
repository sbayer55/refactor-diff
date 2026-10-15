//! The file-history API: commits that touched a file (following renames), content, diffs
//! and blame at any version, and the two checkout actions.

mod common;

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use common::{TestServer, commit, git};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

/// `a.py` is added, edited, renamed to `b.py` and edited again; `README.txt` never changes.
fn history_repo(tmp: &Path) -> (PathBuf, [String; 4]) {
    let repo = tmp.join("repo");
    let c1 = commit(
        &repo,
        &[("a.py", Some("one\ntwo\n")), ("README.txt", Some("hi\n"))],
        "add a",
    );
    let c2 = commit(&repo, &[("a.py", Some("one\ntwo\nthree\n"))], "edit a");
    let c3 = commit(
        &repo,
        &[("a.py", None), ("b.py", Some("one\ntwo\nthree\n"))],
        "rename to b",
    );
    let c4 = commit(
        &repo,
        &[("b.py", Some("one\ntwo\nthree\nfour\n"))],
        "edit b",
    );
    (repo, [c1, c2, c3, c4])
}

async fn report_id(server: &TestServer, base: &str, head: &str) -> String {
    server.analyze(json!({"base": base, "head": head})).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn shas(commits: &Value) -> Vec<&str> {
    commits
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["sha"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn list_follows_renames_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let res = server
        .get(&format!("/api/report/{id}/history/commits?path=b.py"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let data = res.json();
    assert_eq!(data["path"], "b.py");
    assert_eq!(data["head"], c4);
    assert_eq!(data["base"], c1);
    assert_eq!(data["worktree"], false);
    assert_eq!(data["more"], false);
    let commits = &data["commits"];
    assert_eq!(shas(commits), vec![&c4, &c3, &c2, &c1]);
    let paths: Vec<&str> = commits
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["b.py", "b.py", "a.py", "a.py"]);
    assert_eq!(commits[0]["is_head"], true);
    assert_eq!(commits[1]["is_head"], false);
    assert_eq!(commits[3]["is_base"], true);
    assert_eq!(commits[0]["is_base"], false);
    assert_eq!(commits[0]["in_range"], true);
    assert_eq!(commits[3]["in_range"], false);
    assert_eq!(commits[0]["insertions"], 1);
    assert_eq!(commits[0]["deletions"], 0);
    assert_eq!(commits[1]["subject"], "rename to b");
    assert_eq!(commits[3]["insertions"], 2);
    assert_eq!(commits[0]["short"].as_str().unwrap().len(), 7);

    // A file outside the report has a history too.
    let data = server
        .get(&format!("/api/report/{id}/history/commits?path=README.txt"))
        .await
        .json();
    assert_eq!(shas(&data["commits"]), vec![&c1]);
    let res = server
        .get(&format!(
            "/api/report/{id}/history/commits?path=../etc/passwd"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_paginates() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let data = server
        .get(&format!(
            "/api/report/{id}/history/commits?path=b.py&limit=2"
        ))
        .await
        .json();
    assert_eq!(shas(&data["commits"]), vec![&c4, &c3]);
    assert_eq!(data["more"], true);
    let data = server
        .get(&format!(
            "/api/report/{id}/history/commits?path=b.py&offset=2&limit=2"
        ))
        .await
        .json();
    assert_eq!(shas(&data["commits"]), vec![&c2, &c1]);
    assert_eq!(data["more"], false);
    let data = server
        .get(&format!(
            "/api/report/{id}/history/commits?path=b.py&offset=99"
        ))
        .await
        .json();
    assert_eq!(data["commits"], json!([]));
    assert_eq!(data["more"], false);
    let res = server
        .get(&format!(
            "/api/report/{id}/history/commits?path=b.py&limit=5000"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "limit must be between 1 and 1000.");
}

#[tokio::test]
async fn content_at_any_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, _c2, _c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let data = server
        .get(&format!(
            "/api/report/{id}/history/content?sha={c1}&path=a.py"
        ))
        .await
        .json();
    assert_eq!(data["lines"], json!(["one", "two"]));
    assert_eq!(data["sha"], c1);
    let res = server
        .get(&format!(
            "/api/report/{id}/history/content?sha={c4}&path=a.py"
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.error(), format!("a.py doesn't exist at {}.", &c4[..7]));
    let res = server
        .get(&format!(
            "/api/report/{id}/history/content?sha=zz&path=a.py"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "zz is not a commit sha.");
    // A short sha works, and the result is served from the cache the second time.
    let data = server
        .get(&format!(
            "/api/report/{id}/history/content?sha={}&path=b.py",
            &c4[..8]
        ))
        .await
        .json();
    assert_eq!(data["lines"].as_array().unwrap().len(), 4);

    std::fs::write(repo.join("blob.bin"), b"a\0b").unwrap();
    let c5 = commit(&repo, &[], "binary");
    let res = server
        .get(&format!(
            "/api/report/{id}/history/content?sha={c5}&path=blob.bin"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), format!("blob.bin is binary at {}.", &c5[..7]));
    let res = server
        .get(&format!(
            "/api/report/{id}/history/content?sha=:worktree:&path=b.py"
        ))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn diff_returns_opcodes_hunks_and_stats() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let data = server
        .get(&format!(
            "/api/report/{id}/history/diff?path=b.py&from={c1}&to={c4}&from_path=a.py"
        ))
        .await
        .json();
    assert_eq!(data["from_path"], "a.py");
    assert_eq!(data["to_path"], "b.py");
    assert_eq!(data["from_lines"], 2);
    assert_eq!(data["to_lines"], 4);
    let old: Vec<String> = git(&repo, &["show", &format!("{c1}:a.py")])
        .lines()
        .map(String::from)
        .collect();
    let new: Vec<String> = git(&repo, &["show", &format!("{c4}:b.py")])
        .lines()
        .map(String::from)
        .collect();
    let expected: Vec<Value> = refactor_diff_core::line_matcher(&old, &new)
        .opcodes()
        .iter()
        .map(|op| {
            let tag = match op.tag {
                refactor_diff_core::Tag::Equal => "=",
                refactor_diff_core::Tag::Replace => "~",
                refactor_diff_core::Tag::Delete => "-",
                refactor_diff_core::Tag::Insert => "+",
            };
            json!([tag, op.i1, op.i2, op.j1, op.j2])
        })
        .collect();
    assert_eq!(data["ops"], json!(expected));
    assert_eq!(data["stats"], json!({"add": 2, "del": 0}));
    assert_eq!(data["hunks"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["hunks"][0],
        json!({"a": [2, 2], "b": [2, 4], "del": 0, "add": 2})
    );

    // Identical versions: one equal op, nothing changed.
    let data = server
        .get(&format!(
            "/api/report/{id}/history/diff?path=b.py&from={c2}&to={c3}&from_path=a.py"
        ))
        .await
        .json();
    assert_eq!(data["ops"], json!([["=", 0, 3, 0, 3]]));
    assert_eq!(data["hunks"], json!([]));
}

#[tokio::test]
async fn blame_maps_lines_to_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, _c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let data = server
        .get(&format!(
            "/api/report/{id}/history/blame?sha={c4}&path=b.py"
        ))
        .await
        .json();
    let commits = data["commits"].as_array().unwrap();
    let idx = |sha: &str| {
        commits
            .iter()
            .position(|c| c["sha"] == sha)
            .unwrap_or_else(|| panic!("{sha} missing from {commits:?}")) as u64
    };
    assert_eq!(
        data["lines"],
        json!([idx(&c1), idx(&c1), idx(&c2), idx(&c4)])
    );
    assert_eq!(data["origin"], json!([1, 2, 3, 4]));
    let first = &commits[idx(&c1) as usize];
    assert_eq!(first["path"], "a.py");
    assert_eq!(first["subject"], "add a");
    assert_eq!(first["author"], "t");
    assert_eq!(first["short"], &c1[..7]);
    assert!(first["date"].as_str().unwrap().starts_with("20"));
    let res = server
        .get(&format!(
            "/api/report/{id}/history/blame?sha={c4}&path=nope.py"
        ))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn worktree_report_shows_uncommitted_version() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, _c2, _c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, ":worktree:").await;
    // Clean tree: the HEAD commit is the head.
    let data = server
        .get(&format!("/api/report/{id}/history/commits?path=b.py"))
        .await
        .json();
    assert_eq!(data["worktree"], true);
    assert_eq!(data["commits"][0]["sha"], c4);
    assert_eq!(data["commits"][0]["is_head"], true);

    std::fs::write(repo.join("b.py"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let data = server
        .get(&format!("/api/report/{id}/history/commits?path=b.py"))
        .await
        .json();
    let first = &data["commits"][0];
    assert_eq!(first["sha"], ":worktree:");
    assert_eq!(first["is_head"], true);
    assert_eq!(first["insertions"], 1);
    assert_eq!(first["subject"], "Uncommitted changes");
    assert_eq!(data["commits"][1]["sha"], c4);
    assert_eq!(data["commits"][1]["is_head"], false);

    let data = server
        .get(&format!(
            "/api/report/{id}/history/content?sha=:worktree:&path=b.py"
        ))
        .await
        .json();
    assert_eq!(data["lines"].as_array().unwrap().len(), 5);
    let data = server
        .get(&format!(
            "/api/report/{id}/history/blame?sha=:worktree:&path=b.py"
        ))
        .await
        .json();
    let last = data["lines"][4].as_u64().unwrap() as usize;
    assert_eq!(data["commits"][last]["sha"], ":worktree:");
    assert_eq!(data["commits"][last]["short"], "work");
    let data = server
        .get(&format!(
            "/api/report/{id}/history/diff?path=b.py&from={c4}&to=:worktree:"
        ))
        .await
        .json();
    assert_eq!(data["stats"], json!({"add": 1, "del": 0}));
}

#[tokio::test]
async fn checkout_commit_refuses_dirty_tree_then_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, _c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let url = format!("/api/report/{id}/history/checkout");
    std::fs::write(repo.join("b.py"), "dirty\n").unwrap();
    let res = server
        .post(&url, json!({"sha": c2, "mode": "commit"}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        res.error(),
        format!(
            "The working tree has uncommitted changes; commit or stash them before checking out {}.",
            &c2[..7]
        )
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), c4);

    git(&repo, &["checkout", "--", "b.py"]);
    let res = server
        .post(&url, json!({"sha": &c2[..7], "mode": "commit"}))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let data = res.json();
    assert_eq!(data["ok"], true);
    assert_eq!(data["sha"], c2);
    assert_eq!(data["command"], format!("git checkout {c2}"));
    assert_eq!(data["detached"], true);
    assert_eq!(data["stale"], false);
    assert_eq!(data["head_before"], c4);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), c2);

    let res = server.post(&url, data["undo"].clone()).await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), c4);

    let res = server
        .post(&url, json!({"sha": "zz", "mode": "commit"}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = server
        .post(&url, json!({"sha": "deadbeef", "mode": "commit"}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.error(), "deadbeef is not a commit in this repository.");
    let res = server
        .post(&url, json!({"sha": c2, "mode": "sideways"}))
        .await;
    assert_eq!(res.error(), "mode must be commit or file.");
}

#[tokio::test]
async fn checkout_file_restores_one_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, [c1, c2, _c3, c4]) = history_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = report_id(&server, &c1, "main").await;
    let url = format!("/api/report/{id}/history/checkout");
    let res = server.post(&url, json!({"sha": c2, "mode": "file"})).await;
    assert_eq!(res.error(), "path is required to restore a file.");
    let res = server
        .post(&url, json!({"sha": c2, "mode": "file", "path": "nope.py"}))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    let res = server
        .post(&url, json!({"sha": c2, "mode": "file", "path": "a.py"}))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let data = res.json();
    assert_eq!(data["command"], format!("git checkout {c2} -- a.py"));
    assert_eq!(data["message"], format!("Restored a.py from {}.", &c2[..7]));
    assert_eq!(data["detached"], false);
    assert_eq!(
        std::fs::read_to_string(repo.join("a.py")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), c4);
    assert_eq!(git(&repo, &["status", "--porcelain"]), "A  a.py");

    // The path is now dirty: a second restore needs force.
    let res = server
        .post(&url, json!({"sha": c1, "mode": "file", "path": "a.py"}))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        res.error(),
        "a.py has uncommitted changes; the restore would overwrite them."
    );
    let res = server
        .post(
            &url,
            json!({"sha": c1, "mode": "file", "path": "a.py", "force": true}),
        )
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    assert_eq!(
        std::fs::read_to_string(repo.join("a.py")).unwrap(),
        "one\ntwo\n"
    );
}
