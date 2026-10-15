//! Caches and limits for the file-history API. Everything cached is keyed by a commit sha,
//! which never changes, so entries are never invalidated; the working-tree version bypasses
//! every cache.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lru::LruCache;
use serde::Serialize;

use crate::git::{Blame, HistoryPage};

/// Files larger than this are not served through the history API.
pub const MAX_BLOB_BYTES: usize = 2 * 1024 * 1024;
/// Per side; the line matcher is quadratic in the worst case.
pub const MAX_DIFF_LINES: usize = 60_000;
/// How long one git command may take before the request fails.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;
/// The most commits fetched for one file; older ones are reported as `more`.
pub const MAX_HISTORY: usize = 5000;

/// A line diff between two versions: difflib opcodes plus the hunks they form.
#[derive(Clone, Debug, Serialize)]
pub struct DiffResult {
    pub from_lines: usize,
    pub to_lines: usize,
    /// `[tag, i1, i2, j1, j2]`, 0-based half-open; tag is `=`, `~`, `-` or `+`.
    pub ops: Vec<(char, usize, usize, usize, usize)>,
    pub hunks: Vec<Hunk>,
    pub stats: Stats,
}

#[derive(Clone, Debug, Serialize)]
pub struct Hunk {
    pub a: [usize; 2],
    pub b: [usize; 2],
    pub del: usize,
    pub add: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub add: usize,
    pub del: usize,
}

type Key2 = (String, String);
/// `(from sha, from path, to sha, to path)`.
type DiffKey = (String, String, String, String);

pub struct HistoryCache {
    /// `(head sha, path)` → the whole history (up to [`MAX_HISTORY`]).
    pages: Mutex<LruCache<Key2, Arc<HistoryPage>>>,
    /// `(base, head)` → shas in `base..head`.
    ranges: Mutex<LruCache<Key2, Arc<HashSet<String>>>>,
    /// `(sha, path)` → lines.
    blobs: Mutex<LruCache<Key2, Arc<Vec<String>>>>,
    blames: Mutex<LruCache<Key2, Arc<Blame>>>,
    /// `(from sha, from path, to sha, to path)` → diff.
    diffs: Mutex<LruCache<DiffKey, Arc<DiffResult>>>,
    /// One checkout at a time.
    pub checkout_lock: tokio::sync::Mutex<()>,
}

impl Default for HistoryCache {
    fn default() -> Self {
        let cap = |n: usize| NonZeroUsize::new(n).expect("non-zero");
        Self {
            pages: Mutex::new(LruCache::new(cap(256))),
            ranges: Mutex::new(LruCache::new(cap(8))),
            blobs: Mutex::new(LruCache::new(cap(64))),
            blames: Mutex::new(LruCache::new(cap(64))),
            diffs: Mutex::new(LruCache::new(cap(128))),
            checkout_lock: tokio::sync::Mutex::new(()),
        }
    }
}

macro_rules! cached {
    ($name:ident, $put:ident, $field:ident, $key:ty, $val:ty) => {
        pub fn $name(&self, key: &$key) -> Option<Arc<$val>> {
            self.$field
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(key)
                .cloned()
        }
        pub fn $put(&self, key: $key, value: Arc<$val>) {
            self.$field
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .put(key, value);
        }
    };
}

impl HistoryCache {
    cached!(page, put_page, pages, Key2, HistoryPage);
    cached!(range, put_range, ranges, Key2, HashSet<String>);
    cached!(blob, put_blob, blobs, Key2, Vec<String>);
    cached!(blame, put_blame, blames, Key2, Blame);
    cached!(diff, put_diff, diffs, DiffKey, DiffResult);
}

/// The difflib opcodes between two versions, with their hunks and totals.
pub fn line_diff(a: &[String], b: &[String]) -> DiffResult {
    use refactor_diff_core::Tag;
    let mut ops = Vec::new();
    let mut hunks = Vec::new();
    let mut stats = Stats::default();
    for op in refactor_diff_core::line_matcher(a, b).opcodes() {
        let tag = match op.tag {
            Tag::Equal => '=',
            Tag::Replace => '~',
            Tag::Delete => '-',
            Tag::Insert => '+',
        };
        if op.tag != Tag::Equal {
            let (del, add) = (op.i2 - op.i1, op.j2 - op.j1);
            stats.del += del;
            stats.add += add;
            hunks.push(Hunk {
                a: [op.i1, op.i2],
                b: [op.j1, op.j2],
                del,
                add,
            });
        }
        ops.push((tag, op.i1, op.i2, op.j1, op.j2));
    }
    DiffResult {
        from_lines: a.len(),
        to_lines: b.len(),
        ops,
        hunks,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    #[test]
    fn diff_has_opcodes_hunks_and_stats() {
        let d = line_diff(&lines("a\nb\nc\nd"), &lines("a\nx\nc\nd\ne"));
        assert_eq!(d.from_lines, 4);
        assert_eq!(d.to_lines, 5);
        assert_eq!(d.ops[0], ('=', 0, 1, 0, 1));
        assert_eq!(d.ops[1], ('~', 1, 2, 1, 2));
        assert_eq!(d.ops.last().unwrap().0, '+');
        assert_eq!(d.hunks.len(), 2);
        assert_eq!((d.stats.add, d.stats.del), (2, 1));
    }

    #[test]
    fn cache_round_trip() {
        let c = HistoryCache::default();
        let key = ("abc".to_string(), "a.py".to_string());
        assert!(c.blob(&key).is_none());
        c.put_blob(key.clone(), Arc::new(vec!["x".into()]));
        assert_eq!(c.blob(&key).unwrap().len(), 1);
    }
}
