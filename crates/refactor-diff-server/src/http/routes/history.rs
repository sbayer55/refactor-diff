//! One file's history: `/api/report/{id}/history/commits` (the commits that touched it,
//! following renames), `.../content` (the file at one of them), `.../diff` (line opcodes
//! between any two), `.../blame` (who last wrote each line) and `.../checkout` (restore a
//! version, or move HEAD to it).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use refactor_diff_core::{Report, split_lines};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::{AppState, run_blocking};
use crate::git::{SourceError, WORKTREE_SHA};
use crate::history::{
    DEFAULT_LIMIT, GIT_TIMEOUT, MAX_BLOB_BYTES, MAX_DIFF_LINES, MAX_HISTORY, MAX_LIMIT, line_diff,
};
use crate::http::error::ApiError;
use crate::http::extract::ApiJson;

/// A version of the file: a commit, or the uncommitted working tree.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Version {
    Commit(String),
    Worktree,
}

impl Version {
    fn sha(&self) -> Option<&str> {
        match self {
            Version::Commit(s) => Some(s),
            Version::Worktree => None,
        }
    }

    /// For messages and cache keys.
    fn key(&self) -> String {
        match self {
            Version::Commit(s) => s.clone(),
            Version::Worktree => WORKTREE_SHA.to_string(),
        }
    }

    fn label(&self) -> String {
        match self {
            Version::Commit(s) => s[..7.min(s.len())].to_string(),
            Version::Worktree => "the working tree".to_string(),
        }
    }
}

fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn parse_version(s: &str, report: &Report) -> Result<Version, ApiError> {
    if s == WORKTREE_SHA {
        if report.source.head_sha.is_none() {
            return Ok(Version::Worktree);
        }
        return Err(ApiError::BadRequest(
            "This report compares two commits; there is no working-tree version.".into(),
        ));
    }
    if is_sha(s) {
        return Ok(Version::Commit(s.to_ascii_lowercase()));
    }
    Err(ApiError::BadRequest(format!("{s} is not a commit sha.")))
}

/// A relative path that stays inside the repository.
fn safe_path(path: &str) -> Result<&str, ApiError> {
    let p = std::path::Path::new(path);
    let escapes = path.is_empty()
        || p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        });
    if escapes {
        return Err(ApiError::BadRequest(
            "path must be a file inside the repository.".into(),
        ));
    }
    Ok(path)
}

/// Run a git call on a blocking thread, giving up after [`GIT_TIMEOUT`].
async fn git_call<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, SourceError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::time::timeout(GIT_TIMEOUT, run_blocking(f)).await {
        Err(_) => Err(ApiError::Internal(format!(
            "git did not finish within {}s.",
            GIT_TIMEOUT.as_secs()
        ))),
        Ok(result) => Ok(result??),
    }
}

struct Head {
    sha: String,
    worktree: bool,
}

/// The report's head side: its head commit, or HEAD for a working-tree report.
async fn head_of(state: &Arc<AppState>, report: &Report) -> Result<Head, ApiError> {
    match &report.source.head_sha {
        Some(sha) => Ok(Head {
            sha: sha.clone(),
            worktree: false,
        }),
        None => {
            let git = state.git.clone();
            let sha = git_call(move || git.rev("HEAD")).await?;
            Ok(Head {
                sha,
                worktree: true,
            })
        }
    }
}

/// The file's lines at `version`, from the cache when the version is a commit.
async fn lines_at(
    state: &Arc<AppState>,
    version: &Version,
    path: &str,
) -> Result<Arc<Vec<String>>, ApiError> {
    let key = (version.key(), path.to_string());
    if version.sha().is_some() {
        if let Some(lines) = state.history.blob(&key) {
            return Ok(lines);
        }
    }
    let git = state.git.clone();
    let sha = version.sha().map(String::from);
    let p = path.to_string();
    let bytes = git_call(move || git.read_blob(sha.as_deref(), &p)).await?;
    let Some(bytes) = bytes else {
        return Err(ApiError::NotFound(format!(
            "{path} doesn't exist at {}.",
            version.label()
        )));
    };
    if bytes.contains(&0) {
        return Err(ApiError::BadRequest(format!(
            "{path} is binary at {}.",
            version.label()
        )));
    }
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(ApiError::BadRequest(format!(
            "{path} is larger than {} MB at {}.",
            MAX_BLOB_BYTES / (1024 * 1024),
            version.label()
        )));
    }
    let lines = Arc::new(split_lines(&String::from_utf8_lossy(&bytes)));
    if version.sha().is_some() {
        state.history.put_blob(key, lines.clone());
    }
    Ok(lines)
}

#[derive(Debug, Default, Deserialize)]
pub struct CommitsQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The commits that touched `?path=` (as named on the head side), newest first.
pub async fn list_commits(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<CommitsQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let path = safe_path(&query.path)?.to_string();
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ApiError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIMIT}."
        )));
    }
    let head = head_of(&state, &report).await?;
    let key = (head.sha.clone(), path.clone());
    let history = match state.history.page(&key) {
        Some(page) => page,
        None => {
            let git = state.git.clone();
            let (rev, p) = (head.sha.clone(), path.clone());
            let page = Arc::new(git_call(move || git.file_history(&rev, &p, MAX_HISTORY)).await?);
            state.history.put_page(key, page.clone());
            page
        }
    };
    let total = history.commits.len();
    let start = query.offset.min(total);
    let end = (start + limit).min(total);
    let more = end < total || history.more;
    let base = report.source.base_sha.clone();
    let range_key = (base.clone(), head.sha.clone());
    let range = match state.history.range(&range_key) {
        Some(r) => r,
        None => {
            let git = state.git.clone();
            let (b, h) = range_key.clone();
            let r = Arc::new(git_call(move || git.commits_between(&b, &h)).await?);
            state.history.put_range(range_key, r.clone());
            r
        }
    };
    let mut commits: Vec<Value> = history.commits[start..end]
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).expect("commit serializes");
            v["is_head"] = json!(c.sha == head.sha);
            v["is_base"] = json!(c.sha == base);
            v["in_range"] = json!(range.contains(&c.sha));
            v
        })
        .collect();
    if query.offset == 0 && head.worktree {
        let git = state.git.clone();
        let p = path.clone();
        if let Some((insertions, deletions)) = git_call(move || git.worktree_delta(&p)).await? {
            for c in &mut commits {
                c["is_head"] = json!(false);
            }
            commits.insert(
                0,
                json!({
                    "sha": WORKTREE_SHA, "short": "work", "subject": "Uncommitted changes",
                    "author": "", "date": "", "insertions": insertions, "deletions": deletions,
                    "path": path, "is_head": true, "is_base": false, "in_range": true,
                }),
            );
        }
    }
    Ok(Json(json!({
        "path": path,
        "head": head.sha,
        "base": base,
        "worktree": head.worktree,
        "offset": query.offset,
        "limit": limit,
        "total": total,
        "more": more,
        "commits": commits,
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct VersionQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub sha: String,
}

/// The file at one version (`?sha=&path=`), as lines.
pub async fn get_content(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<VersionQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let path = safe_path(&query.path)?;
    let version = parse_version(&query.sha, &report)?;
    let lines = lines_at(&state, &version, path).await?;
    Ok(Json(json!({
        "sha": version.key(),
        "path": path,
        "lines": *lines,
    })))
}

#[derive(Debug, Default, Deserialize)]
pub struct DiffQuery {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub from_path: Option<String>,
    #[serde(default)]
    pub to_path: Option<String>,
}

/// Line opcodes between two versions (`?from=&to=&path=`, with `from_path`/`to_path` when
/// the file was named differently at either end).
pub async fn get_diff(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let path = safe_path(&query.path)?.to_string();
    let from = parse_version(&query.from, &report)?;
    let to = parse_version(&query.to, &report)?;
    let from_path = query.from_path.unwrap_or_else(|| path.clone());
    let to_path = query.to_path.unwrap_or_else(|| path.clone());
    safe_path(&from_path)?;
    safe_path(&to_path)?;
    let key = (from.key(), from_path.clone(), to.key(), to_path.clone());
    let cacheable = from.sha().is_some() && to.sha().is_some();
    let result = match cacheable.then(|| state.history.diff(&key)).flatten() {
        Some(d) => d,
        None => {
            let a = lines_at(&state, &from, &from_path).await?;
            let b = lines_at(&state, &to, &to_path).await?;
            if a.len() > MAX_DIFF_LINES || b.len() > MAX_DIFF_LINES {
                return Err(ApiError::BadRequest(format!(
                    "Too many lines to diff (limit {MAX_DIFF_LINES} per side)."
                )));
            }
            let d = Arc::new(run_blocking(move || line_diff(&a, &b)).await?);
            if cacheable {
                state.history.put_diff(key, d.clone());
            }
            d
        }
    };
    let mut body = serde_json::to_value(&*result).expect("diff serializes");
    body["from"] = json!(from.key());
    body["to"] = json!(to.key());
    body["from_path"] = json!(from_path);
    body["to_path"] = json!(to_path);
    Ok(Json(body))
}

/// Who last wrote each line of the file at one version (`?sha=&path=`).
pub async fn get_blame(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    Query(query): Query<VersionQuery>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let path = safe_path(&query.path)?.to_string();
    let version = parse_version(&query.sha, &report)?;
    let key = (version.key(), path.clone());
    let blame = match version.sha().and_then(|_| state.history.blame(&key)) {
        Some(b) => b,
        None => {
            // Make sure the file exists (and is text) there first: the 404 is clearer than
            // git's own message.
            lines_at(&state, &version, &path).await?;
            let git = state.git.clone();
            let sha = version.sha().map(String::from);
            let p = path.clone();
            let b = Arc::new(git_call(move || git.blame(sha.as_deref(), &p)).await?);
            if version.sha().is_some() {
                state.history.put_blame(key, b.clone());
            }
            b
        }
    };
    let mut body = serde_json::to_value(&*blame).expect("blame serializes");
    body["sha"] = json!(version.key());
    body["path"] = json!(path);
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
pub struct CheckoutBody {
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub force: bool,
}

/// Move HEAD to a commit (`mode: "commit"`, refused on a dirty tree) or restore one file from
/// it (`mode: "file"`). The only git writes the server makes.
pub async fn checkout(
    State(state): State<Arc<AppState>>,
    Path(report_id): Path<String>,
    ApiJson(body): ApiJson<CheckoutBody>,
) -> Result<Json<Value>, ApiError> {
    let report = state.report(&report_id)?;
    let _one_at_a_time = state.history.checkout_lock.lock().await;
    if !is_sha(&body.sha) {
        return Err(ApiError::BadRequest("sha must be a commit.".into()));
    }
    let git = state.git.clone();
    let full = {
        let (git, sha) = (git.clone(), body.sha.clone());
        git_call(move || git.rev(&sha)).await.map_err(|_| {
            ApiError::BadRequest(format!("{} is not a commit in this repository.", body.sha))
        })?
    };
    let short = full[..7].to_string();
    let head_before = {
        let git = git.clone();
        git_call(move || git.rev("HEAD")).await?
    };
    let stale = report.source.head_sha.is_none();
    let (command, message, undo, path) = match body.mode.as_str() {
        "commit" => {
            let clean = {
                let git = git.clone();
                git_call(move || git.is_clean()).await?
            };
            if !clean {
                return Err(ApiError::BadRequest(format!(
                    "The working tree has uncommitted changes; commit or stash them before checking out {short}."
                )));
            }
            {
                let (git, sha) = (git.clone(), full.clone());
                git_call(move || git.checkout_commit(&sha)).await?;
            }
            (
                format!("git checkout {full}"),
                format!(
                    "Checked out {short}. HEAD is detached; check out {} to go back.",
                    &head_before[..7]
                ),
                json!({"mode": "commit", "sha": head_before}),
                Value::Null,
            )
        }
        "file" => {
            let path = body.path.as_deref().ok_or_else(|| {
                ApiError::BadRequest("path is required to restore a file.".into())
            })?;
            let path = safe_path(path)?.to_string();
            let exists = {
                let (git, sha, p) = (git.clone(), full.clone(), path.clone());
                git_call(move || Ok(git.has_path_at(&sha, &p))).await?
            };
            if !exists {
                return Err(ApiError::NotFound(format!(
                    "{path} doesn't exist at {short}."
                )));
            }
            if !body.force {
                let clean = {
                    let (git, p) = (git.clone(), path.clone());
                    git_call(move || git.path_is_clean(&p)).await?
                };
                if !clean {
                    return Err(ApiError::BadRequest(format!(
                        "{path} has uncommitted changes; the restore would overwrite them."
                    )));
                }
            }
            {
                let (git, sha, p) = (git.clone(), full.clone(), path.clone());
                git_call(move || git.checkout_file(&sha, &p)).await?;
            }
            (
                format!("git checkout {full} -- {path}"),
                format!("Restored {path} from {short}."),
                json!({"mode": "file", "sha": head_before, "path": path, "force": true}),
                json!(path),
            )
        }
        _ => return Err(ApiError::BadRequest("mode must be commit or file.".into())),
    };
    Ok(Json(json!({
        "ok": true,
        "mode": body.mode,
        "sha": full,
        "short": short,
        "path": path,
        "command": command,
        "head_before": head_before,
        "detached": body.mode == "commit",
        "stale": stale,
        "undo": undo,
        "message": message,
    })))
}
