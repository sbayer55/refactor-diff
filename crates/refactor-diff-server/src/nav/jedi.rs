//! Python navigation through Jedi, running in the user's interpreter.
//!
//! One long-lived helper process (`python/helper.py`, bundled with the vendored jedi and
//! parso) answers queries over JSON lines on stdin/stdout. It runs under the interpreter the
//! user pointed at (`--python`), the repository's virtualenv, or `python3` from PATH, so
//! third-party imports resolve to the installed packages. The helper remaps any `sys.path`
//! entry that points back into the repository (an editable install) under the queried
//! revision's root.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use refactor_diff_core::split_lines;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::bundle::Bundle;
use super::{
    Kind, LibraryFiles, Location, MAX_RESULTS, NavigationError, finish, line_of, posix, read_text,
    relative_to, revision_root,
};
use crate::exec::Tools;
use crate::snapshots::Snapshots;

const VENV_DIRS: &[&str] = &[".venv", "venv", "env"];
/// Time for the helper to import Jedi and report ready.
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-query limit; the first query in a large project loads a lot of modules.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Lines of the helper's stderr kept for error messages.
const STDERR_TAIL: usize = 40;

pub struct JediBackend {
    repo: PathBuf,
    snapshots: Arc<Snapshots>,
    tools: Arc<Tools>,
    python: Option<PathBuf>,
    bundle: Bundle,
    library_files: LibraryFiles,
    helper: tokio::sync::Mutex<Option<Helper>>,
    /// Line caches for files outside the repository (working-tree files can still change).
    lines: Mutex<HashMap<PathBuf, Arc<Vec<String>>>>,
}

/// What the helper reports on startup.
#[derive(Clone, Debug, Deserialize)]
struct Ready {
    ready: bool,
    #[serde(default)]
    python: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Response {
    id: Option<u64>,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Name {
    module_path: Option<String>,
    #[serde(default)]
    module_name: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    is_definition: bool,
}

struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    info: Ready,
    seq: u64,
}

impl JediBackend {
    pub fn new(
        repo: PathBuf,
        snapshots: Arc<Snapshots>,
        tools: Arc<Tools>,
        python: Option<PathBuf>,
        library_files: LibraryFiles,
    ) -> Self {
        Self {
            repo,
            snapshots,
            tools,
            python,
            bundle: Bundle::new(None),
            library_files,
            helper: tokio::sync::Mutex::new(None),
            lines: Mutex::new(HashMap::new()),
        }
    }

    /// Extract the helper bundle under `dir` instead of the user's cache directory.
    pub fn with_cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.bundle = Bundle::new(Some(dir.into()));
        self
    }

    // --- environment -------------------------------------------------------------------

    /// The interpreter the helper runs in.
    fn find_python(&self) -> Result<PathBuf, NavigationError> {
        if let Some(given) = &self.python {
            let path = expand_user(given);
            if path.is_dir() {
                let inside = venv_python(&path);
                if inside.is_file() {
                    return Ok(inside);
                }
            } else if path.is_file() {
                return Ok(path);
            } else if let Some(found) = given.to_str().and_then(|name| self.tools.which(name)) {
                return Ok(found);
            }
            return Err(NavigationError(format!(
                "Can't use Python environment {}: no interpreter found there.",
                given.display()
            )));
        }
        for name in VENV_DIRS {
            let venv = self.repo.join(name);
            if venv.join("pyvenv.cfg").is_file() {
                let python = venv_python(&venv);
                if python.is_file() {
                    return Ok(python);
                }
            }
        }
        if let Some(found) = self
            .tools
            .which("python3")
            .or_else(|| self.tools.which("python"))
        {
            return Ok(found);
        }
        Err(NavigationError::new(
            "Python navigation needs a Python interpreter: pass --python PATH, create .venv in \
             the repository, or put python3 on PATH.",
        ))
    }

    pub async fn describe_environment(&self) -> Result<String, NavigationError> {
        let mut guard = self.helper.lock().await;
        let helper = self.ensure_started(&mut guard).await?;
        Ok(format!(
            "Python {} at {}",
            helper.info.python, helper.info.prefix
        ))
    }

    // --- process -----------------------------------------------------------------------

    async fn ensure_started<'a>(
        &self,
        slot: &'a mut Option<Helper>,
    ) -> Result<&'a mut Helper, NavigationError> {
        let alive = matches!(slot.as_mut().map(|h| h.child.try_wait()), Some(Ok(None)));
        if !alive {
            *slot = None;
            *slot = Some(self.spawn().await?);
        }
        Ok(slot.as_mut().expect("just started"))
    }

    async fn spawn(&self) -> Result<Helper, NavigationError> {
        let python = self.find_python()?;
        let bundle = self.bundle.dir().await?;
        let fail = |detail: String| {
            NavigationError(format!(
                "Can't use Python environment {}: {detail}",
                python.display()
            ))
        };
        let mut cmd = tokio::process::Command::from(self.tools.command(&python));
        cmd.arg(bundle.join("helper.py"))
            .arg(bundle.join("vendor"))
            .env("PYTHONIOENCODING", "utf-8")
            .current_dir(&self.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| fail(e.to_string()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let stderr = child.stderr.take().expect("piped stderr");
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        tokio::spawn(drain_stderr(stderr, Arc::clone(&tail)));

        let mut helper = Helper {
            child,
            stdin: Some(stdin),
            stdout,
            stderr: tail,
            info: Ready {
                ready: false,
                python: String::new(),
                prefix: String::new(),
                error: None,
            },
            seq: 0,
        };
        let mut line = String::new();
        let read = tokio::time::timeout(READY_TIMEOUT, helper.stdout.read_line(&mut line)).await;
        match read {
            Err(_) => {
                return Err(fail(format!(
                    "the Jedi helper didn't start within {}s{}",
                    READY_TIMEOUT.as_secs(),
                    helper.stderr_suffix()
                )));
            }
            Ok(Err(e)) => return Err(fail(format!("{e}{}", helper.stderr_suffix()))),
            Ok(Ok(0)) => {
                tokio::time::sleep(Duration::from_millis(50)).await; // let stderr land
                return Err(fail(format!(
                    "the Jedi helper exited before it was ready{}",
                    helper.stderr_suffix()
                )));
            }
            Ok(Ok(_)) => {}
        }
        let info: Ready = serde_json::from_str(line.trim())
            .map_err(|e| fail(format!("unexpected helper output ({e}): {}", line.trim())))?;
        if !info.ready {
            return Err(fail(
                info.error.unwrap_or_else(|| "helper not ready".into()),
            ));
        }
        tracing::debug!(python = %python.display(), version = %info.python, "jedi helper started");
        helper.info = info;
        Ok(helper)
    }

    /// Stop the helper: close its stdin, give it two seconds, then kill it.
    pub async fn close(&self) {
        let helper = self.helper.lock().await.take();
        if let Some(mut helper) = helper {
            drop(helper.stdin.take());
            if tokio::time::timeout(Duration::from_secs(2), helper.child.wait())
                .await
                .is_err()
            {
                let _ = helper.child.kill().await;
            }
        }
    }

    // --- queries -----------------------------------------------------------------------

    pub async fn definitions(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        self.query(sha, path, "definitions", line, col).await
    }

    pub async fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        self.query(sha, path, "references", line, col).await
    }

    async fn query(
        &self,
        sha: Option<&str>,
        path: &str,
        cmd: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        let root = revision_root(&self.snapshots, sha).await?;
        let file = root.join(path);
        if !file.is_file() {
            return Err(NavigationError(format!(
                "{path} doesn't exist at this revision."
            )));
        }
        let request = json!({
            "cmd": cmd,
            "root": root,
            "repo": self.repo,
            "file": file,
            "line": line,
            "col": col,
            "limit": MAX_RESULTS,
        });
        let response = {
            let mut guard = self.helper.lock().await;
            let helper = self.ensure_started(&mut guard).await?;
            match helper.request(request).await {
                Ok(response) => response,
                Err(e) => {
                    *guard = None; // kill_on_drop; the next query respawns
                    return Err(e);
                }
            }
        };
        if !response.ok {
            let error = response.error.unwrap_or_default();
            return Err(match response.kind.as_deref() {
                Some("position") => NavigationError(error),
                _ => NavigationError(format!("Jedi failed: {error}")),
            });
        }
        let names: Vec<Name> = response
            .result
            .and_then(|r| r.get("names").cloned())
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| NavigationError(format!("Jedi failed: unexpected result ({e})")))?
            .unwrap_or_default();
        let locations = names
            .into_iter()
            .take(MAX_RESULTS)
            .map(|name| self.location(name, &root))
            .collect();
        Ok(finish(locations))
    }

    fn location(&self, name: Name, root: &Path) -> Location {
        let base = |kind, path, text| Location {
            kind,
            path,
            line: name.line,
            col: name.col,
            name: name.name.clone(),
            kind_name: name.kind.clone(),
            text,
            is_definition: name.is_definition,
        };
        let Some(module) = name.module_path.as_deref() else {
            return base(
                Kind::Builtin,
                name.module_name.clone().unwrap_or_default(),
                String::new(),
            );
        };
        let module = PathBuf::from(module);
        let text = self.line_text(&module, name.line);
        if let Some(rel) = relative_to(&module, root) {
            return base(Kind::Repo, posix(&rel), text);
        }
        self.library_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(module.clone());
        base(Kind::Library, module.to_string_lossy().into_owned(), text)
    }

    fn line_text(&self, file: &Path, line: Option<u32>) -> String {
        if line.is_none() {
            return String::new();
        }
        let cached = self
            .lines
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(file)
            .cloned();
        let lines = match cached {
            Some(lines) => lines,
            None => {
                let lines = Arc::new(read_text(file).map(|t| split_lines(&t)).unwrap_or_default());
                if !file.starts_with(&self.repo) {
                    self.lines
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(file.to_path_buf(), Arc::clone(&lines));
                }
                lines
            }
        };
        line_of(&lines, line)
    }
}

impl Helper {
    /// Send one request and wait for its response, skipping anything else the helper
    /// writes. Any failure means the process is unusable and must be dropped.
    async fn request(&mut self, mut request: Value) -> Result<Response, NavigationError> {
        self.seq += 1;
        let id = self.seq;
        request["id"] = json!(id);
        let mut line = serde_json::to_string(&request).expect("serializable request");
        line.push('\n');
        let stopped =
            |detail: String| NavigationError(format!("Jedi stopped responding: {detail}"));
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(stopped("helper closed".into()));
        };
        let exchange = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
            let mut buf = String::new();
            loop {
                buf.clear();
                if self.stdout.read_line(&mut buf).await? == 0 {
                    return Err(std::io::Error::other("the helper exited"));
                }
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Response>(trimmed) {
                    Ok(response) if response.id == Some(id) => return Ok(response),
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::debug!(line = trimmed, "unparseable jedi helper output: {e}");
                        continue;
                    }
                }
            }
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, exchange).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => Err(stopped(format!("{e}{}", self.stderr_suffix()))),
            Err(_) => {
                let _ = self.child.kill().await;
                Err(stopped(format!(
                    "no answer within {}s{}",
                    REQUEST_TIMEOUT.as_secs(),
                    self.stderr_suffix()
                )))
            }
        }
    }

    fn stderr_suffix(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        if tail.is_empty() {
            String::new()
        } else {
            format!("\n{}", tail.iter().cloned().collect::<Vec<_>>().join("\n"))
        }
    }
}

async fn drain_stderr(stderr: tokio::process::ChildStderr, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
        if tail.len() == STDERR_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// The interpreter inside a virtualenv (or any prefix) directory.
fn venv_python(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        dir.join("Scripts").join("python.exe")
    } else {
        dir.join("bin").join("python")
    }
}

/// `~` and `~/...` → the home directory.
pub(crate) fn expand_user(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let rest = if text == "~" {
        Some("")
    } else {
        text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\"))
    };
    match (rest, directories::BaseDirs::new()) {
        (Some(rest), Some(dirs)) => dirs.home_dir().join(rest),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_user_handles_tilde() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        assert_eq!(expand_user(Path::new("~")), home);
        assert_eq!(expand_user(Path::new("~/x/y")), home.join("x/y"));
        assert_eq!(
            expand_user(Path::new("/abs/~/x")),
            PathBuf::from("/abs/~/x")
        );
        assert_eq!(expand_user(Path::new("~user/x")), PathBuf::from("~user/x"));
    }

    #[test]
    fn responses_parse() {
        let r: Response = serde_json::from_str(
            r#"{"id": 3, "ok": false, "kind": "position", "error": "`line` parameter is not in a valid range."}"#,
        )
        .unwrap();
        assert_eq!(r.id, Some(3));
        assert!(!r.ok);
        assert_eq!(r.kind.as_deref(), Some("position"));
        let n: Name = serde_json::from_str(
            r#"{"module_path": null, "module_name": "builtins", "line": null, "col": null, "name": "len", "type": "function", "is_definition": true}"#,
        )
        .unwrap();
        assert!(n.module_path.is_none());
        assert_eq!(n.module_name.as_deref(), Some("builtins"));
        assert_eq!(n.kind, "function");
    }
}
