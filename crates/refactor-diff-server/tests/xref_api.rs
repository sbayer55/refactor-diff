//! The xref API: code issues for what a "remove concept X" change left behind. They stay out
//! of the report and its refactor warnings.

mod common;

use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use common::{TestServer, commit};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const API_BASE: &str = "\
export function save(programId: string, name: string) {
  localStorage.setItem('prefs', JSON.stringify({
    programId,
    name,
  }));
}

export async function create(api: Api, programId: string, title: string) {
  return api.post('/items', { programId, title });
}
";

const API_HEAD: &str = "\
export function save(name: string) {
  localStorage.setItem('prefs', JSON.stringify({
    name,
  }));
}

export async function create(api: Api, title: string) {
  return api.post('/items', { title });
}
";

/// Unchanged: still uses `currentProgram` and the deleted `program.title` key.
const VIEW: &str = "\
import { t } from './i18n';
export function View({ currentProgram }: Props) {
  const label = t(\"program.title\");
  const other = t(\"common.ok\");
  return label + currentProgram.name + other;
}
";

const EN_BASE: &str = "\
{
  \"program\": {
    \"title\": \"Program\"
  },
  \"common\": {
    \"ok\": \"OK\",
    \"cancel\": \"Cancel\"
  }
}
";

const EN_HEAD: &str = "\
{
  \"common\": {
    \"ok\": \"OK\",
    \"cancel\": \"Cancel\"
  }
}
";

/// `main` is the base; `feature` drops the program concept but leaves some of it behind.
fn xref_repo(tmp: &Path) -> (PathBuf, String) {
    let repo = tmp.join("repo");
    let base = commit(
        &repo,
        &[
            ("src/api.ts", Some(API_BASE)),
            ("src/view.tsx", Some(VIEW)),
            ("src/i18n/en.json", Some(EN_BASE)),
            (
                "src/header.ts",
                Some("const currentProgram = useProgram();\nexport default currentProgram;\n"),
            ),
            (
                "src/cancel.ts",
                Some("export const c = t(\"common.cancel\");\n"),
            ),
            ("docs/notes.md", Some("# Notes\n\nPass the programId.\n")),
        ],
        "base",
    );
    common::git(&repo, &["checkout", "-qb", "feature"]);
    commit(
        &repo,
        &[
            ("src/api.ts", Some(API_HEAD)),
            ("src/i18n/en.json", Some(EN_HEAD)),
            ("src/header.ts", None),
            ("src/cancel.ts", Some("export const c = \"Cancel\";\n")),
        ],
        "drop programs",
    );
    (repo, base)
}

async fn analyzed(server: &TestServer, base: &str) -> Value {
    server
        .analyze(json!({"base": base, "head": "feature"}))
        .await
}

fn find<'a>(checks: &'a Value, code: &str, path: &str, line: u64) -> &'a Value {
    checks
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == code && c["path"] == path && c["line"] == line)
        .unwrap_or_else(|| panic!("no {code} at {path}:{line} in {checks:#}"))
}

#[tokio::test]
async fn reports_what_the_removal_left_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, base) = xref_repo(tmp.path());
    let server = TestServer::new(&repo);
    let report = analyzed(&server, &base).await;
    let id = report["id"].as_str().unwrap();

    let res = server
        .get(&format!("/api/report/{id}/xref?concept=program"))
        .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let data = res.json();
    assert_eq!(data["concept"], "program");
    assert_eq!(data["truncated"], false);
    let checks = &data["checks"];

    let dangling = find(checks, "xref-dangling", "src/view.tsx", 2);
    assert_eq!(dangling["severity"], "warning");
    assert_eq!(
        dangling["message"],
        "`currentProgram` still referenced after removal"
    );
    assert_eq!(
        find(checks, "xref-dangling", "src/view.tsx", 5)["severity"],
        "warning"
    );
    let docs = find(checks, "xref-dangling", "docs/notes.md", 3);
    assert_eq!(docs["severity"], "info");
    assert_eq!(
        docs["message"],
        "`programId` still mentioned in docs after removal"
    );

    let orphan = find(checks, "xref-i18n-orphan", "src/view.tsx", 3);
    assert_eq!(orphan["severity"], "warning");
    assert_eq!(
        orphan["message"],
        "translation key `program.title` was deleted from src/i18n/en.json but is still used"
    );
    let unused = find(checks, "xref-i18n-orphan", "src/i18n/en.json", 4);
    assert_eq!(unused["severity"], "info");
    assert_eq!(
        unused["message"],
        "translation key `common.cancel` is no longer used: its last use (in src/cancel.ts) was removed"
    );

    let storage = find(checks, "xref-storage-shape", "src/api.ts", 2);
    assert_eq!(storage["severity"], "warning");
    assert_eq!(
        storage["message"],
        "persisted payload drops `programId`; old stored values still carry it"
    );
    let payload = find(checks, "xref-payload-drop", "src/api.ts", 8);
    assert_eq!(payload["severity"], "info");
    assert_eq!(
        payload["message"],
        "request body (api.post '/items') drops `programId` (base line 9)"
    );

    // Warnings come first.
    let severities: Vec<&str> = checks
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["severity"].as_str().unwrap())
        .collect();
    let first_info = severities.iter().position(|s| *s == "info").unwrap();
    assert!(severities[first_info..].iter().all(|s| *s == "info"));
    assert_eq!(data["total"], checks.as_array().unwrap().len());

    // Nothing leaks into the report or its refactor warnings.
    let again = server.get(&format!("/api/report/{id}")).await.json();
    for r in [&report, &again] {
        assert!(!r["warnings"].to_string().contains("xref"));
        assert!(!r.to_string().contains("xref-"));
    }

    // Cached: the same answer again.
    let cached = server
        .get(&format!("/api/report/{id}/xref?concept=program"))
        .await
        .json();
    assert_eq!(cached, data);
}

#[tokio::test]
async fn without_a_concept_param_the_detected_concept_is_used() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, base) = xref_repo(tmp.path());
    let server = TestServer::new(&repo);
    let report = analyzed(&server, &base).await;
    assert_eq!(report["concepts"][0]["stem"], "program");
    let id = report["id"].as_str().unwrap();
    let data = server.get(&format!("/api/report/{id}/xref")).await.json();
    assert_eq!(data["concept"], "program");
    let codes: Vec<&str> = data["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"xref-dangling"), "{codes:?}");
    assert!(codes.contains(&"xref-storage-shape"), "{codes:?}");
}

#[tokio::test]
async fn rejects_bad_concepts_and_unknown_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, base) = xref_repo(tmp.path());
    let server = TestServer::new(&repo);
    let id = analyzed(&server, &base).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let res = server
        .get(&format!("/api/report/{id}/xref?concept=a%20b"))
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = server.get("/api/report/nope/xref?concept=program").await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}
