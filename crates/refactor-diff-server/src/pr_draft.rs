//! The pending review for a pull request: inline comments collected while reading, kept on
//! disk until they are submitted together with Comment / Approve / Request changes.
//!
//! One file per repository and PR under `<config>/pr-drafts/`, apart from `reviews/` so the
//! review-mark files keep their bytes.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::paths::{atomic_write, canonical_json, config_dir, now_iso};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DraftComment {
    pub id: String,
    pub path: String,
    pub line: i64,
    /// `LEFT` or `RIGHT`.
    pub side: String,
    pub hunk_id: Option<String>,
    pub body: String,
    pub created: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    /// The head the comments' lines refer to.
    pub head_sha: Option<String>,
    #[serde(default)]
    pub comments: Vec<DraftComment>,
    #[serde(default)]
    pub next_id: u64,
}

pub struct DraftStore {
    dir: PathBuf,
    repo_key: String,
    lock: Mutex<()>,
}

impl DraftStore {
    /// `root` defaults to the XDG config dir.
    pub fn new(repo: &Path, root: Option<&Path>) -> Self {
        let resolved = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        let root = root.map(Path::to_path_buf).unwrap_or_else(config_dir);
        Self {
            dir: root.join("pr-drafts"),
            repo_key: refactor_diff_core::short_hash([resolved.to_string_lossy()]),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self, pr: i64) -> PathBuf {
        self.dir.join(format!("{}-{pr}.json", self.repo_key))
    }

    fn read(&self, pr: i64) -> Draft {
        std::fs::read_to_string(self.path(pr))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write(&self, pr: i64, draft: &Draft) -> std::io::Result<()> {
        if draft.comments.is_empty() {
            return match std::fs::remove_file(self.path(pr)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let value = serde_json::to_value(draft).expect("draft serializes");
        atomic_write(&self.path(pr), &canonical_json(&value), None)
    }

    pub fn load(&self, pr: i64) -> Draft {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read(pr)
    }

    /// Add a comment; the draft takes `head_sha` if it is new.
    pub fn add(
        &self,
        pr: i64,
        head_sha: Option<&str>,
        mut comment: DraftComment,
    ) -> std::io::Result<Draft> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut draft = self.read(pr);
        if draft.comments.is_empty() {
            draft.head_sha = head_sha.map(str::to_string);
        }
        draft.next_id += 1;
        comment.id = format!("d{}", draft.next_id);
        comment.created = now_iso();
        draft.comments.push(comment);
        self.write(pr, &draft)?;
        Ok(draft)
    }

    /// Change a comment's body; `None` when there is no such comment.
    pub fn edit(&self, pr: i64, id: &str, body: &str) -> std::io::Result<Option<Draft>> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut draft = self.read(pr);
        let Some(c) = draft.comments.iter_mut().find(|c| c.id == id) else {
            return Ok(None);
        };
        c.body = body.to_string();
        self.write(pr, &draft)?;
        Ok(Some(draft))
    }

    pub fn remove(&self, pr: i64, id: &str) -> std::io::Result<Draft> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut draft = self.read(pr);
        draft.comments.retain(|c| c.id != id);
        self.write(pr, &draft)?;
        Ok(draft)
    }

    pub fn discard(&self, pr: i64) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write(pr, &Draft::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(body: &str) -> DraftComment {
        DraftComment {
            path: "a.py".into(),
            line: 3,
            side: "RIGHT".into(),
            body: body.into(),
            ..Default::default()
        }
    }

    #[test]
    fn add_edit_remove_discard() {
        let dir = tempfile::tempdir().unwrap();
        let store = DraftStore::new(dir.path(), Some(dir.path()));
        let d = store.add(7, Some("abc"), comment("one")).unwrap();
        assert_eq!(d.head_sha.as_deref(), Some("abc"));
        let d = store.add(7, Some("def"), comment("two")).unwrap();
        assert_eq!(
            d.head_sha.as_deref(),
            Some("abc"),
            "the first comment fixes the head"
        );
        assert_eq!(
            d.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["d1", "d2"]
        );
        assert!(store.edit(7, "d1", "uno").unwrap().is_some());
        assert!(store.edit(7, "zz", "x").unwrap().is_none());
        let fresh = DraftStore::new(dir.path(), Some(dir.path()));
        assert_eq!(
            fresh.load(7).comments[0].body,
            "uno",
            "drafts survive a restart"
        );
        assert!(fresh.load(8).comments.is_empty(), "drafts are per PR");
        let d = store.remove(7, "d1").unwrap();
        assert_eq!(d.comments.len(), 1);
        store.discard(7).unwrap();
        assert!(!store.path(7).exists());
        assert_eq!(store.load(7), Draft::default());
    }
}
