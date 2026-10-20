//! Scores that outlive the server, per repository, keyed by hunk fingerprint: a hunk keeps
//! its score when lines above it shift or the report is re-run, and loses it when its
//! content changes. Kept in its own file (`jev/<repo hash>.json`) so the review marks file
//! stays byte-identical to what the Python tool wrote. Group checks live in the same file
//! under `groups`, keyed by group key plus sampled hunk fingerprints, with their own version.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::groups::GroupCheck;
use super::questions::{GROUP_QUESTION_SET_VERSION, QUESTION_SET_VERSION};
use super::risk::HunkRisk;
use crate::paths::{atomic_write, canonical_json, config_dir};

/// Newest records kept per repository (hunks and groups each).
pub const MAX_ENTRIES: usize = 5000;

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    question_set: String,
    #[serde(default)]
    hunks: HashMap<String, HunkRisk>,
    #[serde(default)]
    group_question_set: String,
    #[serde(default)]
    groups: HashMap<String, GroupCheck>,
}

/// Drop the oldest records until at most [`MAX_ENTRIES`] are left.
fn prune<T>(records: &mut HashMap<String, T>, scored_at: impl Fn(&T) -> &str) {
    if records.len() <= MAX_ENTRIES {
        return;
    }
    let mut by_age: Vec<(String, String)> = records
        .iter()
        .map(|(k, r)| (scored_at(r).to_string(), k.clone()))
        .collect();
    by_age.sort();
    for (_, k) in by_age.into_iter().take(records.len() - MAX_ENTRIES) {
        records.remove(&k);
    }
}

pub struct JevStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JevStore {
    /// `root` defaults to the XDG config dir. The file is named after the resolved repo path,
    /// like the review marks.
    pub fn new(repo: &Path, root: Option<&Path>) -> Self {
        let resolved = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        let name = refactor_diff_core::short_hash([resolved.to_string_lossy()]);
        let root = root.map(Path::to_path_buf).unwrap_or_else(config_dir);
        Self {
            path: root.join("jev").join(format!("{name}.json")),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file with each section emptied when it is from another question set.
    fn read(&self) -> StoreFile {
        let mut file = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoreFile>(&text).ok())
            .unwrap_or_default();
        if file.question_set != QUESTION_SET_VERSION {
            file.hunks.clear();
        }
        if file.group_question_set != GROUP_QUESTION_SET_VERSION {
            file.groups.clear();
        }
        file.question_set = QUESTION_SET_VERSION.to_string();
        file.group_question_set = GROUP_QUESTION_SET_VERSION.to_string();
        file
    }

    fn write(&self, file: &StoreFile) -> std::io::Result<()> {
        let value = serde_json::to_value(file).expect("records serialize");
        atomic_write(&self.path, &canonical_json(&value), None)
    }

    /// Every cached score (empty when the file is missing, broken or from another question
    /// set), by fingerprint.
    pub fn load(&self) -> HashMap<String, HunkRisk> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read().hunks
    }

    /// Every cached group check (empty when from another group question set), by cache key.
    pub fn load_groups(&self) -> HashMap<String, GroupCheck> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read().groups
    }

    /// Add or replace records, keeping the newest [`MAX_ENTRIES`].
    pub fn put(&self, records: &[HunkRisk]) -> std::io::Result<()> {
        self.put_all(records, &[])
    }

    /// Add or replace hunk and group records in one write.
    pub fn put_all(&self, records: &[HunkRisk], groups: &[GroupCheck]) -> std::io::Result<()> {
        if records.is_empty() && groups.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read();
        for r in records {
            file.hunks.insert(r.fingerprint.clone(), r.clone());
        }
        for g in groups {
            file.groups.insert(g.cache_key.clone(), g.clone());
        }
        prune(&mut file.hunks, |r| &r.scored_at);
        prune(&mut file.groups, |g| &g.scored_at);
        self.write(&file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn record(fp: &str, at: &str) -> HunkRisk {
        HunkRisk {
            fingerprint: fp.into(),
            hunk_id: format!("h-{fp}"),
            path: "a.py".into(),
            risk: 0.5,
            level: "medium".into(),
            flagged: false,
            primary_concern: "none".into(),
            confidence: 0.9,
            runner_up: None,
            blast_radius: 1.0,
            blast_label: "local".into(),
            reviewer_effort: 0.0,
            effort_label: "glance".into(),
            concerns: vec![],
            behavior_beyond_stated_change: 0.0,
            removes_guard_or_validation: 0.0,
            change_role: None,
            change_role_confidence: 0.0,
            adds_tests: false,
            docs_only: false,
            answers: BTreeMap::new(),
            usage: None,
            cost: None,
            question_set: QUESTION_SET_VERSION.into(),
            scored_at: at.into(),
        }
    }

    #[test]
    fn round_trip_and_version_wipe() {
        let dir = tempfile::tempdir().unwrap();
        let store = JevStore::new(Path::new("/srv/example-repo"), Some(dir.path()));
        assert!(store.load().is_empty());
        store
            .put(&[record("a", "2026-01-01T00:00:00+00:00")])
            .unwrap();
        store
            .put(&[record("b", "2026-01-02T00:00:00+00:00")])
            .unwrap();
        let loaded = store.load();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded["a"].hunk_id, "h-a");
        assert!(store.path().starts_with(dir.path().join("jev")));
        let text = std::fs::read_to_string(store.path()).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        file["question_set"] = serde_json::Value::String("0".into());
        std::fs::write(store.path(), file.to_string()).unwrap();
        assert!(store.load().is_empty());
        std::fs::write(store.path(), "{broken").unwrap();
        assert!(store.load().is_empty());
    }

    fn group(key: &str) -> GroupCheck {
        GroupCheck {
            cache_key: key.into(),
            group_id: "g1".into(),
            same_edit: 0.9,
            sampled: vec!["u1".into()],
            answers: BTreeMap::new(),
            usage: None,
            cost: None,
            question_set: GROUP_QUESTION_SET_VERSION.into(),
            scored_at: "2026-01-01T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn groups_live_beside_hunks_with_their_own_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = JevStore::new(Path::new("/srv/example-repo"), Some(dir.path()));
        store
            .put_all(&[record("a", "2026-01-01T00:00:00+00:00")], &[group("k1")])
            .unwrap();
        store
            .put(&[record("b", "2026-01-02T00:00:00+00:00")])
            .unwrap();
        assert_eq!(store.load().len(), 2);
        assert_eq!(store.load_groups()["k1"].same_edit, 0.9);
        // A new group question set drops the groups but keeps the hunks, and vice versa.
        let text = std::fs::read_to_string(store.path()).unwrap();
        let mut file: serde_json::Value = serde_json::from_str(&text).unwrap();
        file["group_question_set"] = serde_json::Value::String("0".into());
        std::fs::write(store.path(), file.to_string()).unwrap();
        assert!(store.load_groups().is_empty());
        assert_eq!(store.load().len(), 2);
        file["group_question_set"] = GROUP_QUESTION_SET_VERSION.into();
        file["question_set"] = "1".into();
        std::fs::write(store.path(), file.to_string()).unwrap();
        assert!(store.load().is_empty());
        assert_eq!(store.load_groups().len(), 1);
    }
}
