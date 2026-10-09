//! Load the files changed between two git revisions, a GitHub PR, or the working tree.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use refactor_diff_core::{FileChange, FileStatus, HeadFiles, Source};
use serde::Serialize;
use serde_json::Value;

use super::parse::{cat_file_batch, name_status_z};
use super::{Git, GitHub, SourceError};

pub const WORKTREE: &str = ":worktree:";

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedSource {
    pub label: String,
    /// What the user asked for.
    pub base: String,
    pub head: String,
    /// Merge-base actually diffed against.
    pub base_sha: String,
    /// `None` for the working tree.
    pub head_sha: Option<String>,
    pub pr: Option<Value>,
}

impl ResolvedSource {
    /// What the comparison *is*, independent of the commits it currently resolves to, so
    /// review state keyed on it survives new commits: the PR, or the ref names.
    pub fn identity(&self) -> String {
        if let Some(pr) = &self.pr {
            return format!(
                "pr:{}",
                pr.get("number").map(|n| n.to_string()).unwrap_or_default()
            );
        }
        if self.head == WORKTREE {
            return format!("worktree:{}", self.base);
        }
        format!("refs:{}:{}", self.base, self.head)
    }

    pub fn to_core(&self) -> Source {
        Source {
            label: self.label.clone(),
            base: self.base.clone(),
            head: self.head.clone(),
            base_sha: self.base_sha.clone(),
            head_sha: self.head_sha.clone(),
            pr: self.pr.clone(),
            identity: self.identity(),
            min_count: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SourcesInfo {
    pub repo: String,
    pub branches: Vec<String>,
    pub current: String,
    pub default_base: String,
    pub prs: Vec<Value>,
    pub pr_error: Option<String>,
}

fn is_sha(r: &str) -> bool {
    r.len() >= 7
        && r.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

impl Git {
    pub fn list_sources(&self, gh: &dyn GitHub) -> Result<SourcesInfo, SourceError> {
        let branches: Vec<String> = self
            .text(&[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                "refs/heads",
                "refs/remotes",
            ])?
            .lines()
            .filter(|b| !b.is_empty() && !b.ends_with("/HEAD"))
            .map(String::from)
            .collect();
        let current = self
            .text(&["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "HEAD".into());
        let default_base = ["main", "master", "origin/main", "origin/master"]
            .iter()
            .find(|b| branches.iter().any(|x| x == *b))
            .map(|b| b.to_string())
            .unwrap_or_else(|| branches.first().cloned().unwrap_or_else(|| "HEAD".into()));
        let (prs, pr_error) = if gh.available() {
            match gh.pr_list() {
                Ok(prs) => (prs, None),
                Err(e) => (vec![], Some(e.to_string())),
            }
        } else {
            (vec![], Some("gh CLI not found".to_string()))
        };
        Ok(SourcesInfo {
            repo: self.repo().to_string_lossy().into_owned(),
            branches,
            current,
            default_base,
            prs,
            pr_error,
        })
    }

    pub fn resolve(
        &self,
        base: Option<&str>,
        head: Option<&str>,
        pr: Option<i64>,
        gh: &dyn GitHub,
    ) -> Result<ResolvedSource, SourceError> {
        if let Some(number) = pr {
            return self.resolve_pr(number, gh);
        }
        let base = base
            .filter(|b| !b.is_empty())
            .ok_or_else(|| SourceError::Message("Choose a base ref to compare against.".into()))?;
        let head = head.filter(|h| !h.is_empty()).unwrap_or("HEAD");
        let base_sha = self.resolve_ref(base)?;
        if head == WORKTREE {
            return Ok(ResolvedSource {
                label: format!("{base} → working tree"),
                base: base.into(),
                head: head.into(),
                base_sha,
                head_sha: None,
                pr: None,
            });
        }
        let head_sha = self.resolve_ref(head)?;
        let merge_base = self.text(&["merge-base", &base_sha, &head_sha])?;
        let mut label = format!("{base}...{head}");
        if base == format!("{head}^") && is_sha(head) {
            // One commit on its own (see the commits view): name it like git log does.
            let subject = self.text(&["log", "-1", "--format=%s", &head_sha])?;
            label = format!("commit {} {subject}", &head[..7]);
        }
        Ok(ResolvedSource {
            label,
            base: base.into(),
            head: head.into(),
            base_sha: merge_base,
            head_sha: Some(head_sha),
            pr: None,
        })
    }

    fn resolve_pr(&self, number: i64, gh: &dyn GitHub) -> Result<ResolvedSource, SourceError> {
        let info = gh.pr_view(number)?;
        let (base_sha, head_sha) = (info.base_ref_oid.clone(), info.head_ref_oid.clone());
        if !self.has_commit(&head_sha) {
            self.fetch(&format!("pull/{number}/head"));
        }
        if !self.has_commit(&base_sha) {
            self.fetch(&info.base_ref_name);
        }
        for sha in [&base_sha, &head_sha] {
            if !self.has_commit(sha) {
                let short: String = sha.chars().take(10).collect();
                return Err(SourceError::Message(format!(
                    "Could not fetch commit {short} for PR #{number}."
                )));
            }
        }
        let merge_base = self.text(&["merge-base", &base_sha, &head_sha])?;
        Ok(ResolvedSource {
            label: format!("#{number} {}", info.title),
            base: info.base_ref_name.clone(),
            head: info.head_ref_name.clone(),
            base_sha: merge_base,
            head_sha: Some(head_sha),
            pr: Some(info.raw),
        })
    }

    /// Resolve `r` to a commit, falling back to a remote branch of the same name. A branch
    /// that was never checked out locally only exists as `origin/<ref>`; if even that is
    /// missing, the branch is fetched from the remote.
    pub fn resolve_ref(&self, r: &str) -> Result<String, SourceError> {
        if let Ok(sha) = self.rev(r) {
            return Ok(sha);
        }
        let remotes = self.remotes();
        for remote in &remotes {
            if let Ok(sha) = self.rev(&format!("{remote}/{r}")) {
                return Ok(sha);
            }
        }
        for remote in &remotes {
            let refspec = format!("+refs/heads/{r}:refs/remotes/{remote}/{r}");
            if self.run(&["fetch", "--quiet", remote, &refspec]).is_ok() {
                if let Ok(sha) = self.rev(&format!("{remote}/{r}")) {
                    return Ok(sha);
                }
            }
        }
        let searched: Vec<String> = remotes.iter().map(|rm| format!("{rm}/{r}")).collect();
        let quoted = refactor_diff_core::lang::pyrepr::py_repr_str(r);
        Err(SourceError::Message(if remotes.is_empty() {
            format!("Unknown ref {quoted}: not a local ref.")
        } else {
            format!(
                "Unknown ref {quoted}: not a local ref, and not found as {} (also tried fetching it).",
                searched.join(", ")
            )
        }))
    }

    fn fetch(&self, refspec: &str) {
        for remote in self.remotes() {
            if self.run(&["fetch", "--quiet", &remote, refspec]).is_ok() {
                return;
            }
        }
    }

    pub fn load_changes(&self, src: &ResolvedSource) -> Result<Vec<FileChange>, SourceError> {
        let mut args = vec!["diff", "--name-status", "-M", "-z", src.base_sha.as_str()];
        if let Some(head) = &src.head_sha {
            args.push(head);
        }
        let entries = name_status_z(&self.run(&args)?);

        let mut wanted: Vec<String> = Vec::new();
        for e in &entries {
            if e.status != FileStatus::Added {
                if let Some(old) = &e.old {
                    wanted.push(format!("{}:{old}", src.base_sha));
                }
            }
            if e.status != FileStatus::Deleted {
                if let Some(head) = &src.head_sha {
                    wanted.push(format!("{head}:{}", e.new));
                }
            }
        }
        let blobs = self.read_blobs(&wanted)?;

        let mut changes = Vec::new();
        for e in entries {
            let old_bytes: Vec<u8> = match (&e.status, &e.old) {
                (FileStatus::Added, _) => vec![],
                (_, Some(old)) => blobs
                    .get(&format!("{}:{old}", src.base_sha))
                    .cloned()
                    .unwrap_or_default(),
                _ => vec![],
            };
            let new_bytes: Vec<u8> = if e.status == FileStatus::Deleted {
                vec![]
            } else if let Some(head) = &src.head_sha {
                blobs
                    .get(&format!("{head}:{}", e.new))
                    .cloned()
                    .unwrap_or_default()
            } else {
                let file = self.repo().join(&e.new);
                if file.is_file() {
                    std::fs::read(&file).unwrap_or_default()
                } else {
                    vec![]
                }
            };
            if old_bytes.contains(&0) || new_bytes.contains(&0) {
                continue; // binary
            }
            changes.push(FileChange {
                path: e.new.clone(),
                old_path: if e.status == FileStatus::Renamed {
                    e.old.clone()
                } else {
                    None
                },
                status: e.status,
                old_text: String::from_utf8_lossy(&old_bytes).into_owned(),
                new_text: String::from_utf8_lossy(&new_bytes).into_owned(),
            });
        }
        Ok(changes)
    }

    /// Contents of `paths` at commit `sha`, or in the working tree when `sha` is `None`.
    /// Missing files are left out.
    pub fn read_files(
        &self,
        sha: Option<&str>,
        paths: &[&str],
    ) -> Result<BTreeMap<String, String>, SourceError> {
        let Some(sha) = sha else {
            let mut out = BTreeMap::new();
            for p in paths {
                if let Some(file) = worktree_file(self.repo(), p) {
                    if let Ok(bytes) = std::fs::read(&file) {
                        out.insert(p.to_string(), String::from_utf8_lossy(&bytes).into_owned());
                    }
                }
            }
            return Ok(out);
        };
        let specs: Vec<String> = paths.iter().map(|p| format!("{sha}:{p}")).collect();
        let blobs = self.read_blobs(&specs)?;
        Ok(blobs
            .into_iter()
            .map(|(spec, bytes)| {
                let path = spec
                    .split_once(':')
                    .map(|(_, p)| p.to_string())
                    .unwrap_or(spec);
                (path, String::from_utf8_lossy(&bytes).into_owned())
            })
            .collect())
    }

    pub fn read_blobs(&self, specs: &[String]) -> Result<HashMap<String, Vec<u8>>, SourceError> {
        if specs.is_empty() {
            return Ok(HashMap::new());
        }
        let input = specs.join("\n") + "\n";
        let out = self.run_input(&["cat-file", "--batch"], Some(input.as_bytes()))?;
        Ok(cat_file_batch(&out, specs))
    }

    /// `git ls-tree -r --name-only`: every path at `sha`.
    pub fn ls_tree(&self, sha: &str) -> Result<Vec<String>, SourceError> {
        let out = self.run(&["ls-tree", "-r", "-z", "--name-only", sha])?;
        Ok(String::from_utf8_lossy(&out)
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect())
    }

    /// Files (at `head_sha`, or in the working tree) containing `word` as a whole word.
    pub fn grep_files(&self, head_sha: Option<&str>, word: &str, pathspec: &[&str]) -> Vec<String> {
        let mut args = vec!["grep", "-l", "-w", "-F", "-e", word];
        if let Some(head) = head_sha {
            args.push(head);
        }
        args.push("--");
        args.extend(pathspec);
        let Ok((code, out)) = self.status(&args) else {
            return vec![];
        };
        if code != 0 && code != 1 {
            return vec![];
        }
        let text = String::from_utf8_lossy(&out);
        text.lines()
            .map(|f| {
                if head_sha.is_some() {
                    f.split_once(':').map_or(f, |(_, p)| p).to_string()
                } else {
                    f.to_string()
                }
            })
            .collect()
    }
}

/// A path inside the working tree, or `None` when it is missing or escapes the repository.
pub fn worktree_file(repo: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    let full = repo.join(p);
    if !full.is_file() {
        return None;
    }
    let resolved = full.canonicalize().ok()?;
    let root = repo.canonicalize().ok()?;
    resolved.starts_with(&root).then_some(full)
}

/// The head revision as the engine sees it, for leftover-reference checks.
pub struct HeadLookup<'a> {
    pub git: &'a Git,
    pub head_sha: Option<&'a str>,
}

impl HeadFiles for HeadLookup<'_> {
    fn grep_word(&self, word: &str, globs: &[&str]) -> Vec<String> {
        self.git.grep_files(self.head_sha, word, globs)
    }

    fn read(&self, paths: &[&str]) -> HashMap<String, String> {
        self.git
            .read_files(self.head_sha, paths)
            .map(|m| m.into_iter().collect())
            .unwrap_or_default()
    }
}
