//! `refactor-diff://open?repo=OWNER/NAME&pr=N` (or `&base=B&head=H`) links, which the browser
//! extension opens from GitHub pull request and compare pages. LaunchServices hands them to
//! the app as `RunEvent::Opened`, whether it's running or not.
//!
//! Everything in a link comes from a web page, so it's validated strictly here: refs end up
//! as `git` and `gh` arguments and must not be able to look like options.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use refactor_diff_server::{Defaults, Mode};
use tauri::{AppHandle, Manager};
use url::Url;

use crate::{app_state, backend::Backend, commands, recents, repo_match};

/// Open `url` (from `RunEvent::Opened`): find the repository's checkout and diff the link's
/// source there, or have the landing page ask where the checkout is.
pub async fn handle(app: AppHandle, url: Url) {
    let request = match parse(&url) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("ignoring link {url}: {e}");
            return;
        }
    };
    tracing::info!("open link: {} {:?}", request.slug, request.source);
    focus(&app);
    let backend = app.state::<Backend>();
    let tools = backend.tools().await;
    let state = app_state::store(&app).get();
    let list = repo_match::candidates(
        state
            .github_repos
            .get(&request.slug.key())
            .map(PathBuf::from),
        backend.repo(),
        recents::list(&app).into_iter().map(PathBuf::from).collect(),
        &app_state::repos_dir(),
        &request.slug,
    );
    let slug = request.slug.clone();
    let found = tauri::async_runtime::spawn_blocking(move || {
        repo_match::find(&slug, &list, Path::is_dir, |p| {
            repo_match::remote_slugs(&tools, p)
        })
    })
    .await
    .ok()
    .flatten();
    match found {
        Some(repo) => {
            backend.locate.lock().unwrap().take();
            if let Err(e) = open_found(app.clone(), &request, repo).await {
                tracing::warn!("open link failed: {e}");
            }
        }
        None => {
            tracing::info!("no checkout of {} found; asking", request.slug);
            *backend.locate.lock().unwrap() = Some(request);
            commands::show_landing(&app, "#locate");
        }
    }
}

/// Remember `repo` as the checkout of the link's repository and show the link's source there:
/// in place when that repository's review UI is already showing, otherwise by opening it.
pub async fn open_found(
    app: AppHandle,
    request: &OpenRequest,
    repo: PathBuf,
) -> Result<(), String> {
    let repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    let path = repo.to_string_lossy().into_owned();
    app_state::store(&app).update(|s| {
        s.github_repos.insert(request.slug.key(), path);
    });
    focus(&app);
    let backend = app.state::<Backend>();
    let idle = backend.opening.lock().unwrap().is_none();
    if idle && backend.repo().as_deref() == Some(repo.as_path()) && crate::review_showing(&app) {
        crate::eval_in_review(&app, &format!("openSource({})", request.source.js_arg()));
        return Ok(());
    }
    commands::open(app, repo, request.source.defaults()).await
}

fn focus(app: &AppHandle) {
    if let Some(win) = crate::main_window(app) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// A GitHub repository as `owner/name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSlug {
    pub owner: String,
    pub name: String,
}

impl RepoSlug {
    pub fn parse(s: &str) -> Option<Self> {
        let (owner, name) = s.split_once('/')?;
        let owner_ok = !owner.is_empty()
            && owner.len() <= 39
            && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !owner.starts_with('-');
        let name_ok = !name.is_empty()
            && name.len() <= 100
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !matches!(name, "." | "..")
            && !name.starts_with('-');
        (owner_ok && name_ok).then(|| Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// GitHub names are case-insensitive: the key to compare and remember slugs by.
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.name).to_ascii_lowercase()
    }
}

impl std::fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// What to diff once the repository is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Pr(u64),
    /// `base` missing means the repository's default base branch.
    Refs {
        base: Option<String>,
        head: String,
    },
}

impl Source {
    /// The review UI's pre-selection for this source.
    pub fn defaults(&self) -> Defaults {
        match self {
            Source::Pr(pr) => Defaults {
                mode: Some(Mode::Pr),
                pr: Some(*pr),
                ..Default::default()
            },
            Source::Refs { base, head } => Defaults {
                mode: Some(Mode::Refs),
                base: base.clone(),
                head: Some(head.clone()),
                ..Default::default()
            },
        }
    }

    /// The `window.refactorDiff.openSource(...)` argument for a review UI already showing.
    pub fn js_arg(&self) -> String {
        let value = match self {
            Source::Pr(pr) => serde_json::json!({"mode": "pr", "pr": pr}),
            Source::Refs { base, head } => {
                serde_json::json!({"mode": "refs", "base": base, "head": head})
            }
        };
        value.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    pub slug: RepoSlug,
    pub source: Source,
}

/// Whether `url` is an open link (as opposed to a folder or something else).
pub fn is_open_link(url: &Url) -> bool {
    url.scheme() == "refactor-diff" && url.host_str() == Some("open")
}

pub fn parse(url: &Url) -> Result<OpenRequest, String> {
    if !is_open_link(url) {
        return Err(format!("not an open link: {url}"));
    }
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    let get = |k: &str| query.get(k).map(String::as_str).filter(|v| !v.is_empty());
    let slug = get("repo")
        .and_then(RepoSlug::parse)
        .ok_or("the link needs repo=OWNER/NAME")?;
    let source = if let Some(pr) = get("pr") {
        let pr: u64 = pr
            .parse()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("bad pull request number {pr:?}"))?;
        Source::Pr(pr)
    } else {
        let head = get("head").ok_or("the link needs pr=N or head=REF")?;
        let base = get("base");
        for r in base.iter().chain([&head]) {
            if !valid_ref(r) {
                return Err(format!("bad ref {r:?}"));
            }
        }
        Source::Refs {
            base: base.map(str::to_string),
            head: head.to_string(),
        }
    };
    Ok(OpenRequest { slug, source })
}

/// A branch or tag name per `git check-ref-format` (plus: no leading `-`, no `:` so a
/// cross-fork `owner:branch` is refused rather than misread).
fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 255
        && !r.starts_with(['-', '/', '.'])
        && !r.ends_with(['/', '.'])
        && !r.ends_with(".lock")
        && !r.contains("..")
        && !r.contains("//")
        && !r.contains("@{")
        && !r.contains("/.")
        && r != "@"
        && !r.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<OpenRequest, String> {
        parse(&Url::parse(s).unwrap())
    }

    #[test]
    fn pull_request_link() {
        let req = parse_str("refactor-diff://open?repo=sbayer55/refactor-diff&pr=19").unwrap();
        assert_eq!(req.slug.to_string(), "sbayer55/refactor-diff");
        assert_eq!(req.source, Source::Pr(19));
        assert_eq!(req.source.defaults().mode, Some(Mode::Pr));
        assert_eq!(req.source.js_arg(), r#"{"mode":"pr","pr":19}"#);
    }

    #[test]
    fn compare_links() {
        let req =
            parse_str("refactor-diff://open?repo=o/n.js&base=main&head=feature%2Fx-1").unwrap();
        assert_eq!(
            req.source,
            Source::Refs {
                base: Some("main".into()),
                head: "feature/x-1".into()
            }
        );
        let req = parse_str("refactor-diff://open?repo=o/n&head=topic&base=").unwrap();
        assert_eq!(
            req.source,
            Source::Refs {
                base: None,
                head: "topic".into()
            }
        );
        let d = req.source.defaults();
        assert_eq!((d.base, d.head.as_deref()), (None, Some("topic")));
        assert_eq!(
            req.source.js_arg(),
            r#"{"base":null,"head":"topic","mode":"refs"}"#
        );
    }

    #[test]
    fn rejects_bad_links() {
        for bad in [
            "refactor-diff://settings/x",
            "refactor-diff://open?pr=1",
            "refactor-diff://open?repo=o&pr=1",
            "refactor-diff://open?repo=o/n/x&pr=1",
            "refactor-diff://open?repo=-o/n&pr=1",
            "refactor-diff://open?repo=o/..&pr=1",
            "refactor-diff://open?repo=o/n",
            "refactor-diff://open?repo=o/n&pr=0",
            "refactor-diff://open?repo=o/n&pr=-1",
            "refactor-diff://open?repo=o/n&pr=1x",
            "refactor-diff://open?repo=o/n&head=--upload-pack=x",
            "refactor-diff://open?repo=o/n&head=x&base=-b",
            "refactor-diff://open?repo=o/n&head=a..b",
            "refactor-diff://open?repo=o/n&head=someone:topic",
            "refactor-diff://open?repo=o/n&head=a%20b",
            "refactor-diff://open?repo=o/n&head=x.lock",
            "refactor-diff://open?repo=o/n&head=x%0a",
        ] {
            assert!(parse_str(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn slug_key_ignores_case() {
        assert_eq!(
            RepoSlug::parse("SBayer55/Refactor-Diff").unwrap().key(),
            "sbayer55/refactor-diff"
        );
    }
}
