//! Review state that outlives the server: which patterns and hunks were marked reviewed, and
//! what the diff looked like last time, so that re-running after new commits can say what is
//! new.
//!
//! Stored per repository under the config dir, keyed by the *identity* of a comparison (the PR
//! number or the ref names) rather than by commit, so marks survive a push. Hunks are tracked
//! by content fingerprint, so a reviewed hunk stays reviewed when lines above it shift and
//! stops being reviewed when its content changes.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::{atomic_write, canonical_json, config_dir, now_iso};

/// Hunk fingerprints of one report, in hunk order, with the hunk's path.
pub type Fingerprints = IndexMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Previous {
    pub head_sha: Option<String>,
    #[serde(default)]
    pub seen: Fingerprints,
    #[serde(default)]
    pub hunks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub head_sha: Option<String>,
    pub updated: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub hunks: Vec<String>,
    #[serde(default)]
    pub seen: Fingerprints,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Previous>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Changed {
    pub fingerprint: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub prev_head: Option<String>,
    pub new: Vec<String>,
    pub changed_reviewed: Vec<Changed>,
}

/// The state the UI needs for one report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub identity: String,
    pub groups: Vec<String>,
    pub hunks: Vec<String>,
    pub delta: Delta,
}

/// `{"add": [...], "remove": [...]}`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MarkChange {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    repo: Option<Value>,
    #[serde(default)]
    comparisons: BTreeMap<String, Entry>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

pub struct ReviewStore {
    repo: PathBuf,
    path: PathBuf,
    lock: Mutex<()>,
    clock: Box<dyn Fn() -> String + Send + Sync>,
}

impl ReviewStore {
    /// `root` defaults to the XDG config dir. The file is named after the resolved repo path.
    pub fn new(repo: &Path, root: Option<&Path>) -> Self {
        let resolved = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        let name = refactor_diff_core::short_hash([resolved.to_string_lossy()]);
        let root = root.map(Path::to_path_buf).unwrap_or_else(config_dir);
        Self {
            repo: repo.to_path_buf(),
            path: root.join("reviews").join(format!("{name}.json")),
            lock: Mutex::new(()),
            clock: Box::new(now_iso),
        }
    }

    /// Use a fixed clock for `updated` timestamps (tests).
    pub fn with_clock(mut self, clock: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> StateFile {
        std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<StateFile>(&text).ok())
            .unwrap_or_else(|| StateFile {
                repo: Some(Value::String(self.repo.to_string_lossy().into_owned())),
                comparisons: BTreeMap::new(),
                extra: BTreeMap::new(),
            })
    }

    fn write(&self, data: &StateFile) -> std::io::Result<()> {
        let value = serde_json::to_value(data).expect("state serializes");
        atomic_write(&self.path, &canonical_json(&value), None)
    }

    pub fn load(&self, identity: &str) -> Entry {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.read()
            .comparisons
            .get(identity)
            .cloned()
            .unwrap_or_default()
    }

    /// Add/remove reviewed marks.
    pub fn mark(
        &self,
        identity: &str,
        groups: Option<&MarkChange>,
        hunks: Option<&MarkChange>,
    ) -> std::io::Result<Entry> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.read();
        let mut entry = data.comparisons.get(identity).cloned().unwrap_or_default();
        for (field, change) in [(&mut entry.groups, groups), (&mut entry.hunks, hunks)] {
            let Some(change) = change else { continue };
            let mut current: IndexMap<String, ()> = field.drain(..).map(|x| (x, ())).collect();
            for x in &change.remove {
                current.shift_remove(x);
            }
            for x in &change.add {
                current.entry(x.clone()).or_insert(());
            }
            *field = current.into_keys().collect();
        }
        entry.updated = Some((self.clock)());
        data.comparisons.insert(identity.to_string(), entry.clone());
        self.write(&data)?;
        Ok(entry)
    }

    /// Remember the hunks of this analysis, prune reviewed marks of hunks that no longer exist,
    /// and return the delta against the previous head.
    pub fn record_analysis(
        &self,
        identity: &str,
        head_sha: Option<&str>,
        hunks: &Fingerprints,
    ) -> std::io::Result<Delta> {
        let head = head_sha.unwrap_or("worktree");
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.read();
        let mut entry = data.comparisons.get(identity).cloned().unwrap_or_default();
        let moved_on = entry.head_sha.as_deref().is_some_and(|prev| {
            prev != head
                || (head == "worktree"
                    && entry.seen.keys().collect::<HashSet<_>>()
                        != hunks.keys().collect::<HashSet<_>>())
        });
        if moved_on {
            // Keep the last distinct state so re-analyzing the same head still reports it.
            entry.previous = Some(Previous {
                head_sha: entry.head_sha.clone(),
                seen: entry.seen.clone(),
                hunks: entry.hunks.clone(),
            });
        }
        entry.head_sha = Some(head.to_string());
        entry.seen = hunks.clone();
        entry.hunks.retain(|fp| hunks.contains_key(fp));
        entry.updated = Some((self.clock)());
        data.comparisons.insert(identity.to_string(), entry.clone());
        self.write(&data)?;
        Ok(delta(&entry, hunks))
    }

    /// The state the UI needs for a report whose hunks are `hunks`.
    pub fn review(&self, identity: &str, hunks: &Fingerprints) -> Review {
        let entry = self.load(identity);
        Review {
            identity: identity.to_string(),
            groups: entry.groups.clone(),
            hunks: entry
                .hunks
                .iter()
                .filter(|fp| hunks.contains_key(*fp))
                .cloned()
                .collect(),
            delta: delta(&entry, hunks),
        }
    }
}

pub fn delta(entry: &Entry, hunks: &Fingerprints) -> Delta {
    let Some(prev) = &entry.previous else {
        return Delta {
            prev_head: None,
            new: vec![],
            changed_reviewed: vec![],
        };
    };
    Delta {
        prev_head: prev.head_sha.clone(),
        new: hunks
            .keys()
            .filter(|fp| !prev.seen.contains_key(*fp))
            .cloned()
            .collect(),
        changed_reviewed: prev
            .hunks
            .iter()
            .filter(|fp| !hunks.contains_key(*fp))
            .map(|fp| Changed {
                fingerprint: fp.clone(),
                path: prev.seen.get(fp).cloned().unwrap_or_default(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/goldens");

    fn fps(pairs: &[(&str, &str)]) -> Fingerprints {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn matches_python_bytes_and_results() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(Path::new("/srv/example-repo"), Some(dir.path()))
            .with_clock(|| "2026-10-08T12:00:00+00:00".to_string());
        let identity = "refs:main:feature";
        let d1 = store
            .record_analysis(
                identity,
                Some("aaaa"),
                &fps(&[("fp1", "a.py"), ("fp2", "b.py")]),
            )
            .unwrap();
        assert_eq!(
            d1,
            Delta {
                prev_head: None,
                new: vec![],
                changed_reviewed: vec![]
            }
        );
        let marked = store
            .mark(
                identity,
                Some(&MarkChange {
                    add: vec!["g1".into(), "g2".into()],
                    remove: vec![],
                }),
                Some(&MarkChange {
                    add: vec!["fp1".into()],
                    remove: vec![],
                }),
            )
            .unwrap();
        assert_eq!(marked.groups, vec!["g1", "g2"]);
        assert_eq!(marked.hunks, vec!["fp1"]);
        let final_hunks = fps(&[("fp2", "b.py"), ("fp3", "c.py")]);
        let d2 = store
            .record_analysis(identity, Some("bbbb"), &final_hunks)
            .unwrap();
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{GOLDENS}/review.expected.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&d2).unwrap(),
            expected["record_analysis_2"]
        );
        assert_eq!(
            serde_json::to_value(store.review(identity, &final_hunks)).unwrap(),
            expected["review"]
        );
        assert_eq!(
            store.path().file_name().unwrap().to_str().unwrap(),
            expected["file_name"]
        );
        let written = std::fs::read_to_string(store.path()).unwrap();
        let golden = std::fs::read_to_string(format!("{GOLDENS}/review.sample.json")).unwrap();
        assert_eq!(written, golden);
    }

    #[test]
    fn corrupt_file_is_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(Path::new("/srv/x"), Some(dir.path()));
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), "nope").unwrap();
        assert_eq!(store.load("id"), Entry::default());
        let r = store.review("id", &fps(&[]));
        assert_eq!(r.groups, Vec::<String>::new());
    }

    #[test]
    fn mark_is_an_ordered_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(Path::new("/srv/x"), Some(dir.path()));
        store
            .mark(
                "id",
                Some(&MarkChange {
                    add: vec!["a".into(), "b".into()],
                    remove: vec![],
                }),
                None,
            )
            .unwrap();
        let e = store
            .mark(
                "id",
                Some(&MarkChange {
                    add: vec!["a".into(), "c".into()],
                    remove: vec!["b".into()],
                }),
                None,
            )
            .unwrap();
        assert_eq!(e.groups, vec!["a", "c"]);
        assert!(e.updated.is_some());
        let unknown = store
            .mark(
                "id",
                Some(&MarkChange {
                    add: vec![],
                    remove: vec!["zzz".into()],
                }),
                None,
            )
            .unwrap();
        assert_eq!(unknown.groups, vec!["a", "c"]);
        let _ = json!(null);
    }

    #[test]
    fn worktree_moves_on_when_hunks_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReviewStore::new(Path::new("/srv/x"), Some(dir.path()));
        store
            .record_analysis("w", None, &fps(&[("a", "x.py")]))
            .unwrap();
        store
            .mark(
                "w",
                None,
                Some(&MarkChange {
                    add: vec!["a".into()],
                    remove: vec![],
                }),
            )
            .unwrap();
        let d = store
            .record_analysis("w", None, &fps(&[("a", "x.py")]))
            .unwrap();
        assert_eq!(d.prev_head, None);
        let d = store
            .record_analysis("w", None, &fps(&[("b", "x.py")]))
            .unwrap();
        assert_eq!(d.prev_head.as_deref(), Some("worktree"));
        assert_eq!(d.new, vec!["b"]);
        assert_eq!(
            d.changed_reviewed,
            vec![Changed {
                fingerprint: "a".into(),
                path: "x.py".into()
            }]
        );
    }
}
