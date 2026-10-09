//! Analyzable sources of a revision written to a temporary directory.
//!
//! Code navigation (Jedi, tsserver) needs real files laid out as they were at a commit; for a
//! branch or PR, neither side is checked out. A snapshot holds only the Python, TypeScript
//! and JavaScript sources plus the project files tsserver reads (package.json,
//! tsconfig.json), which takes well under a second even for large repos, and is reused for
//! the lifetime of the server. Each package's `node_modules` is linked in from the
//! repository, since it isn't in git.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use refactor_diff_core::SNAPSHOT_SUFFIXES;

use crate::git::{Git, SourceError};

const PROJECT_FILES: &[&str] = &["package.json", "jsconfig.json"];
/// Blobs per `git cat-file --batch` call.
const BATCH: usize = 500;

pub struct Snapshots {
    git: Git,
    dir: PathBuf,
    /// Set when the directory is ours to delete.
    owned: Option<tempfile::TempDir>,
    roots: Mutex<HashMap<String, PathBuf>>,
}

impl Snapshots {
    /// Snapshots under a fresh `refactor-diff-*` temp dir, or under `cache_dir` when given.
    pub fn new(git: Git, cache_dir: Option<PathBuf>) -> std::io::Result<Self> {
        let (dir, owned) = match cache_dir {
            Some(dir) => {
                std::fs::create_dir_all(&dir)?;
                (dir, None)
            }
            None => {
                let tmp = tempfile::Builder::new()
                    .prefix("refactor-diff-")
                    .tempdir()?;
                (tmp.path().to_path_buf(), Some(tmp))
            }
        };
        Ok(Self {
            git,
            dir,
            owned,
            roots: Mutex::new(HashMap::new()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn repo(&self) -> &Path {
        self.git.repo()
    }

    /// Directory holding the revision's sources; the repo itself for the working tree.
    /// Blocking: runs git. Call from a blocking thread.
    pub fn root(&self, sha: Option<&str>) -> Result<PathBuf, SourceError> {
        let Some(sha) = sha else {
            return Ok(self.git.repo().to_path_buf());
        };
        let mut roots = self.roots.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = roots.get(sha) {
            return Ok(p.clone());
        }
        let root = self.write(sha)?;
        roots.insert(sha.to_string(), root.clone());
        Ok(root)
    }

    /// Delete every snapshot. Idempotent.
    pub fn close(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        self.roots.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn write(&self, sha: &str) -> Result<PathBuf, SourceError> {
        let dest = self.dir.join(sha);
        let paths: Vec<String> = self
            .git
            .ls_tree(sha)?
            .into_iter()
            .filter(|p| wanted(p))
            .collect();
        for chunk in paths.chunks(BATCH) {
            let specs: Vec<String> = chunk.iter().map(|p| format!("{sha}:{p}")).collect();
            for (spec, data) in self.git.read_blobs(&specs)? {
                let rel = spec.split_once(':').map(|(_, p)| p).unwrap_or(&spec);
                let file = dest.join(rel);
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent).map_err(io_err)?;
                }
                std::fs::write(&file, data).map_err(io_err)?;
            }
        }
        std::fs::create_dir_all(&dest).map_err(io_err)?;
        for path in &paths {
            if path.rsplit('/').next() == Some("package.json") {
                let package_dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                self.link_node_modules(&dest, package_dir);
            }
        }
        Ok(dest)
    }

    fn link_node_modules(&self, dest: &Path, package_dir: &str) {
        let installed = self.git.repo().join(package_dir).join("node_modules");
        let link = dest.join(package_dir).join("node_modules");
        if installed.is_dir() && std::fs::symlink_metadata(&link).is_err() {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(&installed, &link);
            #[cfg(windows)]
            let _ = std::os::windows::fs::symlink_dir(&installed, &link);
        }
    }
}

impl Drop for Snapshots {
    fn drop(&mut self) {
        if self.owned.is_some() {
            self.close();
        }
    }
}

fn io_err(e: std::io::Error) -> SourceError {
    SourceError::Message(format!("Couldn't write snapshot: {e}"))
}

fn wanted(path: &str) -> bool {
    if path.split('/').any(|c| c == "node_modules") {
        return false; // vendored packages; the installed ones are linked in instead
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    SNAPSHOT_SUFFIXES.iter().any(|s| path.ends_with(s))
        || PROJECT_FILES.contains(&name)
        || (name.starts_with("tsconfig") && name.ends_with(".json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wanted_paths() {
        assert!(wanted("a.py"));
        assert!(wanted("src/x.tsx"));
        assert!(wanted("package.json"));
        assert!(wanted("sub/tsconfig.build.json"));
        assert!(!wanted("node_modules/x/index.js"));
        assert!(!wanted("README.md"));
        assert!(!wanted("tsconfig.txt"));
    }
}
