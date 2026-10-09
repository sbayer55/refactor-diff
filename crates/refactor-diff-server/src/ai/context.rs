//! What the model gets to see: the pieces of context around one location in a report.
//!
//! Everything refactor-diff already knows about a hunk is turned into text here: the hunk
//! with its pattern annotations, the enclosing function on both sides (from the language
//! analyzers' statement spans), references from the navigator, the commits that touched the
//! line, the pull request, and the diff's mechanical patterns. Tasks pick which pieces they
//! need; [`render`] lays them out in a fixed order so prompts stay stable from one request to
//! the next.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use lru::LruCache;
use refactor_diff_core::{
    FileAnalysis, Group, Hunk, LineType, Report, SignatureKind, StmtKind, StmtSpan, Unit,
    analyzer_for, anchor_line,
};
use serde::Serialize;

use super::tasks::Piece;
use crate::git::{Commit, Git, TouchingCommit, line_history_new, line_history_old};

/// Lines per side of the enclosing function.
pub const FUNCTION_LINE_CAP: usize = 400;
pub const REFERENCES_CAP: usize = 50;
pub const PATTERNS_CAP: usize = 20;
const ANALYSIS_CACHE: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ContextError(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub const fn as_str(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }

    pub const fn other(self) -> Side {
        match self {
            Side::Old => Side::New,
            Side::New => Side::Old,
        }
    }

    /// `o`/`old`/`n`/`new`.
    pub fn parse(s: &str) -> Option<Side> {
        match s {
            "o" | "old" => Some(Side::Old),
            "n" | "new" => Some(Side::New),
            _ => None,
        }
    }
}

/// One location in a report: a hunk and a line on one of its sides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Focus {
    #[serde(skip)]
    pub hunk_id: String,
    /// The new path (how the UI names files).
    pub path: String,
    pub side: Side,
    pub line: u32,
}

/// A statement's text on one side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub path: String,
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub truncated: bool,
}

/// The innermost def or class around a focus, on both sides when it exists there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub qualname: String,
    /// `Def` or `Class`.
    pub kind: StmtKind,
    pub old: Option<Span>,
    pub new: Option<Span>,
}

impl Function {
    /// The last segment of the qualified name.
    pub fn name(&self) -> &str {
        self.qualname.rsplit('.').next().unwrap_or(&self.qualname)
    }

    pub fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }
}

/// A reference to a name inside the repository, as the navigator reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RefLocation {
    pub path: String,
    pub line: Option<u32>,
    pub text: String,
    #[serde(rename = "def")]
    pub is_definition: bool,
}

/// Where the code navigator answers "references to the name at this position". Only
/// repository locations are expected (library and builtin hits are left out upstream).
pub trait ReferenceSource: Send + Sync {
    fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefLocation>, String>> + Send + '_>>;
}

/// Where the enclosing def is used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct References {
    pub name: String,
    pub side: Side,
    pub total: usize,
    pub files: usize,
    pub locations: Vec<RefLocation>,
}

/// The navigator failed; the name is kept for the heading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReferencesError {
    pub name: String,
    pub side: Side,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize)]
pub struct History {
    /// Every commit in the range, oldest first.
    pub range: Vec<Commit>,
    /// The commits that touched the focus line.
    pub touching: Vec<TouchingCommit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PullRequest {
    pub number: Option<i64>,
    pub title: String,
    pub url: String,
    pub body: String,
}

/// A piece that was not asked for, asked for but absent, or present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requested<T> {
    No,
    Missing,
    Some(T),
}

impl<T> Requested<T> {
    pub fn as_option(&self) -> Option<&T> {
        match self {
            Requested::Some(t) => Some(t),
            _ => None,
        }
    }

    pub fn is_requested(&self) -> bool {
        !matches!(self, Requested::No)
    }

    fn from_option(requested: bool, value: Option<T>) -> Self {
        match (requested, value) {
            (false, _) => Requested::No,
            (true, None) => Requested::Missing,
            (true, Some(v)) => Requested::Some(v),
        }
    }
}

/// Where a hunk starts, for the hunk block's heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HunkAt {
    pub path: String,
    pub old_start: u32,
    pub new_start: u32,
}

/// The collected pieces for one location.
#[derive(Clone, Debug)]
pub struct Context {
    pub focus: Focus,
    pub hunk_at: HunkAt,
    pub hunk: String,
    pub patterns: Vec<String>,
    pub function: Requested<Function>,
    pub references: Requested<Result<References, ReferencesError>>,
    pub history: Requested<History>,
    pub pr: Requested<PullRequest>,
}

impl Context {
    /// The pieces that were asked for and are present, sorted, for the `meta` event.
    pub fn sent_pieces(&self, pieces: &BTreeSet<Piece>) -> Vec<Piece> {
        pieces
            .iter()
            .copied()
            .filter(|p| match p {
                Piece::Hunk | Piece::Patterns => true,
                Piece::Function => self.function.as_option().is_some(),
                Piece::References => self.references.as_option().is_some(),
                Piece::History => self.history.as_option().is_some(),
                Piece::Pr => self.pr.as_option().is_some(),
            })
            .collect()
    }
}

struct Inner {
    report: Arc<Report>,
    git: Git,
    navigator: Option<Arc<dyn ReferenceSource>>,
    groups_by_key: HashMap<String, usize>,
    groups_by_id: HashMap<String, usize>,
    analyses: Mutex<LruCache<(String, u64), Arc<FileAnalysis>>>,
}

/// Builds context pieces for locations in one report. Cheap to clone (shared state).
#[derive(Clone)]
pub struct ContextBuilder(Arc<Inner>);

impl ContextBuilder {
    pub fn new(report: Arc<Report>, git: Git, navigator: Option<Arc<dyn ReferenceSource>>) -> Self {
        let groups_by_key = report
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.key.clone(), i))
            .collect();
        let groups_by_id = report
            .groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.clone(), i))
            .collect();
        Self(Arc::new(Inner {
            report,
            git,
            navigator,
            groups_by_key,
            groups_by_id,
            analyses: Mutex::new(LruCache::new(
                NonZeroUsize::new(ANALYSIS_CACHE).expect("non-zero cache size"),
            )),
        }))
    }

    pub fn report(&self) -> &Arc<Report> {
        &self.0.report
    }

    pub fn git(&self) -> &Git {
        &self.0.git
    }

    pub fn has_navigator(&self) -> bool {
        self.0.navigator.is_some()
    }

    fn hunk(&self, focus: &Focus) -> &Hunk {
        self.0
            .report
            .hunks
            .get(&focus.hunk_id)
            .expect("a Focus is only built for a hunk in this report")
    }

    // --- focus -------------------------------------------------------------------------------

    /// Validate a location. Without a line, the hunk's anchor line (new side when it has one).
    pub fn focus(
        &self,
        hunk_id: &str,
        side: Option<&str>,
        line: Option<i64>,
    ) -> Result<Focus, ContextError> {
        let Some(hunk) = self.0.report.hunks.get(hunk_id) else {
            return Err(ContextError::new("Unknown hunk; run the analysis again."));
        };
        let Some(line) = line else {
            let at = anchor_line(&self.0.report, hunk);
            let side = if at.new_no.is_some() {
                Side::New
            } else {
                Side::Old
            };
            return Ok(Focus {
                hunk_id: hunk.id.clone(),
                path: hunk.path.clone(),
                side,
                line: at.new_no.or(at.old_no).unwrap_or(1),
            });
        };
        let Some(side) = Side::parse(side.unwrap_or("")) else {
            return Err(ContextError::new("side must be old or new."));
        };
        let known = hunk.lines.iter().any(|ln| {
            let n = match side {
                Side::New => ln.new_no,
                Side::Old => ln.old_no,
            };
            n.is_some_and(|n| i64::from(n) == line)
        });
        if !known {
            return Err(ContextError(format!(
                "Line {line} isn't part of that hunk."
            )));
        }
        Ok(Focus {
            hunk_id: hunk.id.clone(),
            path: hunk.path.clone(),
            side,
            line: line as u32,
        })
    }

    // --- pieces ------------------------------------------------------------------------------

    /// The hunk as a numbered unified diff with annotation lines for pattern-explained
    /// units, verified units and near misses.
    pub fn hunk_piece(&self, focus: &Focus) -> String {
        let r = &self.0.report;
        let mut out: Vec<String> = Vec::new();
        let mut noted: BTreeSet<&str> = BTreeSet::new();
        for ln in &self.hunk(focus).lines {
            let unit = ln.unit.as_deref().and_then(|u| r.units.get(u));
            if let Some(unit) = unit {
                if noted.insert(&unit.id) {
                    for note in self.unit_notes(unit) {
                        out.push(format!("[[{note}]]"));
                    }
                }
            }
            let o = ln.old_no.map(|n| n.to_string()).unwrap_or_default();
            let n = ln.new_no.map(|n| n.to_string()).unwrap_or_default();
            out.push(format!(
                "{o:>5} {n:>5} {} {}",
                line_marker(ln.kind),
                ln.text
            ));
        }
        out.join("\n")
    }

    /// Changed lines that no pattern explains.
    pub fn unexplained_lines(&self, focus: &Focus) -> usize {
        let r = &self.0.report;
        self.hunk(focus)
            .lines
            .iter()
            .filter(|ln| ln.kind != LineType::Context)
            .filter(|ln| {
                !ln.unit
                    .as_deref()
                    .and_then(|u| r.units.get(u))
                    .is_some_and(|u| u.explained)
            })
            .count()
    }

    fn group_by_key(&self, key: &str) -> Option<&Group> {
        self.0
            .groups_by_key
            .get(key)
            .map(|&i| &self.0.report.groups[i])
    }

    fn group_by_id(&self, id: &str) -> Option<&Group> {
        self.0
            .groups_by_id
            .get(id)
            .map(|&i| &self.0.report.groups[i])
    }

    fn unit_notes(&self, unit: &Unit) -> Vec<String> {
        let mut notes = Vec::new();
        for sig in &unit.signatures {
            let Some(g) = self.group_by_key(&sig.key) else {
                continue;
            };
            if g.mechanical {
                notes.push(format!(
                    "pattern: {} {} (×{} in {} files)",
                    g.kind.as_str(),
                    g.label,
                    g.unit_ids.len(),
                    g.files.len()
                ));
            } else {
                notes.push(format!("unique {}: {}", g.kind.as_str(), g.label));
            }
        }
        for near in &unit.near {
            if let Some(g) = self.group_by_id(&near.group_id) {
                notes.push(format!(
                    "near miss: almost {} {} — {}",
                    g.kind.as_str(),
                    g.label,
                    near.hint
                ));
            }
        }
        if unit.verified {
            notes.push(
                "verified: the enclosing statement is the same program on both sides".to_string(),
            );
        }
        notes
    }

    /// The innermost def/class around the focus line, on both sides when it exists.
    pub fn function_piece(&self, focus: &Focus) -> Option<Function> {
        let an = self.analysis(&focus.path, focus.side)?;
        let this_side = innermost(&an.statements, focus.line)?.clone();
        let other = self.counterpart(focus, &this_side, focus.side.other());
        let (old, new) = match focus.side {
            Side::New => (other, Some(this_side.clone())),
            Side::Old => (Some(this_side.clone()), other),
        };
        Some(Function {
            qualname: this_side.qualname.clone(),
            kind: this_side.kind,
            old: old.and_then(|s| self.span_text(&focus.path, Side::Old, &s)),
            new: new.and_then(|s| self.span_text(&focus.path, Side::New, &s)),
        })
    }

    /// The analysis of one side of a file, cached by content.
    fn analysis(&self, path: &str, side: Side) -> Option<Arc<FileAnalysis>> {
        let texts = self.0.report.texts.get(path)?;
        let text = match side {
            Side::Old => &texts.0,
            Side::New => &texts.1,
        };
        if text.is_empty() {
            return None;
        }
        let analyzer = analyzer_for(path)?;
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let key = (path.to_string(), hasher.finish());
        if let Some(hit) = self
            .0
            .analyses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Some(Arc::clone(hit));
        }
        let an = Arc::new(analyzer.analyze(text));
        self.0
            .analyses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, Arc::clone(&an));
        Some(an)
    }

    /// The same def on the other side: by (renamed) qualified name, else by the hunk's
    /// line numbers on that side.
    fn counterpart(&self, focus: &Focus, span: &StmtSpan, other: Side) -> Option<StmtSpan> {
        let an = self.analysis(&focus.path, other)?;
        let renames = self.rename_map(other == Side::Old);
        let wanted: Vec<&str> = span
            .qualname
            .split('.')
            .map(|p| renames.get(p).map(String::as_str).unwrap_or(p))
            .collect();
        let wanted = wanted.join(".");
        for s in an.flat_statements() {
            if s.kind == span.kind && !s.qualname.is_empty() && s.qualname == wanted {
                return Some(s.clone());
            }
        }
        let nums = self.hunk(focus).lines.iter().filter_map(|ln| match other {
            Side::Old => ln.old_no,
            Side::New => ln.new_no,
        });
        for n in nums {
            if let Some(s) = innermost(&an.statements, n) {
                if s.kind == span.kind {
                    return Some(s.clone());
                }
            }
        }
        None
    }

    fn rename_map(&self, to_old: bool) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for g in &self.0.report.groups {
            if g.kind == SignatureKind::Rename && g.mechanical {
                let (a, b) = if to_old {
                    (&g.new, &g.old)
                } else {
                    (&g.old, &g.new)
                };
                out.insert(a.clone(), b.clone());
            }
        }
        out
    }

    fn span_text(&self, path: &str, side: Side, span: &StmtSpan) -> Option<Span> {
        let an = self.analysis(path, side)?;
        let start = (span.start as usize).saturating_sub(1).min(an.lines.len());
        let end = (span.end as usize).min(an.lines.len()).max(start);
        let mut lines = &an.lines[start..end];
        let truncated = lines.len() > FUNCTION_LINE_CAP;
        if truncated {
            lines = &lines[..FUNCTION_LINE_CAP];
        }
        Some(Span {
            path: side_path(&self.0.report, side, path),
            start: span.start,
            end: span.start + lines.len() as u32 - 1,
            text: lines.join("\n"),
            truncated,
        })
    }

    /// Where the enclosing def is used, at head (or at base when it only exists there).
    /// `None` when references can't be looked up for this location at all.
    pub async fn references_piece(
        &self,
        focus: &Focus,
        func: Option<&Function>,
    ) -> Option<Result<References, ReferencesError>> {
        let navigator = self.0.navigator.as_ref()?;
        let func = func?;
        let (side, span) = match (&func.new, &func.old) {
            (Some(new), _) => (Side::New, new),
            (None, Some(old)) => (Side::Old, old),
            (None, None) => return None,
        };
        let name = func.name().to_string();
        let (line, col) = self.name_position(&focus.path, side, span, &name)?;
        let src = &self.0.report.source;
        let sha = match side {
            Side::Old => Some(src.base_sha.as_str()),
            Side::New => src.head_sha.as_deref(),
        };
        let locs = match navigator.references(sha, &span.path, line, col).await {
            Ok(locs) => locs,
            Err(error) => return Some(Err(ReferencesError { name, side, error })),
        };
        let files = locs
            .iter()
            .map(|l| l.path.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let total = locs.len();
        let locations = locs
            .into_iter()
            .take(REFERENCES_CAP)
            .map(|l| RefLocation {
                text: l.text.trim().to_string(),
                ..l
            })
            .collect();
        Some(Ok(References {
            name,
            side,
            total,
            files,
            locations,
        }))
    }

    /// `None` when references can be looked up, else why not.
    pub fn references_available(&self, focus: &Focus) -> Option<String> {
        if self.0.navigator.is_none() {
            return Some("code navigation is off".to_string());
        }
        if analyzer_for(&focus.path).is_none() {
            return Some("no code navigation for this file type".to_string());
        }
        None
    }

    /// The first whole-word occurrence of `name` inside the span: `(line, 1-based column)`
    /// in code points.
    fn name_position(&self, path: &str, side: Side, span: &Span, name: &str) -> Option<(u32, u32)> {
        let an = self.analysis(path, side)?;
        let start = (span.start as usize).saturating_sub(1).min(an.lines.len());
        let end = (span.end as usize).min(an.lines.len()).max(start);
        for (offset, text) in an.lines[start..end].iter().enumerate() {
            if let Some(col) = find_word(text, name) {
                return Some((span.start + offset as u32, col as u32 + 1));
            }
        }
        None
    }

    /// The commits in the range that touched the focus line, plus the whole range.
    pub fn history_piece(&self, focus: &Focus) -> History {
        let src = &self.0.report.source;
        let Some(head) = src.head_sha.as_deref() else {
            return History::default();
        };
        let range = match self.0.git.list_commits(&src.base_sha, Some(head)) {
            Ok(commits) => commits,
            Err(e) => {
                tracing::warn!("listing commits {}..{head} failed: {e}", src.base_sha);
                vec![]
            }
        };
        let touching = if range.is_empty() {
            vec![]
        } else {
            self.line_commits(focus, &src.base_sha, head)
        };
        History { range, touching }
    }

    fn line_commits(&self, focus: &Focus, base: &str, head: &str) -> Vec<TouchingCommit> {
        match focus.side {
            Side::New => line_history_new(&self.0.git, base, head, &focus.path, focus.line),
            Side::Old => {
                let text = self
                    .hunk(focus)
                    .lines
                    .iter()
                    .find(|ln| ln.old_no == Some(focus.line))
                    .map(|ln| ln.text.trim().to_string())
                    .unwrap_or_default();
                if text.chars().count() < 6 {
                    return vec![];
                }
                let old_path = side_path(&self.0.report, Side::Old, &focus.path);
                line_history_old(&self.0.git, base, head, &text, &[&old_path, &focus.path])
            }
        }
    }

    pub fn pr_piece(&self) -> Option<PullRequest> {
        let pr = self.0.report.source.pr.as_ref()?;
        if !crate::settings::truthy(pr) {
            return None;
        }
        Some(PullRequest {
            number: pr["number"].as_i64(),
            title: pr["title"].as_str().unwrap_or("").to_string(),
            url: pr["url"].as_str().unwrap_or("").to_string(),
            body: pr["body"].as_str().unwrap_or("").trim().to_string(),
        })
    }

    /// The mechanical patterns, most repeated first.
    pub fn patterns_piece(&self) -> Vec<String> {
        let mut groups: Vec<&Group> = self
            .0
            .report
            .groups
            .iter()
            .filter(|g| g.mechanical)
            .collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.unit_ids.len()));
        groups
            .iter()
            .take(PATTERNS_CAP)
            .map(|g| {
                format!(
                    "{} {} ×{} in {} files",
                    g.kind.as_str(),
                    g.label,
                    g.unit_ids.len(),
                    g.files.len()
                )
            })
            .collect()
    }

    /// How many references the enclosing def has; `None` when there is no def, no navigator
    /// or the lookup failed.
    pub async fn refs_count(&self, focus: &Focus) -> Option<usize> {
        let func = self.function_piece(focus);
        match self.references_piece(focus, func.as_ref()).await? {
            Ok(refs) => Some(refs.total),
            Err(_) => None,
        }
    }

    // --- assembling --------------------------------------------------------------------------

    /// The requested pieces (plus the hunk and the patterns, always). Everything but the
    /// references runs on a blocking thread.
    pub async fn collect(&self, focus: &Focus, pieces: &BTreeSet<Piece>) -> Context {
        let this = self.clone();
        let focus = focus.clone();
        let wanted = pieces.clone();
        let (mut ctx, func) =
            tokio::task::spawn_blocking(move || this.collect_sync(&focus, &wanted))
                .await
                .expect("context collection does not panic");
        if pieces.contains(&Piece::References) {
            let refs = self.references_piece(&ctx.focus, func.as_ref()).await;
            ctx.references = Requested::from_option(true, refs);
        }
        ctx
    }

    /// The synchronous pieces; the function is returned separately because the references
    /// need it even when it wasn't asked for.
    pub fn collect_sync(
        &self,
        focus: &Focus,
        pieces: &BTreeSet<Piece>,
    ) -> (Context, Option<Function>) {
        let func = if pieces.contains(&Piece::Function) || pieces.contains(&Piece::References) {
            self.function_piece(focus)
        } else {
            None
        };
        let hunk = self.hunk(focus);
        let ctx = Context {
            focus: focus.clone(),
            hunk_at: HunkAt {
                path: hunk.path.clone(),
                old_start: hunk.old_start,
                new_start: hunk.new_start,
            },
            hunk: self.hunk_piece(focus),
            patterns: self.patterns_piece(),
            function: Requested::from_option(pieces.contains(&Piece::Function), func.clone()),
            references: Requested::No,
            history: if pieces.contains(&Piece::History) {
                Requested::Some(self.history_piece(focus))
            } else {
                Requested::No
            },
            pr: Requested::from_option(pieces.contains(&Piece::Pr), self.pr_piece()),
        };
        (ctx, func)
    }
}

impl ContextError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// The user message: every collected piece as a titled block, in a fixed order.
pub fn render(ctx: &Context) -> String {
    let focus = &ctx.focus;
    let func = ctx.function.as_option();
    let mut where_ = format!(
        "{}, {} side, line {}",
        focus.path,
        focus.side.as_str(),
        focus.line
    );
    if let Some(f) = func {
        where_.push_str(&format!(" (inside {} {})", f.kind_str(), f.qualname));
    }
    let mut blocks = vec![format!("## Location\n{where_}")];
    blocks.push(format!(
        "## Hunk ({} @@ old line {}, new line {})\n{}",
        ctx.hunk_at.path, ctx.hunk_at.old_start, ctx.hunk_at.new_start, ctx.hunk
    ));
    if let Some(f) = func {
        let lang = fence_lang(&focus.path);
        for (label, span) in [("before", &f.old), ("after", &f.new)] {
            let Some(span) = span else {
                blocks.push(format!(
                    "## Enclosing {} {} — {label}\n(does not exist on this side)",
                    f.kind_str(),
                    f.qualname
                ));
                continue;
            };
            let cut = if span.truncated {
                format!(" (cut after {FUNCTION_LINE_CAP} lines)")
            } else {
                String::new()
            };
            blocks.push(format!(
                "## Enclosing {} {} — {label} ({}:{}–{}{cut})\n```{lang}\n{}\n```",
                f.kind_str(),
                f.qualname,
                span.path,
                span.start,
                span.end,
                span.text
            ));
        }
    } else if ctx.function.is_requested() {
        blocks.push(
            "## Enclosing function\n(none: the line is not inside a def or class)".to_string(),
        );
    }
    match &ctx.references {
        Requested::Some(Err(e)) => {
            blocks.push(format!(
                "## References to {}\n(unavailable: {})",
                e.name, e.error
            ));
        }
        Requested::Some(Ok(refs)) => {
            let side = match refs.side {
                Side::New => "head",
                Side::Old => "base",
            };
            let mut lines: Vec<String> = refs
                .locations
                .iter()
                .map(|loc| {
                    format!(
                        "{}:{}  {}{}",
                        loc.path,
                        loc.line
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "None".into()),
                        loc.text,
                        if loc.is_definition {
                            "   [definition]"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            let more = refs.total.saturating_sub(refs.locations.len());
            if more > 0 {
                lines.push(format!("… and {more} more"));
            }
            blocks.push(format!(
                "## References to {} ({} in {} files, at {side})\n{}",
                refs.name,
                refs.total,
                refs.files,
                lines.join("\n")
            ));
        }
        Requested::Missing => blocks.push("## References\n(unavailable)".to_string()),
        Requested::No => {}
    }
    if let Some(hist) = ctx.history.as_option() {
        if hist.range.is_empty() {
            blocks.push("## Commits\n(none: this is the working tree)".to_string());
        } else {
            let mut lines: Vec<String> = Vec::new();
            for c in &hist.touching {
                let date: String = c.date.chars().take(10).collect();
                lines.push(format!("{} {} ({}, {date})", c.short, c.subject, c.author));
                if !c.body.is_empty() {
                    lines.push(format!("    {}", c.body.replace('\n', "\n    ")));
                }
            }
            let touching = if lines.is_empty() {
                "(no commit in the range touches this line directly)".to_string()
            } else {
                lines.join("\n")
            };
            let rng = hist
                .range
                .iter()
                .map(|c| format!("{} {}", c.short, c.subject))
                .collect::<Vec<_>>()
                .join("\n");
            blocks.push(format!("## Commits that touched this line\n{touching}"));
            blocks.push(format!("## All commits in the range (oldest first)\n{rng}"));
        }
    }
    match &ctx.pr {
        Requested::Some(pr) => {
            let body = if pr.body.is_empty() {
                "(no description)"
            } else {
                &pr.body
            };
            let number = pr
                .number
                .map(|n| n.to_string())
                .unwrap_or_else(|| "None".to_string());
            blocks.push(format!("## Pull request #{number}: {}\n{body}", pr.title));
        }
        Requested::Missing => {
            blocks.push("## Pull request\n(this comparison is not a pull request)".to_string());
        }
        Requested::No => {}
    }
    let pats = if ctx.patterns.is_empty() {
        "(none)".to_string()
    } else {
        ctx.patterns.join("\n")
    };
    blocks.push(format!("## Mechanical patterns in this diff\n{pats}"));
    blocks.join("\n\n")
}

// --- helpers ------------------------------------------------------------------------------------

fn line_marker(kind: LineType) -> char {
    match kind {
        LineType::Context => ' ',
        LineType::Removed => '-',
        LineType::Added => '+',
    }
}

/// The UI names files by their new path; a renamed file's old side lives at `old_path`.
pub fn side_path(report: &Report, side: Side, path: &str) -> String {
    if side == Side::Old {
        if let Some(old) = report
            .files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.old_path.clone())
        {
            return old;
        }
    }
    path.to_string()
}

fn fence_lang(path: &str) -> &'static str {
    match analyzer_for(path).map(|a| a.name()) {
        Some("python") => "python",
        Some("typescript") => "ts",
        Some("tsx") => "tsx",
        Some("javascript") => "js",
        _ => "",
    }
}

/// The def/class around `line`: a top-level statement's direct child def/class when one
/// contains the line, else the statement itself when it is a def/class.
fn innermost(statements: &[StmtSpan], line: u32) -> Option<&StmtSpan> {
    for s in statements {
        if s.start <= line && line <= s.end {
            for c in &s.children {
                if c.start <= line && line <= c.end && is_def(c.kind) {
                    return Some(c);
                }
            }
            return if is_def(s.kind) { Some(s) } else { None };
        }
    }
    None
}

fn is_def(kind: StmtKind) -> bool {
    matches!(kind, StmtKind::Def | StmtKind::Class)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// `(?<![\w$])name(?![\w$])`: the code-point column of the first whole-word match.
fn find_word(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if before_ok && after_ok {
            return Some(text[..start].chars().count());
        }
        from = start + name.chars().next().map_or(1, char::len_utf8);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: u32, end: u32, kind: StmtKind, name: &str, children: Vec<StmtSpan>) -> StmtSpan {
        StmtSpan {
            start,
            end,
            kind,
            qualname: name.into(),
            node: None,
            children,
        }
    }

    #[test]
    fn innermost_prefers_a_method_then_the_class() {
        let stmts = vec![
            span(1, 1, StmtKind::Stmt, "", vec![]),
            span(
                3,
                10,
                StmtKind::Class,
                "A",
                vec![
                    span(4, 5, StmtKind::Def, "A.f", vec![]),
                    span(6, 7, StmtKind::Stmt, "", vec![]),
                ],
            ),
        ];
        assert!(innermost(&stmts, 1).is_none());
        assert_eq!(innermost(&stmts, 4).unwrap().qualname, "A.f");
        assert_eq!(innermost(&stmts, 6).unwrap().qualname, "A");
        assert!(innermost(&stmts, 12).is_none());
    }

    #[test]
    fn find_word_respects_identifier_boundaries() {
        assert_eq!(find_word("def get_user(x):", "get_user"), Some(4));
        assert_eq!(find_word("fetch_user_id = 1", "user"), None);
        assert_eq!(find_word("$user + user", "user"), Some(8));
        assert_eq!(find_word("é user", "user"), Some(2));
        assert_eq!(find_word("", "x"), None);
    }

    #[test]
    fn side_parsing() {
        assert_eq!(Side::parse("o"), Some(Side::Old));
        assert_eq!(Side::parse("new"), Some(Side::New));
        assert_eq!(Side::parse("sideways"), None);
        assert_eq!(serde_json::to_string(&Side::New).unwrap(), "\"new\"");
    }
}
