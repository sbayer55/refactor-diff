//! Review UI preferences (layout, highlighting, panes, per-repository filters).
//!
//! The UI used to keep these in the browser's localStorage, which is scoped to the page's
//! origin: `http://127.0.0.1:<port>`. The port is random for the CLI and only usually the
//! same for the desktop app, so preferences kept disappearing. They live in `ui.json` next to
//! the review marks as a flat `{key: string}` map, with localStorage as a cache.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;

use crate::paths::{atomic_write, canonical_json, config_dir};

pub const PREFIX: &str = "refactor-diff:";
pub const MAX_BYTES: usize = 256 * 1024;

pub type Prefs = BTreeMap<String, String>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct PrefsError(pub String);

pub struct PrefsStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl PrefsStore {
    /// `root` defaults to the XDG config dir.
    pub fn new(root: Option<&Path>) -> Self {
        Self {
            root: root.map(Path::to_path_buf).unwrap_or_else(config_dir),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.root.join("ui.json")
    }

    /// The stored preferences (never fails on a missing or broken file). Entries outside the
    /// UI's namespace, or with non-string values, are dropped.
    pub fn load(&self) -> Prefs {
        let Ok(text) = std::fs::read_to_string(self.path()) else {
            return Prefs::new();
        };
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
            return Prefs::new();
        };
        map.into_iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) if k.starts_with(PREFIX) => Some((k, s)),
                _ => None,
            })
            .collect()
    }

    /// Apply `changes` (a `null` value deletes the key) and return the result. Fails for
    /// keys outside the UI's namespace, non-string values, or a result past [`MAX_BYTES`].
    pub fn update(&self, changes: &Value) -> Result<Prefs, PrefsError> {
        let Value::Object(changes) = changes else {
            return Err(PrefsError(
                "Expected an object of preference changes.".into(),
            ));
        };
        for (key, value) in changes {
            if !key.starts_with(PREFIX) {
                return Err(PrefsError(format!(
                    "Preference keys must start with '{PREFIX}'."
                )));
            }
            if !(value.is_null() || value.is_string()) {
                return Err(PrefsError(format!("{key} must be a string or null.")));
            }
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut data = self.load();
        for (key, value) in changes {
            match value {
                Value::Null => {
                    data.remove(key);
                }
                Value::String(s) => {
                    data.insert(key.clone(), s.clone());
                }
                _ => unreachable!("validated above"),
            }
        }
        let value = serde_json::to_value(&data).expect("string map serializes");
        let text = canonical_json(&value);
        if text.len() > MAX_BYTES {
            return Err(PrefsError("Too many preferences stored.".into()));
        }
        atomic_write(&self.path(), &text, None)
            .map_err(|e| PrefsError(format!("Couldn't save preferences: {e}")))?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, PrefsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = PrefsStore::new(Some(dir.path()));
        (dir, store)
    }

    #[test]
    fn round_trip_and_delete() {
        let (_dir, store) = store();
        assert!(store.load().is_empty());
        store
            .update(&json!({"refactor-diff:layout": "split", "refactor-diff:panes": "{}"}))
            .unwrap();
        assert_eq!(
            store.load(),
            Prefs::from([
                ("refactor-diff:layout".into(), "split".into()),
                ("refactor-diff:panes".into(), "{}".into())
            ])
        );
        let left = store.update(&json!({"refactor-diff:panes": null})).unwrap();
        assert_eq!(
            left,
            Prefs::from([("refactor-diff:layout".into(), "split".into())])
        );
        assert_eq!(store.load(), left);
    }

    #[test]
    fn corrupt_or_foreign_file_is_ignored() {
        let (_dir, store) = store();
        std::fs::write(store.path(), "{not json").unwrap();
        assert!(store.load().is_empty());
        std::fs::write(store.path(), r#"{"other": "x", "refactor-diff:a": 1}"#).unwrap();
        assert!(store.load().is_empty());
        store.update(&json!({"refactor-diff:b": "y"})).unwrap();
        assert_eq!(
            store.load(),
            Prefs::from([("refactor-diff:b".into(), "y".into())])
        );
    }

    #[test]
    fn rejects_bad_changes() {
        let (_dir, store) = store();
        for bad in [
            json!({"theme": "dark"}),
            json!({"refactor-diff:layout": 3}),
            json!(["refactor-diff:layout"]),
            json!(null),
        ] {
            assert!(store.update(&bad).is_err(), "{bad}");
            assert!(!store.path().exists());
        }
        assert_eq!(
            store.update(&json!({"theme": "dark"})).unwrap_err().0,
            "Preference keys must start with 'refactor-diff:'."
        );
        assert_eq!(
            store
                .update(&json!({"refactor-diff:layout": 3}))
                .unwrap_err()
                .0,
            "refactor-diff:layout must be a string or null."
        );
    }

    #[test]
    fn size_cap() {
        let (_dir, store) = store();
        let big = "x".repeat(MAX_BYTES + 1);
        assert_eq!(
            store
                .update(&json!({"refactor-diff:big": big}))
                .unwrap_err()
                .0,
            "Too many preferences stored."
        );
    }
}
