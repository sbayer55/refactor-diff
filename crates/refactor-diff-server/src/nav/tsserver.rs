//! TypeScript and JavaScript navigation through tsserver (TypeScript's language server, the
//! one editors use).
//!
//! One long-lived process answers queries for every revision: each side of a diff is a
//! separate project root, a snapshot of the commit or the working tree. Snapshots link the
//! repository's `node_modules` in, so imported packages resolve to the installed versions,
//! like the virtualenv does for Python.
//!
//! tsserver is found via `--tsserver`, then the repository's own `node_modules/typescript`,
//! then a `tsserver` on PATH.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use refactor_diff_core::split_lines;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::jedi::expand_user;
use super::{
    Kind, LibraryFiles, Location, MAX_RESULTS, NavigationError, finish, posix, read_text,
    relative_to, revision_root,
};
use crate::exec::Tools;
use crate::snapshots::Snapshots;

/// The first query in a large project loads the whole program.
const TIMEOUT: Duration = Duration::from_secs(60);
const INSTALL_HINT: &str = "TypeScript navigation needs tsserver: install TypeScript in the \
                            repository (npm install --save-dev typescript) or pass --tsserver \
                            PATH.";

pub struct TsServerBackend {
    repo: PathBuf,
    snapshots: Arc<Snapshots>,
    tools: Arc<Tools>,
    tsserver: Option<PathBuf>,
    /// The resolved command line, cached once discovery succeeds.
    command: Mutex<Option<Vec<OsString>>>,
    library_files: LibraryFiles,
    process: tokio::sync::Mutex<Option<TsProcess>>,
}

struct TsProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: u64,
}

impl TsServerBackend {
    pub fn new(
        repo: PathBuf,
        snapshots: Arc<Snapshots>,
        tools: Arc<Tools>,
        tsserver: Option<PathBuf>,
        library_files: LibraryFiles,
    ) -> Self {
        Self {
            repo,
            snapshots,
            tools,
            tsserver,
            command: Mutex::new(None),
            library_files,
            process: tokio::sync::Mutex::new(None),
        }
    }

    // --- discovery -----------------------------------------------------------------------

    fn command(&self) -> Result<Vec<OsString>, NavigationError> {
        let mut cached = self.command.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(command) = cached.as_ref() {
            return Ok(command.clone());
        }
        let command = self.find()?;
        *cached = Some(command.clone());
        Ok(command)
    }

    fn find(&self) -> Result<Vec<OsString>, NavigationError> {
        let mut candidates = Vec::new();
        if let Some(given) = &self.tsserver {
            candidates.push(expand_user(given));
        }
        candidates.push(
            self.repo
                .join("node_modules")
                .join("typescript")
                .join("lib")
                .join("tsserver.js"),
        );
        for mut path in candidates {
            if path.is_dir() {
                path = path.join("lib").join("tsserver.js"); // a typescript package directory
            }
            if path.extension().and_then(|e| e.to_str()) == Some("js") && path.is_file() {
                let node = find_node(&self.tools).ok_or_else(|| {
                    NavigationError::new("TypeScript navigation needs Node.js on PATH.")
                })?;
                return Ok(vec![node.into_os_string(), path.into_os_string()]);
            }
            if path.is_file() {
                return Ok(vec![path.into_os_string()]);
            }
        }
        if let Some(given) = &self.tsserver {
            return Err(NavigationError(format!(
                "Can't find tsserver at {}.",
                given.display()
            )));
        }
        match self.tools.which("tsserver") {
            Some(found) => Ok(vec![found.into_os_string()]),
            None => Err(NavigationError::new(INSTALL_HINT)),
        }
    }

    pub fn describe_environment(&self) -> Result<String, NavigationError> {
        let command = self.command()?;
        let script = PathBuf::from(command.last().expect("non-empty command"));
        let script = script.canonicalize().unwrap_or(script);
        let mut version = String::new();
        for parent in script.ancestors().skip(1) {
            let manifest = parent.join("package.json");
            if manifest.is_file() {
                if let Some(v) = std::fs::read_to_string(&manifest)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|json| json.get("version")?.as_str().map(str::to_owned))
                {
                    version = format!(" {v}");
                }
                break;
            }
        }
        Ok(format!("TypeScript{version} at {}", script.display()))
    }

    // --- process -------------------------------------------------------------------------

    async fn ensure_started<'a>(
        &self,
        slot: &'a mut Option<TsProcess>,
    ) -> Result<&'a mut TsProcess, NavigationError> {
        let alive = matches!(slot.as_mut().map(|p| p.child.try_wait()), Some(Ok(None)));
        if !alive {
            *slot = None;
            let command = self.command()?;
            let mut cmd = tokio::process::Command::from(self.tools.command(&command[0]));
            cmd.args(&command[1..])
                .arg("--disableAutomaticTypingAcquisition")
                .current_dir(&self.repo)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut child = cmd
                .spawn()
                .map_err(|e| NavigationError(format!("Can't start tsserver: {e}")))?;
            let stdin = child.stdin.take().expect("piped stdin");
            let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
            *slot = Some(TsProcess {
                child,
                stdin,
                stdout,
                seq: 0,
            });
        }
        Ok(slot.as_mut().expect("just started"))
    }

    /// Ask tsserver to exit, give it two seconds, then kill it.
    pub async fn close(&self) {
        let process = self.process.lock().await.take();
        if let Some(mut process) = process {
            if matches!(process.child.try_wait(), Ok(None)) {
                let exited = async {
                    process.send("exit", json!({})).await?;
                    process.child.wait().await
                };
                if tokio::time::timeout(Duration::from_secs(2), exited)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .is_none()
                {
                    let _ = process.child.kill().await;
                }
            }
        }
    }

    // --- queries -------------------------------------------------------------------------

    pub async fn definitions(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        self.query(sha, path, line, col, "definition").await
    }

    pub async fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Result<Vec<Location>, NavigationError> {
        self.query(sha, path, line, col, "references").await
    }

    async fn query(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
        command: &str,
    ) -> Result<Vec<Location>, NavigationError> {
        let root = revision_root(&self.snapshots, sha).await?;
        let file = root.join(path);
        if !file.is_file() {
            return Err(NavigationError(format!(
                "{path} doesn't exist at this revision."
            )));
        }
        let lines = read_text(&file)
            .map(|t| split_lines(&t))
            .map_err(|e| NavigationError(format!("Couldn't read {path}: {e}")))?;
        let Some(source) = line.checked_sub(1).and_then(|i| lines.get(i as usize)) else {
            return Err(NavigationError(format!("Line {line} is outside {path}.")));
        };
        let offset = utf16_len(source.chars().take(col as usize)) + 1;

        let response = {
            let mut guard = self.process.lock().await;
            let process = self.ensure_started(&mut guard).await?;
            let exchange = async {
                process
                    .send("open", json!({"file": file, "projectRootPath": root}))
                    .await?;
                let response = process
                    .request(
                        command,
                        json!({"file": file, "line": line, "offset": offset}),
                    )
                    .await?;
                // Close so the next query re-reads the file.
                process.send("close", json!({"file": file})).await?;
                Ok::<_, std::io::Error>(response)
            };
            match tokio::time::timeout(TIMEOUT, exchange).await {
                Ok(Ok(response)) => response,
                Ok(Err(e)) => {
                    *guard = None; // kill_on_drop
                    return Err(NavigationError(format!("tsserver stopped responding: {e}")));
                }
                Err(_) => {
                    if let Some(p) = guard.as_mut() {
                        let _ = p.child.kill().await;
                    }
                    *guard = None;
                    return Err(NavigationError(format!(
                        "tsserver stopped responding: no answer within {}s",
                        TIMEOUT.as_secs()
                    )));
                }
            }
        };

        if response.get("success").and_then(Value::as_bool) != Some(true) {
            return Ok(Vec::new()); // e.g. "No content available." on whitespace or a keyword
        }
        let body = response.get("body").cloned().unwrap_or(Value::Null);
        let items: Vec<Value> = if command == "references" {
            let name = body
                .get("symbolName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            body.get("refs")
                .and_then(Value::as_array)
                .map(|refs| {
                    refs.iter()
                        .map(|r| {
                            let mut item = r.clone();
                            item["name"] = Value::String(name.clone());
                            item
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            body.as_array()
                .map(|defs| {
                    defs.iter()
                        .map(|d| {
                            let mut item = d.clone();
                            item["isDefinition"] = Value::Bool(true);
                            item
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let locations = items
            .iter()
            .take(MAX_RESULTS)
            .map(|item| self.location(item, &root))
            .collect();
        Ok(finish(locations))
    }

    fn location(&self, item: &Value, root: &Path) -> Location {
        let file = PathBuf::from(item.get("file").and_then(Value::as_str).unwrap_or(""));
        let line = item["start"]["line"].as_u64().unwrap_or(0) as u32;
        let start_offset = item["start"]["offset"].as_u64().unwrap_or(1) as usize;
        let lines = read_text(&file)
            .map(|t| split_lines(&t))
            .unwrap_or_default();
        let in_file = line
            .checked_sub(1)
            .and_then(|i| lines.get(i as usize))
            .cloned();
        let text = match item.get("lineText").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => in_file.clone().unwrap_or_default(),
        };
        let source = in_file.unwrap_or_else(|| text.clone());
        let col = char_col(&source, start_offset.saturating_sub(1));
        let mut name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if name.is_empty() && item["end"]["line"].as_u64() == Some(u64::from(line)) {
            let end_offset = item["end"]["offset"].as_u64().unwrap_or(1) as usize;
            let end = char_col(&source, end_offset.saturating_sub(1));
            name = source
                .chars()
                .skip(col)
                .take(end.saturating_sub(col))
                .collect();
        }
        let is_definition = item.get("isDefinition").and_then(Value::as_bool) == Some(true);
        let base = |kind, path| Location {
            kind,
            path,
            line: Some(line),
            col: Some(col as u32),
            name: name.clone(),
            kind_name: if is_definition {
                "definition"
            } else {
                "reference"
            }
            .to_string(),
            text: text.clone(),
            is_definition,
        };
        if let Some(rel) = relative_to(&file, root) {
            if !rel.components().any(|c| c.as_os_str() == "node_modules") {
                return base(Kind::Repo, posix(&rel));
            }
        }
        self.library_files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file.clone());
        base(Kind::Library, file.to_string_lossy().into_owned())
    }
}

impl TsProcess {
    /// Write one request line; returns its sequence number.
    async fn send(&mut self, command: &str, arguments: Value) -> std::io::Result<u64> {
        self.seq += 1;
        let message = json!({
            "seq": self.seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let mut line = serde_json::to_string(&message).expect("serializable request");
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(self.seq)
    }

    /// Send a request and wait for its response, skipping events and other responses.
    async fn request(&mut self, command: &str, arguments: Value) -> std::io::Result<Value> {
        let seq = self.send(command, arguments).await?;
        loop {
            let message = read_message(&mut self.stdout).await?;
            if message.get("type").and_then(Value::as_str) == Some("response")
                && message.get("request_seq").and_then(Value::as_u64) == Some(seq)
            {
                return Ok(message);
            }
        }
    }
}

/// One `Content-Length`-framed JSON message from tsserver's stdout.
async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Value> {
    let mut length: Option<usize> = None;
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header).await? == 0 {
            return Err(std::io::Error::other("tsserver exited"));
        }
        let trimmed = header.trim();
        if trimmed.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse().map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("bad Content-Length {value:?}: {e}"),
                    )
                })?);
            }
        }
    }
    let mut body = vec![0u8; length.unwrap_or(0)];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `node` from PATH, else the newest nvm install: nvm only puts node on PATH in interactive
/// shells.
pub(crate) fn find_node(tools: &Tools) -> Option<PathBuf> {
    if let Some(found) = tools.which("node") {
        return Some(found);
    }
    let nvm = std::env::var_os("NVM_DIR")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".nvm")))?;
    newest_nvm_node(&nvm.join("versions").join("node"))
}

fn newest_nvm_node(versions: &Path) -> Option<PathBuf> {
    let mut installs: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('v'))
        .map(|e| (version(&e.file_name().to_string_lossy()), e.path()))
        .filter(|(_, p)| p.join("bin").join("node").is_file())
        .collect();
    installs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    installs.pop().map(|(_, p)| p.join("bin").join("node"))
}

/// `v20.11.1` → `[20, 11, 1]`; anything non-numeric sorts first.
fn version(name: &str) -> Vec<u64> {
    name.trim_start_matches('v')
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .unwrap_or_default()
}

fn utf16_len(chars: impl Iterator<Item = char>) -> usize {
    chars.map(char::len_utf16).sum()
}

/// tsserver counts UTF-16 code units; tokens and the UI count characters.
fn char_col(line: &str, utf16_col: usize) -> usize {
    let mut units = 0;
    for (i, ch) in line.chars().enumerate() {
        if units >= utf16_col {
            return i;
        }
        units += ch.len_utf16();
    }
    line.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_conversions() {
        let line = "a😀b = c";
        assert_eq!(utf16_len(line.chars()), 8);
        assert_eq!(utf16_len(line.chars().take(2)), 3);
        assert_eq!(char_col(line, 0), 0);
        assert_eq!(char_col(line, 1), 1);
        assert_eq!(char_col(line, 3), 2);
        assert_eq!(char_col(line, 2), 2); // inside the surrogate pair rounds up
        assert_eq!(char_col(line, 99), 7);
        assert_eq!(char_col("", 5), 0);
    }

    #[test]
    fn versions_sort_numerically() {
        assert_eq!(version("v20.11.1"), vec![20, 11, 1]);
        assert_eq!(version("v9.0.0"), vec![9, 0, 0]);
        assert!(version("v9.0.0") < version("v20.11.1"));
        assert_eq!(version("junk"), Vec::<u64>::new());
        assert!(version("junk") < version("v0.1.0"));
    }

    #[test]
    fn newest_nvm_install_wins() {
        let dir = tempfile::tempdir().unwrap();
        for v in ["v9.0.0", "v20.11.1", "v24.19.0", "junk", "v100.0.0"] {
            let bin = dir.path().join(v).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            if v != "v100.0.0" {
                std::fs::write(bin.join("node"), "").unwrap(); // v100 has no binary
            }
        }
        assert_eq!(
            newest_nvm_node(dir.path()),
            Some(dir.path().join("v24.19.0/bin/node"))
        );
        assert_eq!(newest_nvm_node(&dir.path().join("missing")), None);
    }

    #[tokio::test]
    async fn framing_parser_skips_events_and_reads_bodies() {
        let body1 = r#"{"seq":0,"type":"event","event":"projectLoadingStart","body":{}}"#;
        let body2 = r#"{"seq":1,"type":"response","request_seq":2,"success":true,"body":[]}"#;
        let stream = format!(
            "Content-Length: {}\r\n\r\n{}\nContent-Length: {}\r\n\r\n{}\n",
            body1.len() + 1,
            body1,
            body2.len() + 1,
            body2
        );
        let mut reader = BufReader::new(std::io::Cursor::new(stream.into_bytes()));
        let first = read_message(&mut reader).await.unwrap();
        assert_eq!(first["type"], "event");
        let second = read_message(&mut reader).await.unwrap();
        assert_eq!(second["request_seq"], 2);
        let err = read_message(&mut reader).await.unwrap_err();
        assert_eq!(err.to_string(), "tsserver exited");
    }

    #[tokio::test]
    async fn framing_parser_tolerates_other_headers_and_blank_lines() {
        let body = r#"{"type":"response","request_seq":1,"success":false}"#;
        let stream = format!(
            "\n\nX-Other: 1\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut reader = BufReader::new(std::io::Cursor::new(stream.into_bytes()));
        let msg = read_message(&mut reader).await.unwrap();
        assert_eq!(msg["success"], false);
    }

    #[test]
    fn install_hint_matches_the_python_tool() {
        assert_eq!(
            INSTALL_HINT,
            "TypeScript navigation needs tsserver: install TypeScript in the repository \
             (npm install --save-dev typescript) or pass --tsserver PATH."
        );
    }
}
