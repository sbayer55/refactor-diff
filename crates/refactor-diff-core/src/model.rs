//! The report model shared by the engine and the HTTP API.
//!
//! Ids are derived from content and positions so they stay stable across re-runs of the same
//! diff; review marks and future actions reference units and hunks by these ids. The JSON
//! produced here is read by the browser UI, so field names and enum strings are part of the
//! contract.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

pub use crate::categories::Category;

/// What kind of mechanical edit a signature describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignatureKind {
    Formatting,
    Rename,
    Retype,
    Replace,
    /// Comment or docstring only.
    Docs,
    /// A block deleted in one place and inserted in another.
    Move,
    /// A call's or def's argument list changed shape (added kwarg, ...).
    Args,
    /// An import changed module path or gained/lost a name.
    Import,
}

impl SignatureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            SignatureKind::Formatting => "formatting",
            SignatureKind::Rename => "rename",
            SignatureKind::Retype => "retype",
            SignatureKind::Replace => "replace",
            SignatureKind::Docs => "docs",
            SignatureKind::Move => "move",
            SignatureKind::Args => "args",
            SignatureKind::Import => "import",
        }
    }

    /// Kinds that are never logic changes, so they count as mechanical even when they don't
    /// repeat.
    pub const fn always_mechanical(self) -> bool {
        matches!(
            self,
            SignatureKind::Formatting | SignatureKind::Docs | SignatureKind::Move
        )
    }
}

/// What a change unit is, so the UI can filter it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitTag {
    /// Touches import statements only.
    Imports,
    /// An import update that only follows a renamed/moved file.
    FileMove,
    /// Half of a certain move of whole functions/classes.
    Moved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WarningKind {
    MissedRename,
    InconsistentRename,
    NearMiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    #[serde(rename = "A")]
    Added,
    #[serde(rename = "M")]
    Modified,
    #[serde(rename = "D")]
    Deleted,
    #[serde(rename = "R")]
    Renamed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineType {
    #[serde(rename = " ")]
    Context,
    #[serde(rename = "-")]
    Removed,
    #[serde(rename = "+")]
    Added,
}

/// One changed file as loaded from the diff source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub old_text: String,
    pub new_text: String,
}

/// A normalized description of one mechanical edit.
///
/// `key` decides grouping; `old`/`new` are display text from the first occurrence.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature {
    pub kind: SignatureKind,
    pub key: String,
    pub old: String,
    pub new: String,
    /// e.g. "call", "attribute", "param user_id"
    pub detail: String,
}

impl Signature {
    pub fn new(
        kind: SignatureKind,
        key: impl Into<String>,
        old: impl Into<String>,
        new: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            key: key.into(),
            old: old.into(),
            new: new.into(),
            detail: String::new(),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn label(&self) -> String {
        use SignatureKind::*;
        match self.kind {
            Formatting => "Whitespace / layout only".to_string(),
            Docs => "Comments & docstrings".to_string(),
            Move => {
                let where_ = if self.old == self.new {
                    format!("within {}", self.old)
                } else {
                    format!("{} → {}", self.old, self.new)
                };
                format!("moved {}: {where_}", self.detail)
            }
            Replace | Import if self.old.is_empty() => format!("insert {}", self.new),
            Replace | Import if self.new.is_empty() => format!("delete {}", self.old),
            _ => format!("{} → {}", self.old, self.new),
        }
    }
}

/// A source line with the column ranges to highlight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    pub hl: Vec<[u32; 2]>,
}

/// A mechanical pattern this unit almost, but not quite, matches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NearMiss {
    pub group_id: String,
    pub score: f64,
    pub hint: String,
}

/// One classified change: a contiguous block of removed and/or added lines.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Unit {
    pub id: String,
    pub path: String,
    pub hunk_id: String,
    /// First old line (1-based); for insertions, the old line after the insertion point.
    pub old_start: u32,
    pub new_start: u32,
    pub old: Vec<Line>,
    pub new: Vec<Line>,
    #[serde(serialize_with = "signature_keys")]
    pub signatures: Vec<Signature>,
    pub explained: bool,
    /// The other half of a move.
    pub partner: Option<String>,
    /// AST-identical after normalization.
    pub verified: bool,
    pub near: Vec<NearMiss>,
    pub tags: Vec<UnitTag>,
}

fn signature_keys<S: Serializer>(sigs: &[Signature], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(sigs.iter().map(|sig| &sig.key))
}

impl Unit {
    pub fn signature_kinds(&self) -> impl Iterator<Item = SignatureKind> + '_ {
        self.signatures.iter().map(|s| s.kind)
    }

    pub fn has_tag(&self, tag: UnitTag) -> bool {
        self.tags.contains(&tag)
    }

    pub fn add_tag(&mut self, tag: UnitTag) {
        if !self.has_tag(tag) {
            self.tags.push(tag);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HunkLine {
    #[serde(rename = "type")]
    pub kind: LineType,
    pub text: String,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub unit: Option<String>,
    pub hl: Vec<[u32; 2]>,
}

impl HunkLine {
    pub fn is_change(&self) -> bool {
        self.kind != LineType::Context
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    pub id: String,
    pub path: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<HunkLine>,
    pub unit_ids: Vec<String>,
    /// Hash of the changed lines' text (context and line numbers excluded): the same edit keeps
    /// its fingerprint when lines above it shift, so review marks survive new commits.
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub key: String,
    pub kind: SignatureKind,
    pub label: String,
    pub old: String,
    pub new: String,
    pub unit_ids: Vec<String>,
    pub files: Vec<String>,
    /// Counts per detail, most common first.
    pub details: IndexMap<String, usize>,
    pub mechanical: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub path: String,
    pub line: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
    pub group_id: Option<String>,
    pub locations: Vec<Location>,
    /// All matches found; `locations` may be truncated.
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub analyzed: bool,
    pub additions: u32,
    pub deletions: u32,
    pub units: usize,
    pub residual_units: usize,
    pub parse_ok: bool,
    pub category: Category,
    /// Renamed/moved with identical content.
    pub pure_rename: bool,
}

/// What was compared, as resolved by the caller. Serialized verbatim as `report.source`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub label: String,
    pub base: String,
    pub head: String,
    /// The merge-base (or the base commit itself for the working tree).
    pub base_sha: String,
    /// `None` for the working tree.
    pub head_sha: Option<String>,
    /// The pull request, as returned by `gh`, if any.
    pub pr: Option<serde_json::Value>,
    /// `pr:N`, `worktree:<base>` or `refs:<base>:<head>`.
    pub identity: String,
    #[serde(default)]
    pub min_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub files_changed: usize,
    pub files_analyzed: usize,
    pub units: usize,
    pub explained_units: usize,
    pub residual_units: usize,
    pub collapsed_pct: u32,
    pub mechanical_groups: usize,
    pub moves: usize,
    pub verified_units: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub id: String,
    pub source: Source,
    pub files: Vec<FileSummary>,
    pub groups: Vec<Group>,
    pub units: IndexMap<String, Unit>,
    pub hunks: IndexMap<String, Hunk>,
    pub residual_hunk_ids: Vec<String>,
    pub warnings: Vec<Warning>,
    /// `(old_text, new_text)` per path, exactly as analyzed. Kept for the file viewer so it
    /// always lines up with the units; not serialized.
    pub texts: BTreeMap<String, (String, String)>,
}

impl Report {
    pub fn stats(&self) -> Stats {
        let total = self.units.len();
        let explained = self.units.values().filter(|u| u.explained).count();
        Stats {
            files_changed: self.files.len(),
            files_analyzed: self.files.iter().filter(|f| f.analyzed).count(),
            units: total,
            explained_units: explained,
            residual_units: total - explained,
            collapsed_pct: if total > 0 {
                (100.0 * explained as f64 / total as f64).round_ties_even() as u32
            } else {
                0
            },
            mechanical_groups: self.groups.iter().filter(|g| g.mechanical).count(),
            moves: self
                .groups
                .iter()
                .filter(|g| g.kind == SignatureKind::Move)
                .count(),
            verified_units: self.units.values().filter(|u| u.verified).count(),
        }
    }

    pub fn file(&self, path: &str) -> Option<&FileSummary> {
        self.files.iter().find(|f| f.path == path)
    }

    pub fn group(&self, id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }
}

impl Serialize for Report {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Report", 9)?;
        st.serialize_field("id", &self.id)?;
        st.serialize_field("source", &self.source)?;
        st.serialize_field("stats", &self.stats())?;
        st.serialize_field("files", &self.files)?;
        st.serialize_field("groups", &self.groups)?;
        st.serialize_field("units", &self.units)?;
        st.serialize_field("hunks", &self.hunks)?;
        st.serialize_field("residual_hunk_ids", &self.residual_hunk_ids)?;
        st.serialize_field("warnings", &self.warnings)?;
        st.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_python() {
        let s = Signature::new(SignatureKind::Rename, "k", "a", "b");
        assert_eq!(s.label(), "a → b");
        assert_eq!(
            Signature::new(SignatureKind::Formatting, "k", "", "").label(),
            "Whitespace / layout only"
        );
        assert_eq!(
            Signature::new(SignatureKind::Replace, "k", "", "x").label(),
            "insert x"
        );
        assert_eq!(
            Signature::new(SignatureKind::Import, "k", "x", "").label(),
            "delete x"
        );
        let m = Signature::new(SignatureKind::Move, "k", "a.py", "a.py").with_detail("f");
        assert_eq!(m.label(), "moved f: within a.py");
        let m = Signature::new(SignatureKind::Move, "k", "a.py", "b.py").with_detail("block");
        assert_eq!(m.label(), "moved block: a.py → b.py");
    }

    #[test]
    fn enums_serialize_to_the_contract_strings() {
        assert_eq!(
            serde_json::to_string(&SignatureKind::Retype).unwrap(),
            "\"retype\""
        );
        assert_eq!(
            serde_json::to_string(&UnitTag::FileMove).unwrap(),
            "\"file-move\""
        );
        assert_eq!(
            serde_json::to_string(&WarningKind::NearMiss).unwrap(),
            "\"near-miss\""
        );
        assert_eq!(
            serde_json::to_string(&FileStatus::Renamed).unwrap(),
            "\"R\""
        );
        assert_eq!(serde_json::to_string(&LineType::Context).unwrap(), "\" \"");
    }

    #[test]
    fn collapsed_pct_rounds_half_to_even() {
        let mut units = IndexMap::new();
        for i in 0..8 {
            let u = Unit {
                id: i.to_string(),
                path: "a".into(),
                hunk_id: "h".into(),
                old_start: 1,
                new_start: 1,
                old: vec![],
                new: vec![],
                signatures: vec![],
                explained: i < 1,
                partner: None,
                verified: false,
                near: vec![],
                tags: vec![],
            };
            units.insert(i.to_string(), u);
        }
        let report = Report {
            id: "r".into(),
            source: Source {
                label: String::new(),
                base: String::new(),
                head: String::new(),
                base_sha: String::new(),
                head_sha: None,
                pr: None,
                identity: String::new(),
                min_count: 2,
            },
            files: vec![],
            groups: vec![],
            units,
            hunks: IndexMap::new(),
            residual_hunk_ids: vec![],
            warnings: vec![],
            texts: BTreeMap::new(),
        };
        // 100 * 1 / 8 = 12.5 -> 12 (banker's rounding), like Python's round().
        assert_eq!(report.stats().collapsed_pct, 12);
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("texts").is_none());
        assert_eq!(json["stats"]["units"], 8);
    }
}
