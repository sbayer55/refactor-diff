//! Shared helpers: fixture git repositories, a fake GitHub and an in-process HTTP client.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use refactor_diff_server::ai::ProviderFactory;
use refactor_diff_server::git::{GitHub, PrInfo, ReviewCommentPayload, SourceError};
use refactor_diff_server::{App, ServerConfig};
use serde_json::Value;
use tower::ServiceExt;

pub const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures");

fn run_git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run git with a fixed identity and return its output.
pub fn git(repo: &Path, args: &[&str]) -> String {
    run_git(repo, args)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            copy_fresh(&entry.path(), &dest);
        }
    }
}

/// Copy a file and give it a fresh mtime, so git never considers the rewritten file "racily
/// clean" when the next commit follows within the same second as the previous one.
fn copy_fresh(from: &Path, to: &Path) {
    std::fs::copy(from, to).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(to).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    f.set_modified(later).unwrap();
}

/// A git repo with `main` (before/) and `feature` (after/) branches, like
/// `tests/conftest.py::_fixture_repo`.
pub fn fixture_repo(project: &str, repo: &Path) -> PathBuf {
    let project = Path::new(FIXTURES).join(project);
    copy_tree(&project.join("before"), repo);
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-qm", "before"]);
    run_git(repo, &["checkout", "-qb", "feature"]);
    for entry in std::fs::read_dir(project.join("after")).unwrap() {
        let entry = entry.unwrap();
        copy_fresh(&entry.path(), &repo.join(entry.file_name()));
    }
    run_git(repo, &["commit", "-qam", "after"]);
    repo.to_path_buf()
}

/// Write `files` (`None` deletes), commit everything and return the commit sha. Creates the
/// repository on first use.
pub fn commit(repo: &Path, files: &[(&str, Option<&str>)], message: &str) -> String {
    if !repo.join(".git").exists() {
        std::fs::create_dir_all(repo).unwrap();
        run_git(repo, &["init", "-q", "-b", "main"]);
    }
    for (path, text) in files {
        let file = repo.join(path);
        match text {
            None => std::fs::remove_file(&file).unwrap(),
            Some(text) => {
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, text).unwrap();
            }
        }
    }
    run_git(repo, &["add", "-A"]);
    run_git(repo, &["commit", "-qm", message]);
    run_git(repo, &["rev-parse", "HEAD"])
}

/// A GitHub that answers from canned data and records what was posted.
#[derive(Default)]
pub struct FakeGitHub {
    pub available: AtomicBool,
    pub prs: Mutex<Vec<Value>>,
    pub pr: Mutex<Option<PrInfo>>,
    pub comments: Mutex<Vec<(i64, String)>>,
    pub review_comments: Mutex<Vec<(i64, ReviewCommentPayload)>>,
}

impl FakeGitHub {
    pub fn unavailable() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Available, with one PR `gh pr view` answers for.
    pub fn with_pr(info: Value) -> Arc<Self> {
        let gh = Self::default();
        gh.available.store(true, Ordering::SeqCst);
        *gh.pr.lock().unwrap() = Some(PrInfo::from_value(info).unwrap());
        Arc::new(gh)
    }
}

impl GitHub for FakeGitHub {
    fn available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }

    fn pr_list(&self) -> Result<Vec<Value>, SourceError> {
        Ok(self.prs.lock().unwrap().clone())
    }

    fn pr_view(&self, number: i64) -> Result<PrInfo, SourceError> {
        self.pr
            .lock()
            .unwrap()
            .clone()
            .filter(|pr| pr.number == number)
            .ok_or_else(|| SourceError::Gh {
                args: "pr view".into(),
                stderr: format!("no pull requests found for {number}"),
            })
    }

    fn post_pr_comment(&self, number: i64, body: &str) -> Result<String, SourceError> {
        self.comments
            .lock()
            .unwrap()
            .push((number, body.to_string()));
        Ok(format!("https://example.test/pr/{number}#issuecomment-1"))
    }

    fn post_review_comment(
        &self,
        number: i64,
        payload: &ReviewCommentPayload,
    ) -> Result<String, SourceError> {
        self.review_comments
            .lock()
            .unwrap()
            .push((number, payload.clone()));
        Ok(format!("https://example.test/pr/{number}#discussion_r1"))
    }
}

/// A response, decoded.
pub struct Resp {
    pub status: StatusCode,
    pub content_type: String,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

impl Resp {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text()))
    }

    pub fn error(&self) -> String {
        self.json()["error"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// An `App` for a repository with its own state dir, driven in-process through its router.
pub struct TestServer {
    pub app: App,
    pub router: Router,
    pub state_dir: PathBuf,
    pub github: Arc<FakeGitHub>,
    _tmp: Option<tempfile::TempDir>,
}

impl TestServer {
    pub fn new(repo: &Path) -> Self {
        Self::build(repo, FakeGitHub::unavailable(), None, None)
    }

    pub fn with_github(repo: &Path, github: Arc<FakeGitHub>) -> Self {
        Self::build(repo, github, None, None)
    }

    /// Share `state_dir` with another server ("another process reads the same state").
    pub fn with_state_dir(repo: &Path, state_dir: &Path) -> Self {
        Self::build(
            repo,
            FakeGitHub::unavailable(),
            Some(state_dir.to_path_buf()),
            None,
        )
    }

    /// Swap the AI providers for a fake factory (`provider_factory=` in the Python tests).
    pub fn with_providers(repo: &Path, providers: Arc<dyn ProviderFactory>) -> Self {
        Self::build(repo, FakeGitHub::unavailable(), None, Some(providers))
    }

    /// Any other configuration (`desktop`, `settings_only`, ...), with a fresh state dir and
    /// no GitHub.
    pub fn with_config(repo: &Path, tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::build_with(repo, FakeGitHub::unavailable(), None, None, tweak)
    }

    fn build(
        repo: &Path,
        github: Arc<FakeGitHub>,
        state_dir: Option<PathBuf>,
        providers: Option<Arc<dyn ProviderFactory>>,
    ) -> Self {
        Self::build_with(repo, github, state_dir, providers, |_| {})
    }

    fn build_with(
        repo: &Path,
        github: Arc<FakeGitHub>,
        state_dir: Option<PathBuf>,
        providers: Option<Arc<dyn ProviderFactory>>,
        tweak: impl FnOnce(&mut ServerConfig),
    ) -> Self {
        let (state_dir, tmp) = match state_dir {
            Some(dir) => (dir, None),
            None => {
                let tmp = tempfile::tempdir().unwrap();
                (tmp.path().join("config"), Some(tmp))
            }
        };
        let mut config = ServerConfig {
            repo: repo.to_path_buf(),
            state_dir: Some(state_dir.clone()),
            github: Some(github.clone()),
            providers,
            ..Default::default()
        };
        tweak(&mut config);
        let app = App::build(config).expect("app builds");
        let router = app.router();
        Self {
            app,
            router,
            state_dir,
            github,
            _tmp: tmp,
        }
    }

    pub async fn get(&self, path: &str) -> Resp {
        let req = Request::get(path).body(Body::empty()).unwrap();
        self.send(req).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Resp {
        let req = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(req).await
    }

    pub async fn post_raw(&self, path: &str, body: &str) -> Resp {
        let req = Request::post(path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.send(req).await
    }

    async fn send(&self, req: Request<Body>) -> Resp {
        let res = self.router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let content_type = res
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let location = res
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        Resp {
            status,
            content_type,
            location,
            body,
        }
    }

    /// Analyze and return the report JSON (asserting success).
    pub async fn analyze(&self, body: Value) -> Value {
        let res = self.post("/api/analyze", body).await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.text());
        res.json()
    }
}

/// The hunks of a report as `fingerprint -> path`.
pub fn fingerprints(report: &Value) -> BTreeMap<String, String> {
    report["hunks"]
        .as_object()
        .unwrap()
        .values()
        .map(|h| {
            (
                h["fingerprint"].as_str().unwrap().to_string(),
                h["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}
