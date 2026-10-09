//! What a server is started with: the repository, the UI defaults the CLI flags pre-select,
//! and where tools and state live.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use refactor_diff_core::Category;
use serde::Serialize;

use crate::ai::ProviderFactory;
use crate::git::GitHub;

/// Everything [`crate::App::build`] needs.
#[derive(Clone, Default)]
pub struct ServerConfig {
    /// Any path inside the repository; the root is resolved with git.
    pub repo: PathBuf,
    /// Pre-selected source and filters, served as `/api/config`'s `defaults`.
    pub defaults: Defaults,
    /// Python interpreter or virtualenv for Python code navigation.
    pub python: Option<PathBuf>,
    /// `tsserver` (or `tsserver.js`) for TypeScript/JavaScript code navigation.
    pub tsserver: Option<PathBuf>,
    /// Where settings and review marks live; the XDG config dir when `None`.
    pub state_dir: Option<PathBuf>,
    /// Where revision snapshots are written; a fresh temp dir (deleted at shutdown) when `None`.
    pub snapshot_dir: Option<PathBuf>,
    /// PATH for child processes; this process's PATH when `None`.
    pub path: Option<OsString>,
    /// GitHub access; the `gh` CLI when `None`.
    pub github: Option<Arc<dyn GitHub>>,
    /// The AI providers behind the Ask menu; the real ones (Claude, Ollama, OpenAI-compatible)
    /// when `None`.
    pub providers: Option<Arc<dyn ProviderFactory>>,
    /// Served as `/api/config`'s `desktop`: the UI is inside the desktop app (whose shell
    /// handles things like the Settings window).
    pub desktop: bool,
    /// Serve only the settings page and its API, with no repository: what the desktop app's
    /// Settings window talks to (`refactor-diff --settings-only`). `repo` is ignored.
    pub settings_only: bool,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("repo", &self.repo)
            .field("defaults", &self.defaults)
            .field("python", &self.python)
            .field("tsserver", &self.tsserver)
            .field("state_dir", &self.state_dir)
            .field("snapshot_dir", &self.snapshot_dir)
            .field("path", &self.path)
            .field("github", &self.github.as_ref().map(|_| "<custom>"))
            .field("providers", &self.providers.as_ref().map(|_| "<custom>"))
            .field("desktop", &self.desktop)
            .field("settings_only", &self.settings_only)
            .finish()
    }
}

/// How the UI pre-selects a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Pr,
    Worktree,
    Refs,
}

/// The `defaults` object of `/api/config`: keys are present only when set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Defaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<Filters>,
    /// URL template for "Open in editor" links; always set when built by the CLI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
}

/// Pre-set UI filters (`--hide`, `--exclude`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Filters {
    /// File categories to hide, sorted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_docs: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_imports: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_file_moves: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_moves: Option<bool>,
    /// Globs of files to hide.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

/// URL templates for "Open in editor" links. `{path}` is absolute; `{line}` and `{col}` are
/// 1-based.
pub const EDITORS: &[(&str, &str)] = &[
    ("vscode", "vscode://file/{path}:{line}:{col}"),
    ("cursor", "cursor://file/{path}:{line}:{col}"),
    ("zed", "zed://file/{path}:{line}:{col}"),
    ("idea", "idea://open?file={path}&line={line}"),
    ("pycharm", "pycharm://open?file={path}&line={line}"),
];

/// `--hide` values for kinds of change, and the UI filter each one sets.
const CHANGE_FILTERS: &[(&str, ChangeFilter)] = &[
    ("comments", ChangeFilter::Docs),
    ("imports", ChangeFilter::Imports),
    ("file-moves", ChangeFilter::FileMoves),
    ("moves", ChangeFilter::Moves),
];

#[derive(Clone, Copy)]
enum ChangeFilter {
    Docs,
    Imports,
    FileMoves,
    Moves,
}

/// A CLI flag the server can't honour; the message is what the CLI prints after
/// `refactor-diff: `.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("unknown --hide value(s): {0}")]
    UnknownHide(String),
    #[error(
        "--editor must be one of {} or a URL template containing {{path}} (and optionally {{line}}, {{col}})",
        editor_names()
    )]
    BadEditor,
}

fn editor_names() -> String {
    EDITORS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The URL template for an editor preset (case-insensitive) or a custom template.
pub fn editor_template(spec: &str) -> Result<String, ConfigError> {
    let lower = spec.to_lowercase();
    let template = EDITORS
        .iter()
        .find(|(name, _)| *name == lower)
        .map_or(spec, |(_, template)| template);
    if !template.contains("{path}") {
        return Err(ConfigError::BadEditor);
    }
    Ok(template.to_string())
}

/// The filters `--hide KINDS` and `--exclude GLOB...` pre-set; `None` when neither is given.
pub fn filter_defaults(
    hide: Option<&str>,
    exclude: &[String],
) -> Result<Option<Filters>, ConfigError> {
    let mut filters = Filters::default();
    let mut any = false;
    if let Some(hide) = hide.filter(|h| !h.is_empty()) {
        let mut kinds: Vec<String> = hide
            .split(',')
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
        kinds.sort();
        kinds.dedup();
        let mut unknown: Vec<&str> = kinds
            .iter()
            .map(String::as_str)
            .filter(|k| Category::parse(k).is_none() && !CHANGE_FILTERS.iter().any(|(c, _)| c == k))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            return Err(ConfigError::UnknownHide(unknown.join(", ")));
        }
        filters.hidden = Some(
            kinds
                .iter()
                .filter(|k| Category::parse(k).is_some())
                .cloned()
                .collect(),
        );
        for (kind, filter) in CHANGE_FILTERS {
            let on = Some(kinds.iter().any(|k| k == kind));
            match filter {
                ChangeFilter::Docs => filters.hide_docs = on,
                ChangeFilter::Imports => filters.hide_imports = on,
                ChangeFilter::FileMoves => filters.hide_file_moves = on,
                ChangeFilter::Moves => filters.hide_moves = on,
            }
        }
        any = true;
    }
    if !exclude.is_empty() {
        filters.exclude = Some(exclude.to_vec());
        any = true;
    }
    Ok(any.then_some(filters))
}

/// The UI defaults for the CLI's flags. A PR wins over `--worktree`, which wins over a range;
/// a range splits at `...` or `..`, and a missing head means `HEAD`.
pub fn defaults_from(
    range: Option<&str>,
    pr: Option<u64>,
    worktree: Option<&str>,
    hide: Option<&str>,
    exclude: &[String],
    editor: &str,
) -> Result<Defaults, ConfigError> {
    let mut defaults = source_defaults(range, pr, worktree);
    defaults.filters = filter_defaults(hide, exclude)?;
    defaults.editor = Some(editor_template(editor)?);
    Ok(defaults)
}

fn source_defaults(range: Option<&str>, pr: Option<u64>, worktree: Option<&str>) -> Defaults {
    if let Some(pr) = pr {
        return Defaults {
            mode: Some(Mode::Pr),
            pr: Some(pr),
            ..Default::default()
        };
    }
    if let Some(base) = worktree.filter(|w| !w.is_empty()) {
        return Defaults {
            mode: Some(Mode::Worktree),
            base: Some(base.to_string()),
            ..Default::default()
        };
    }
    if let Some(range) = range.filter(|r| !r.is_empty()) {
        let sep = if range.contains("...") { "..." } else { ".." };
        let (base, head) = range.split_once(sep).unwrap_or((range, ""));
        return Defaults {
            mode: Some(Mode::Refs),
            base: Some(base.to_string()),
            head: Some(if head.is_empty() { "HEAD" } else { head }.to_string()),
            ..Default::default()
        };
    }
    Defaults::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const VSCODE: &str = "vscode://file/{path}:{line}:{col}";

    fn value(d: &Defaults) -> serde_json::Value {
        serde_json::to_value(d).unwrap()
    }

    #[test]
    fn range_defaults() {
        let d = defaults_from(Some("main..feature"), None, None, None, &[], "vscode").unwrap();
        assert_eq!(
            value(&d),
            json!({"mode": "refs", "base": "main", "head": "feature", "editor": VSCODE})
        );
        let d = defaults_from(Some("main...feature"), None, None, None, &[], "vscode").unwrap();
        assert_eq!(d.base.as_deref(), Some("main"));
        assert_eq!(d.head.as_deref(), Some("feature"));
        let d = defaults_from(Some("main"), None, None, None, &[], "vscode").unwrap();
        assert_eq!(d.head.as_deref(), Some("HEAD"));
    }

    #[test]
    fn pr_wins_over_worktree_and_range() {
        let d = defaults_from(Some("a..b"), Some(7), Some("main"), None, &[], "vscode").unwrap();
        assert_eq!(value(&d), json!({"mode": "pr", "pr": 7, "editor": VSCODE}));
        let d = defaults_from(Some("a..b"), None, Some("main"), None, &[], "vscode").unwrap();
        assert_eq!(
            value(&d),
            json!({"mode": "worktree", "base": "main", "editor": VSCODE})
        );
        let d = defaults_from(None, None, None, None, &[], "vscode").unwrap();
        assert_eq!(value(&d), json!({"editor": VSCODE}));
    }

    #[test]
    fn filter_defaults_match_the_cli() {
        let exclude = ["migrations".to_string(), "*_pb2.py".to_string()];
        let d = defaults_from(
            None,
            None,
            None,
            Some("tests, docs,comments,moves"),
            &exclude,
            "vscode",
        )
        .unwrap();
        assert_eq!(
            value(&d),
            json!({
                "filters": {
                    "hidden": ["docs", "tests"],
                    "hideDocs": true,
                    "hideImports": false,
                    "hideFileMoves": false,
                    "hideMoves": true,
                    "exclude": ["migrations", "*_pb2.py"],
                },
                "editor": VSCODE,
            })
        );
        assert_eq!(filter_defaults(None, &[]).unwrap(), None);
        assert_eq!(filter_defaults(Some(""), &[]).unwrap(), None);
    }

    #[test]
    fn unknown_hide_values_are_an_error() {
        assert_eq!(
            filter_defaults(Some("testz,bogus"), &[]).unwrap_err(),
            ConfigError::UnknownHide("bogus, testz".into())
        );
        assert_eq!(
            ConfigError::UnknownHide("bogus".into()).to_string(),
            "unknown --hide value(s): bogus"
        );
    }

    #[test]
    fn editor_presets_and_templates() {
        assert_eq!(
            editor_template("Zed").unwrap(),
            "zed://file/{path}:{line}:{col}"
        );
        let custom = "x-mine://{path}?l={line}";
        assert_eq!(editor_template(custom).unwrap(), custom);
        assert_eq!(
            editor_template("notepad").unwrap_err(),
            ConfigError::BadEditor
        );
        assert_eq!(
            ConfigError::BadEditor.to_string(),
            "--editor must be one of vscode, cursor, zed, idea, pycharm or a URL template \
             containing {path} (and optionally {line}, {col})"
        );
    }
}
