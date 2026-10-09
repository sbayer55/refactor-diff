//! Go-to-definition and find-references: Jedi for Python, tsserver for TypeScript/JavaScript.
//!
//! Each side of a diff is resolved in its own revision: removed lines against a snapshot of
//! the base commit, added and unchanged lines against the head (a snapshot, or the working
//! tree). The user's own environment supplies third-party packages: a virtualenv for Python
//! (any of its `sys.path` entries pointing back into the repository, an editable install of
//! the project, are remapped into the snapshot so project imports resolve to the code at that
//! revision) and the repository's `node_modules` for TypeScript.
//!
//! Lines are 1-based; columns are 0-based and counted in Unicode code points, like the rest
//! of the engine.

pub mod bundle;
mod jedi;
mod tsserver;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::exec::Tools;
use crate::snapshots::Snapshots;

pub use jedi::JediBackend;
pub use tsserver::TsServerBackend;

/// Results per query, in whatever order the backend produced them.
pub const MAX_RESULTS: usize = 1000;

/// Where a navigation result lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A file in the repository at the queried revision.
    Repo,
    /// An installed package, stdlib or typeshed stub.
    Library,
    /// A compiled builtin with no source to show.
    Builtin,
}

/// One definition or reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Location {
    pub kind: Kind,
    /// Repo-relative (posix) for `Repo`, absolute for `Library`, a module name for `Builtin`.
    pub path: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub name: String,
    /// function, class, module, param, statement, ... (`definition`/`reference` for tsserver).
    #[serde(rename = "type")]
    pub kind_name: String,
    /// The source line, for previews.
    pub text: String,
    pub is_definition: bool,
}

/// A navigation failure the UI shows to the user as-is.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NavigationError(pub String);

impl NavigationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Library files that results have pointed at: the only ones `library_source` will serve.
pub type LibraryFiles = Arc<Mutex<HashSet<PathBuf>>>;

/// Order results (repository files first, then by path and line) and drop duplicates of the
/// same (path, line, col), keeping the first.
pub fn finish(mut locations: Vec<Location>) -> Vec<Location> {
    locations.sort_by(|a, b| {
        (a.kind != Kind::Repo, &a.path, a.line.unwrap_or(0)).cmp(&(
            b.kind != Kind::Repo,
            &b.path,
            b.line.unwrap_or(0),
        ))
    });
    let mut seen = HashSet::new();
    locations.retain(|loc| seen.insert((loc.path.clone(), loc.line, loc.col)));
    locations
}

/// Dispatches each query to the backend for the file's language.
pub struct Navigator {
    jedi: JediBackend,
    ts: TsServerBackend,
    library_files: LibraryFiles,
}

enum Backend<'a> {
    Jedi(&'a JediBackend),
    Ts(&'a TsServerBackend),
}

impl Navigator {
    /// `python` is an interpreter or environment for Jedi (`--python`); `tsserver` a tsserver
    /// executable, TypeScript's `lib/tsserver.js` or a `typescript` package directory
    /// (`--tsserver`). Both are discovered when `None`.
    pub fn new(
        repo: PathBuf,
        snapshots: Arc<Snapshots>,
        tools: Arc<Tools>,
        python: Option<PathBuf>,
        tsserver: Option<PathBuf>,
    ) -> Self {
        let library_files: LibraryFiles = Arc::new(Mutex::new(HashSet::new()));
        Self {
            jedi: JediBackend::new(
                repo.clone(),
                Arc::clone(&snapshots),
                Arc::clone(&tools),
                python,
                Arc::clone(&library_files),
            ),
            ts: TsServerBackend::new(repo, snapshots, tools, tsserver, Arc::clone(&library_files)),
            library_files,
        }
    }

    /// Extract the bundled Jedi helper under `dir` instead of the user's cache directory
    /// (tests). Must be called before the first Python query.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.jedi = self.jedi.with_cache_dir(dir);
        self
    }

    fn backend(&self, path: &str) -> Result<Backend<'_>, NavigationError> {
        match refactor_diff_core::analyzer_for(path).map(|a| a.name()) {
            None => Err(NavigationError(format!(
                "Code navigation isn't available for {path}."
            ))),
            Some("python") => Ok(Backend::Jedi(&self.jedi)),
            Some(_) => Ok(Backend::Ts(&self.ts)),
        }
    }

    pub async fn definitions(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        match self.backend(path)? {
            Backend::Jedi(b) => b.definitions(sha, path, line, col).await,
            Backend::Ts(b) => b.definitions(sha, path, line, col).await,
        }
    }

    pub async fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        match self.backend(path)? {
            Backend::Jedi(b) => b.references(sha, path, line, col).await,
            Backend::Ts(b) => b.references(sha, path, line, col).await,
        }
    }

    /// A one-line description of the environment answering queries for `path`
    /// ("Python 3.12 at /…/.venv", "TypeScript 5.4.5 at /…/tsserver.js").
    pub async fn describe_environment(&self, path: &str) -> Result<String, NavigationError> {
        match self.backend(path)? {
            Backend::Jedi(b) => b.describe_environment().await,
            Backend::Ts(b) => b.describe_environment(),
        }
    }

    /// Source of a library file that an earlier result pointed at.
    pub fn library_source(&self, path: &str) -> Result<String, NavigationError> {
        let allowed = self
            .library_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(Path::new(path));
        if !allowed {
            return Err(NavigationError::new(
                "Only library files reached through navigation can be viewed.",
            ));
        }
        std::fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| NavigationError(e.to_string()))
    }

    /// Stop the helper processes.
    pub async fn close(&self) {
        self.ts.close().await;
        self.jedi.close().await;
    }
}

/// The directory holding `sha`'s sources (the repository for the working tree). Snapshots
/// run git, so this hops to a blocking thread.
async fn revision_root(
    snapshots: &Arc<Snapshots>,
    sha: Option<&str>,
) -> Result<PathBuf, NavigationError> {
    let snapshots = Arc::clone(snapshots);
    let sha = sha.map(str::to_owned);
    tokio::task::spawn_blocking(move || snapshots.root(sha.as_deref()))
        .await
        .map_err(|e| NavigationError(format!("Couldn't prepare the snapshot: {e}")))?
        .map_err(|e| NavigationError(e.to_string()))
}

/// Read a file as text, replacing invalid UTF-8.
fn read_text(path: &Path) -> std::io::Result<String> {
    std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// A path's components joined with `/`, as the UI names files.
fn posix(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// `file` relative to `root`, matching either the paths as given or their canonical forms
/// (a snapshot under a symlinked temp dir may come back resolved).
fn relative_to(file: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(rel) = file.strip_prefix(root) {
        return Some(rel.to_path_buf());
    }
    let (file, root) = (file.canonicalize().ok()?, root.canonicalize().ok()?);
    file.strip_prefix(&root).ok().map(Path::to_path_buf)
}

/// The requested line of a file, or `""` when it doesn't exist.
fn line_of(lines: &[String], line: Option<u32>) -> String {
    line.and_then(|l| l.checked_sub(1))
        .and_then(|i| lines.get(i as usize))
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(kind: Kind, path: &str, line: u32, col: u32) -> Location {
        Location {
            kind,
            path: path.into(),
            line: Some(line),
            col: Some(col),
            name: "x".into(),
            kind_name: "function".into(),
            text: String::new(),
            is_definition: false,
        }
    }

    #[test]
    fn finish_orders_repo_first_and_dedupes() {
        let out = finish(vec![
            loc(Kind::Library, "/lib/b.py", 1, 0),
            loc(Kind::Repo, "b.py", 9, 0),
            loc(Kind::Repo, "a.py", 3, 0),
            loc(Kind::Repo, "a.py", 3, 0),
            loc(Kind::Repo, "a.py", 2, 0),
            loc(Kind::Builtin, "builtins", 0, 0),
        ]);
        let got: Vec<_> = out
            .iter()
            .map(|l| (l.kind, l.path.as_str(), l.line))
            .collect();
        assert_eq!(
            got,
            vec![
                (Kind::Repo, "a.py", Some(2)),
                (Kind::Repo, "a.py", Some(3)),
                (Kind::Repo, "b.py", Some(9)),
                (Kind::Library, "/lib/b.py", Some(1)),
                (Kind::Builtin, "builtins", Some(0)),
            ]
        );
    }

    #[test]
    fn location_serializes_like_the_python_dataclass() {
        let json = serde_json::to_value(loc(Kind::Library, "/x.pyi", 4, 2)).unwrap();
        assert_eq!(json["kind"], "library");
        assert_eq!(json["type"], "function");
        assert_eq!(json["is_definition"], false);
        assert!(json.get("kind_name").is_none());
    }

    #[test]
    fn posix_paths() {
        assert_eq!(posix(Path::new("src/pkg/mod.py")), "src/pkg/mod.py");
        assert_eq!(posix(Path::new("a.py")), "a.py");
    }

    #[test]
    fn line_lookup_is_one_based_and_clamped() {
        let lines = vec!["a".to_string(), "b".to_string()];
        assert_eq!(line_of(&lines, Some(1)), "a");
        assert_eq!(line_of(&lines, Some(2)), "b");
        assert_eq!(line_of(&lines, Some(3)), "");
        assert_eq!(line_of(&lines, Some(0)), "");
        assert_eq!(line_of(&lines, None), "");
    }
}
