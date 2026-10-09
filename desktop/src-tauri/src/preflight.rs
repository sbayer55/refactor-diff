//! Checks for the command-line tools the backend relies on, shown on the landing page so a
//! missing one is explained up front instead of failing halfway through a review.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::sidecar::{self, run_with_timeout, SidecarManager};

const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    name: &'static str,
    /// The app can't do anything without it.
    required: bool,
    /// What it's used for.
    purpose: &'static str,
    path: Option<String>,
    version: Option<String>,
    /// Why it can't be used, if it can't.
    problem: Option<String>,
    /// A command that fixes the problem.
    fix: &'static str,
}

#[tauri::command]
pub async fn preflight(app: AppHandle, refresh: bool) -> Vec<Tool> {
    tauri::async_runtime::spawn_blocking(move || {
        let path_var = if refresh {
            sidecar::recapture_shell_path(&app)
        } else {
            app.state::<SidecarManager>()
                .wait_shell_path(Duration::from_secs(6))
                .unwrap_or_default()
        };
        check_all(&path_var)
    })
    .await
    .unwrap_or_default()
}

fn check_all(path_var: &str) -> Vec<Tool> {
    let clt = has_command_line_tools();
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        check(
            Tool::new(
                "git",
                true,
                "reading the repository",
                "xcode-select --install",
            ),
            find_in_path(path_var, "git", is_executable),
            clt,
            &["--version"],
        ),
        check_gh(find_in_path(path_var, "gh", is_executable)),
        check(
            Tool::new(
                "node",
                false,
                "TypeScript code navigation",
                "brew install node",
            ),
            find_in_path(path_var, "node", is_executable).or_else(|| nvm_node(&home)),
            clt,
            &["--version"],
        ),
        check(
            Tool::new(
                "python3",
                false,
                "Python code navigation",
                "brew install python",
            ),
            find_in_path(path_var, "python3", is_executable),
            clt,
            &["--version"],
        ),
    ]
}

impl Tool {
    fn new(name: &'static str, required: bool, purpose: &'static str, fix: &'static str) -> Self {
        Self {
            name,
            required,
            purpose,
            path: None,
            version: None,
            problem: None,
            fix,
        }
    }
}

fn check(mut tool: Tool, found: Option<PathBuf>, clt: bool, version_args: &[&str]) -> Tool {
    let Some(path) = found else {
        tool.problem = Some("not found on your PATH".into());
        return tool;
    };
    // /usr/bin/git and /usr/bin/python3 are stubs that pop up Apple's installer when the
    // Command Line Tools are missing; don't run them.
    if is_xcode_stub(&path) && !clt {
        tool.problem = Some("needs the Xcode Command Line Tools".into());
        tool.fix = "xcode-select --install";
        return tool;
    }
    tool.path = Some(path.to_string_lossy().into_owned());
    match run_with_timeout(Command::new(&path).args(version_args), VERSION_TIMEOUT) {
        Some(out) => tool.version = first_line(&out),
        None => tool.problem = Some(format!("`{} {}` failed", tool.name, version_args.join(" "))),
    }
    tool
}

fn check_gh(found: Option<PathBuf>) -> Tool {
    let mut tool = check(
        Tool::new(
            "gh",
            false,
            "pull requests",
            "brew install gh && gh auth login",
        ),
        found,
        true,
        &["--version"],
    );
    if let (Some(path), None) = (&tool.path, &tool.problem) {
        if run_with_timeout(Command::new(path).args(["auth", "status"]), VERSION_TIMEOUT).is_none()
        {
            tool.problem =
                Some("couldn't confirm you're signed in to GitHub (`gh auth status`)".into());
            tool.fix = "gh auth login";
        }
    }
    tool
}

fn first_line(out: &str) -> Option<String> {
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn is_xcode_stub(path: &Path) -> bool {
    path.starts_with("/usr/bin")
}

fn has_command_line_tools() -> bool {
    run_with_timeout(
        Command::new("/usr/bin/xcode-select").arg("-p"),
        VERSION_TIMEOUT,
    )
    .is_some()
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The first `name` in the `:`-separated `path_var` that `is_exec` accepts.
fn find_in_path(path_var: &str, name: &str, is_exec: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    path_var
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| is_exec(candidate))
}

/// nvm's newest node, which the backend also falls back to (see `tsserver.py`).
fn nvm_node(home: &str) -> Option<PathBuf> {
    let dir = std::env::var("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(home).join(".nvm"));
    let mut versions: Vec<PathBuf> = std::fs::read_dir(dir.join("versions/node"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path().join("bin/node")))
        .filter(|p| is_executable(p))
        .collect();
    versions.sort();
    versions.pop()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_match_in_path_order() {
        let exists = |p: &Path| p == Path::new("/b/git") || p == Path::new("/c/git");
        assert_eq!(
            find_in_path("/a::/b:/c", "git", exists),
            Some(PathBuf::from("/b/git"))
        );
        assert_eq!(find_in_path("/a", "git", exists), None);
        assert_eq!(find_in_path("", "git", exists), None);
    }

    #[test]
    fn missing_tool_reports_its_fix() {
        let tool = check(
            Tool::new("node", false, "x", "brew install node"),
            None,
            true,
            &["--version"],
        );
        assert_eq!(tool.problem.as_deref(), Some("not found on your PATH"));
        assert_eq!(tool.fix, "brew install node");
    }

    #[test]
    fn xcode_stubs_are_not_run_without_the_command_line_tools() {
        let tool = check(
            Tool::new("git", true, "x", "x"),
            Some(PathBuf::from("/usr/bin/git")),
            false,
            &["--version"],
        );
        assert_eq!(
            tool.problem.as_deref(),
            Some("needs the Xcode Command Line Tools")
        );
        assert!(tool.version.is_none());
    }

    #[test]
    fn version_is_the_first_nonblank_line() {
        assert_eq!(
            first_line("\n gh version 2.60.0 (2026-01-01)\nhttps://…\n").as_deref(),
            Some("gh version 2.60.0 (2026-01-01)")
        );
        assert_eq!(first_line("  \n"), None);
    }
}
