//! Scores that outlive the server, per repository, keyed by hunk fingerprint: a hunk keeps
//! its score when lines above it shift or the report is re-run, and loses it when its
//! content changes. Kept in its own file (`jev/<repo hash>.json`) so the review marks file
//! stays byte-identical to what the Python tool wrote.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::questions::QUESTION_SET_VERSION;
use super::risk::HunkRisk;
use crate::paths::{atomic_write, canonical_json, config_dir};

/// Newest records kept per repository.
pub const MAX_ENTRIES: usize = 5000;

#[derive(Serialize, Deserialize)]
struct StoreFile {
    question_set: String,
    #[serde(default)]
    hunks: HashMap<String, HunkRisk>,
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

    fn read(&self) -> HashMap<String, HunkRisk> {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<StoreFile>(&text).ok())
            .filter(|f| f.question_set == QUESTION_SET_VERSION)
            .map(|f| f.hunks)
            .unwrap_or_default()
    }

    /// Every cached score (empty when the file is missing, broken or from another question
    /// set), by fingerprint.
    pub fn load(&self) -> HashMap<String, HunkRisk> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read()
    }

    /// Add or replace records, keeping the newest [`MAX_ENTRIES`].
    pub fn put(&self, records: &[HunkRisk]) -> std::io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut hunks = self.read();
        for r in records {
            hunks.insert(r.fingerprint.clone(), r.clone());
        }
        if hunks.len() > MAX_ENTRIES {
            let mut by_age: Vec<(String, String)> = hunks
                .iter()
                .map(|(fp, r)| (r.scored_at.clone(), fp.clone()))
                .collect();
            by_age.sort();
            for (_, fp) in by_age.into_iter().take(hunks.len() - MAX_ENTRIES) {
                hunks.remove(&fp);
            }
        }
        let file = StoreFile {
            question_set: QUESTION_SET_VERSION.to_string(),
            hunks,
        };
        let value = serde_json::to_value(&file).expect("records serialize");
        atomic_write(&self.path, &canonical_json(&value), None)
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
}
