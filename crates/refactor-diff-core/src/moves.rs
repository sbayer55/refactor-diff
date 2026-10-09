//! Detect code moved between files (or within one) and the import churn a move causes.
//!
//! A move shows up in the line diff as a deletion in one place and an insertion in another.
//! After every file has been diffed, deletion-only and insertion-only units are compared by
//! their token streams (layout and comments ignored): exact matches pair first, then near
//! matches. A paired unit gets a `move` signature, which is always mechanical, so an exact
//! move disappears from review; a move with edits inside leaves only those edits, classified
//! like any other change.
//!
//! A move is *certain* only when nothing about the pairing is in doubt (see `certain`). Only
//! certain moves are tagged for the "Moved functions" filter; the others still show as moves.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexMap;

use crate::classify::{classify, make_unit, merge_ranges, tokens_in_range};
use crate::lang::typescript::{JS_SUFFIXES, TS_SUFFIXES, TSX_SUFFIXES};
use crate::lang::{FileAnalysis, LanguageAnalyzer, StmtKind, StmtSpan, TokenKind, analyzer_for};
use crate::model::{
    FileStatus, FileSummary, Hunk, LineType, Signature, SignatureKind, Unit, UnitTag,
};
use crate::seqmatch::{Opcode, SequenceMatcher, Tag};
use crate::short_hash;

/// Non-blank lines a block needs before it can count as moved.
const MIN_LINES: usize = 3;
const MIN_TOKENS: usize = 12;
/// Token-stream similarity for a "moved with edits" pair.
const FUZZY_MIN: f64 = 0.75;
/// Comparisons before giving up on near matches (large diffs).
const MAX_FUZZY_PAIRS: usize = 5000;

pub type Analyses = BTreeMap<String, (FileAnalysis, FileAnalysis)>;

fn js_like() -> impl Iterator<Item = &'static str> {
    TS_SUFFIXES
        .iter()
        .chain(TSX_SUFFIXES)
        .chain(JS_SUFFIXES)
        .copied()
}

fn is_js_like(path: &str) -> bool {
    js_like().any(|s| path.ends_with(s))
}

#[derive(Clone, Debug)]
pub struct Move {
    pub id: String,
    pub from_unit: String,
    pub to_unit: String,
    pub old_path: String,
    /// 1-based inclusive.
    pub old_range: (u32, u32),
    pub new_path: String,
    pub new_range: (u32, u32),
    pub ratio: f64,
    /// Defs/classes in the block.
    pub names: Vec<String>,
    /// Whole definitions, moved verbatim, with no other candidate.
    pub certain: bool,
    /// Import edits explained by it.
    pub linked_units: Vec<String>,
}

impl Move {
    pub fn signature(&self) -> Signature {
        let (s, e) = self.old_range;
        let (s2, e2) = self.new_range;
        let detail = if self.names.is_empty() {
            "block".to_string()
        } else {
            self.names.join(", ")
        };
        Signature::new(
            SignatureKind::Move,
            format!(
                "move\0{}\0{s}-{e}\0{}\0{s2}-{e2}",
                self.old_path, self.new_path
            ),
            &self.old_path,
            &self.new_path,
        )
        .with_detail(detail)
    }
}

type Stream = Rc<[String]>;

#[derive(Clone, Debug)]
struct Block {
    unit_id: String,
    path: String,
    /// 1-based inclusive line range within the unit.
    start: u32,
    end: u32,
    stream: Stream,
    /// The entire unit, as opposed to one statement out of it.
    whole: bool,
}

impl Block {
    fn size(&self) -> usize {
        self.stream.len()
    }

    fn overlaps(&self, other: &Block) -> bool {
        self.unit_id == other.unit_id && self.start <= other.end && other.start <= self.end
    }

    fn key(&self) -> (String, u32, u32) {
        (self.unit_id.clone(), self.start, self.end)
    }
}

/// Pair deleted blocks with inserted ones, splitting units where only part of one moved, and
/// tag both halves with a move signature. Mutates `units` and `hunks`.
pub fn detect_moves(
    units: &mut IndexMap<String, Unit>,
    hunks: &mut IndexMap<String, Hunk>,
    analyses: &Analyses,
) -> Vec<Move> {
    let mut dels: Vec<Block> = Vec::new();
    let mut ins: Vec<Block> = Vec::new();
    for u in units.values() {
        let Some((old_an, new_an)) = analyses.get(&u.path) else {
            continue;
        };
        if !u.old.is_empty() && u.new.is_empty() && old_an.parsed {
            dels.extend(blocks(u, old_an, true));
        } else if !u.new.is_empty() && u.old.is_empty() && new_an.parsed {
            ins.extend(blocks(u, new_an, false));
        }
    }
    if dels.is_empty() || ins.is_empty() {
        return vec![];
    }

    let pairs = pair(&dels, &ins);
    if pairs.is_empty() {
        return vec![];
    }

    // Units with a partial match are cut at the matched statements' boundaries first.
    let mut cuts: IndexMap<String, Vec<(u32, u32)>> = IndexMap::new();
    for (d, i, _) in &pairs {
        for b in [d, i] {
            if !b.whole {
                cuts.entry(b.unit_id.clone())
                    .or_default()
                    .push((b.start, b.end));
            }
        }
    }
    let mut pieces: HashMap<(String, u32, u32), String> = HashMap::new();
    for (uid, mut ranges) in cuts {
        ranges.sort();
        pieces.extend(split_unit(units, hunks, &uid, &ranges, analyses));
    }
    let resolve = |b: &Block| -> String {
        if b.whole {
            b.unit_id.clone()
        } else {
            pieces[&b.key()].clone()
        }
    };

    // The same code deleted (or inserted) more than once: which copy went where is a guess.
    let (del_counts, ins_counts) = (count_streams(&dels), count_streams(&ins));
    let mut moves = Vec::new();
    for (d, i, ratio) in &pairs {
        let Some(analyzer) = analyzer_for(&i.path) else {
            continue;
        };
        let unique = del_counts[&*d.stream] == 1 && ins_counts[&*i.stream] == 1;
        let (d_id, i_id) = (resolve(d), resolve(i));
        moves.push(apply_move(
            units,
            &d_id,
            &i_id,
            analyzer,
            &analyses[&d.path].0,
            &analyses[&i.path].1,
            *ratio,
            unique,
        ));
    }
    moves
}

fn count_streams(blocks: &[Block]) -> HashMap<&[String], usize> {
    let mut c = HashMap::new();
    for b in blocks {
        *c.entry(&*b.stream).or_default() += 1;
    }
    c
}

fn blocks(unit: &Unit, analysis: &FileAnalysis, old_side: bool) -> Vec<Block> {
    let lines = if old_side { &unit.old } else { &unit.new };
    let start = if old_side {
        unit.old_start
    } else {
        unit.new_start
    };
    let end = start + lines.len() as u32 - 1;
    let mut out = Vec::new();
    if let Some(whole) = block(unit, analysis, start, end, true) {
        out.push(whole);
    }
    // Statements of their own inside the block, so one function can be matched out of a
    // deleted file (or a class body).
    let mut spans: Vec<&StmtSpan> = analysis
        .statements
        .iter()
        .filter(|s| start <= s.start && s.end <= end)
        .collect();
    let children: Vec<&StmtSpan> = spans
        .iter()
        .flat_map(|s| s.children.iter().filter(|c| c.kind != StmtKind::Stmt))
        .collect();
    spans.extend(children);
    if spans.len() >= 2 {
        for s in spans {
            if let Some(b) = block(unit, analysis, s.start, s.end, false) {
                out.push(b);
            }
        }
    }
    out
}

fn block(unit: &Unit, analysis: &FileAnalysis, start: u32, end: u32, whole: bool) -> Option<Block> {
    let stream = stream(analysis, (start, end));
    let non_blank = (start..=end)
        .filter(|n| {
            analysis
                .lines
                .get(*n as usize - 1)
                .is_some_and(|l| !l.trim().is_empty())
        })
        .count();
    if stream.len() < MIN_TOKENS || non_blank < MIN_LINES {
        return None;
    }
    Some(Block {
        unit_id: unit.id.clone(),
        path: unit.path.clone(),
        start,
        end,
        stream: stream.into(),
        whole,
    })
}

/// Token values of a line range, without layout or comments.
fn stream(analysis: &FileAnalysis, rng: (u32, u32)) -> Vec<String> {
    tokens_in_range(analysis, Some(rng))
        .tokens()
        .iter()
        .filter(|t| !matches!(t.kind, TokenKind::Structural | TokenKind::Comment))
        .map(|t| t.value.clone())
        .collect()
}

fn pair<'a>(dels: &'a [Block], ins: &'a [Block]) -> Vec<(&'a Block, &'a Block, f64)> {
    let mut used: Vec<&Block> = Vec::new();
    let mut pairs: Vec<(&Block, &Block, f64)> = Vec::new();

    let mut by_stream: HashMap<&[String], Vec<&Block>> = HashMap::new();
    for b in ins {
        by_stream.entry(&*b.stream).or_default().push(b);
    }
    // Largest first, so a whole unit wins over the statements inside it.
    let mut order: Vec<&Block> = dels.iter().collect();
    order.sort_by(|a, b| {
        (std::cmp::Reverse(a.size()), &a.path, a.start).cmp(&(
            std::cmp::Reverse(b.size()),
            &b.path,
            b.start,
        ))
    });
    for d in &order {
        if used.iter().any(|u| d.overlaps(u)) {
            continue;
        }
        let mut candidates: Vec<&Block> = by_stream
            .get(&*d.stream)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|i| !used.iter().any(|u| i.overlaps(u)))
                    .collect()
            })
            .unwrap_or_default();
        if !candidates.is_empty() {
            candidates.sort_by(|a, b| {
                (a.path != d.path, &a.path, a.start).cmp(&(b.path != d.path, &b.path, b.start))
            });
            used.push(d);
            used.push(candidates[0]);
            pairs.push((d, candidates[0], 1.0));
        }
    }

    let mut scored: Vec<(f64, usize, &Block, &Block)> = Vec::new();
    let mut comparisons = 0usize;
    for d in &order {
        if used.iter().any(|u| d.overlaps(u)) {
            continue;
        }
        for i in ins {
            let ratio_ok = {
                let r = i.size() as f64 / d.size() as f64;
                (0.6..=1.6).contains(&r)
            };
            if used.iter().any(|u| i.overlaps(u)) || !ratio_ok {
                continue;
            }
            comparisons += 1;
            if comparisons > MAX_FUZZY_PAIRS {
                break;
            }
            let sm = SequenceMatcher::new(&d.stream, &i.stream);
            if sm.quick_ratio() < FUZZY_MIN {
                continue;
            }
            let ratio = sm.ratio();
            if ratio >= FUZZY_MIN {
                scored.push((ratio, d.size(), d, i));
            }
        }
    }
    scored.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.path.cmp(&b.2.path))
            .then_with(|| a.2.start.cmp(&b.2.start))
    });
    for (ratio, _, d, i) in scored {
        if !used.iter().any(|u| d.overlaps(u)) && !used.iter().any(|u| i.overlaps(u)) {
            used.push(d);
            used.push(i);
            pairs.push((d, i, ratio));
        }
    }
    pairs
}

/// Replace a one-sided unit by consecutive pieces cut at `ranges` (the matched blocks).
/// Returns the piece id for each range, keyed like the blocks.
fn split_unit(
    units: &mut IndexMap<String, Unit>,
    hunks: &mut IndexMap<String, Hunk>,
    unit_id: &str,
    ranges: &[(u32, u32)],
    analyses: &Analyses,
) -> HashMap<(String, u32, u32), String> {
    let unit = units[unit_id].clone();
    let analyzer = analyzer_for(&unit.path).expect("units only exist for analyzed files");
    let (old_an, new_an) = &analyses[&unit.path];
    let deleting = !unit.old.is_empty();
    let start = if deleting {
        unit.old_start
    } else {
        unit.new_start
    };
    let end = start
        + (if deleting {
            unit.old.len()
        } else {
            unit.new.len()
        }) as u32
        - 1;
    let mut bounds: Vec<(u32, u32, bool)> = Vec::new();
    let mut cursor = start;
    for &(s, e) in ranges {
        if s > cursor {
            bounds.push((cursor, s - 1, false));
        }
        bounds.push((s, e, true));
        cursor = e + 1;
    }
    if cursor <= end {
        bounds.push((cursor, end, false));
    }

    let mut out = HashMap::new();
    let mut new_units = Vec::new();
    for (s, e, matched) in bounds {
        let (s0, e0) = (s as usize, e as usize);
        let op = if deleting {
            Opcode::new(
                Tag::Delete,
                s0 - 1,
                e0,
                unit.new_start as usize - 1,
                unit.new_start as usize - 1,
            )
        } else {
            Opcode::new(
                Tag::Insert,
                unit.old_start as usize - 1,
                unit.old_start as usize - 1,
                s0 - 1,
                e0,
            )
        };
        let cls = classify(
            analyzer,
            old_an,
            new_an,
            op.old_range(),
            op.new_range(),
            0,
            0,
        );
        let piece = make_unit(
            &unit.path,
            &unit.hunk_id,
            &op,
            cls,
            &old_an.lines,
            &new_an.lines,
        );
        if matched {
            out.insert((unit.id.clone(), s, e), piece.id.clone());
        }
        new_units.push(piece);
    }

    units.shift_remove(unit_id);
    let piece_ids: Vec<String> = new_units.iter().map(|p| p.id.clone()).collect();
    for p in new_units {
        units.insert(p.id.clone(), p);
    }
    let hunk = &mut hunks[&unit.hunk_id];
    let k = hunk
        .unit_ids
        .iter()
        .position(|id| id == unit_id)
        .expect("unit listed in its hunk");
    hunk.unit_ids.splice(k..k + 1, piece_ids);
    out
}

#[allow(clippy::too_many_arguments)]
fn apply_move(
    units: &mut IndexMap<String, Unit>,
    d_id: &str,
    i_id: &str,
    analyzer: &dyn LanguageAnalyzer,
    from_old_an: &FileAnalysis,
    to_new_an: &FileAnalysis,
    ratio: f64,
    unique: bool,
) -> Move {
    let (d_path, old_range) = {
        let d = &units[d_id];
        (
            d.path.clone(),
            (d.old_start, d.old_start + d.old.len() as u32 - 1),
        )
    };
    let (i_path, new_range) = {
        let i = &units[i_id];
        (
            i.path.clone(),
            (i.new_start, i.new_start + i.new.len() as u32 - 1),
        )
    };
    // classify() only reads tokens inside the ranges, so it diffs across files just as well.
    let cls = classify(
        analyzer,
        from_old_an,
        to_new_an,
        Some(old_range),
        Some(new_range),
        0,
        0,
    );
    let mut names = names_in_range(to_new_an, new_range);
    if names.is_empty() {
        names = names_in_range(from_old_an, old_range);
    }
    let certain = is_certain(
        &cls.signatures,
        from_old_an,
        old_range,
        to_new_an,
        new_range,
        ratio,
        unique,
    );
    let mv = Move {
        id: short_hash!("move", d_id, i_id),
        from_unit: d_id.to_string(),
        to_unit: i_id.to_string(),
        old_path: d_path,
        old_range,
        new_path: i_path,
        new_range,
        ratio,
        names,
        certain,
        linked_units: vec![],
    };
    let sig = mv.signature();
    // The edits made inside the moved block live on the new side only, so that a one-off edit
    // isn't counted twice and mistaken for a repeated pattern.
    {
        let d = &mut units[d_id];
        for (k, ln) in d.old.iter_mut().enumerate() {
            ln.hl = merge_ranges(
                cls.old_hl
                    .get(&(old_range.0 + k as u32))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        d.signatures = vec![sig.clone()];
        d.partner = Some(i_id.to_string());
    }
    {
        let i = &mut units[i_id];
        for (k, ln) in i.new.iter_mut().enumerate() {
            ln.hl = merge_ranges(
                cls.new_hl
                    .get(&(new_range.0 + k as u32))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let mut sigs = vec![sig];
        sigs.extend(
            cls.signatures
                .iter()
                .filter(|s| s.kind != SignatureKind::Formatting)
                .cloned(),
        );
        i.signatures = sigs;
        i.partner = Some(d_id.to_string());
    }
    mv
}

/// Whether the pair is a move beyond doubt: an exact copy (comments included), of whole
/// functions or classes that keep their qualified names, and the only candidate either way.
/// A method that went to another class, a function that became a method, loose statements or
/// code that appears twice are not certain.
fn is_certain(
    signatures: &[Signature],
    old_an: &FileAnalysis,
    old_range: (u32, u32),
    new_an: &FileAnalysis,
    new_range: (u32, u32),
    ratio: f64,
    unique: bool,
) -> bool {
    if ratio != 1.0 || !unique {
        return false;
    }
    if signatures
        .iter()
        .any(|s| s.kind != SignatureKind::Formatting)
    {
        return false; // e.g. a comment edited on the way
    }
    let old_defs = defs_only(old_an, old_range);
    old_defs.is_some() && old_defs == defs_only(new_an, new_range)
}

/// Qualified names of the defs/classes in the range, or `None` when the range holds code
/// outside of them (or none at all).
fn defs_only(analysis: &FileAnalysis, rng: (u32, u32)) -> Option<Vec<String>> {
    let spans: Vec<&StmtSpan> = analysis
        .flat_statements()
        .into_iter()
        .filter(|s| {
            s.kind != StmtKind::Stmt && !s.qualname.is_empty() && rng.0 <= s.start && s.end <= rng.1
        })
        .collect();
    if spans.is_empty() {
        return None;
    }
    for t in tokens_in_range(analysis, Some(rng)).tokens() {
        if matches!(t.kind, TokenKind::Structural | TokenKind::Comment) {
            continue;
        }
        if !spans
            .iter()
            .any(|s| s.start <= t.start.line && t.start.line <= s.end)
        {
            return None;
        }
    }
    Some(spans.iter().map(|s| s.qualname.clone()).collect())
}

/// Tag both halves of each certain move, and the import edits it explains, as moved.
pub fn tag_moves(units: &mut IndexMap<String, Unit>, moves: &[Move]) {
    let certain: HashSet<String> = moves
        .iter()
        .filter(|m| m.certain)
        .map(|m| m.signature().key)
        .collect();
    for m in moves.iter().filter(|m| m.certain) {
        for uid in std::iter::once(&m.from_unit)
            .chain(std::iter::once(&m.to_unit))
            .chain(&m.linked_units)
        {
            let u = &mut units[uid];
            if !u.signatures.is_empty() && u.signatures.iter().all(|s| certain.contains(&s.key)) {
                u.tags.push(UnitTag::Moved);
            }
        }
    }
}

fn names_in_range(analysis: &FileAnalysis, rng: (u32, u32)) -> Vec<String> {
    analysis
        .statements
        .iter()
        .filter(|s| !s.qualname.is_empty() && rng.0 <= s.start && s.end <= rng.1)
        .map(|s| s.qualname.clone())
        .collect()
}

type LineOwner<'a> = HashMap<(&'a str, u32), (&'a str, &'a Vec<[u32; 2]>)>;

/// Point hunk lines at the units (and highlights) they belong to after units changed.
pub fn resync_hunks(hunks: &mut IndexMap<String, Hunk>, units: &IndexMap<String, Unit>) {
    let mut by_old: LineOwner<'_> = HashMap::new();
    let mut by_new: LineOwner<'_> = HashMap::new();
    for u in units.values() {
        for (k, ln) in u.old.iter().enumerate() {
            by_old.insert((&u.path, u.old_start + k as u32), (&u.id, &ln.hl));
        }
        for (k, ln) in u.new.iter().enumerate() {
            by_new.insert((&u.path, u.new_start + k as u32), (&u.id, &ln.hl));
        }
    }
    for h in hunks.values_mut() {
        for ln in &mut h.lines {
            let hit = match ln.kind {
                LineType::Removed => ln.old_no.and_then(|n| by_old.get(&(h.path.as_str(), n))),
                LineType::Added => ln.new_no.and_then(|n| by_new.get(&(h.path.as_str(), n))),
                LineType::Context => None,
            };
            if let Some((id, hl)) = hit {
                ln.unit = Some(id.to_string());
                ln.hl = (*hl).clone();
            }
        }
    }
}

// --- import churn caused by a move ---------------------------------------------------------

/// An import edit that only follows a move (`from a import f` -> `from b import f` after `f`
/// moved from a.py to b.py, an import the moved block needs at its destination, or one it no
/// longer needs at its origin) is explained by that move: its `import` signature is replaced
/// by the move's.
pub fn link_imports(units: &mut IndexMap<String, Unit>, moves: &mut [Move], analyses: &Analyses) {
    if moves.is_empty() {
        return;
    }
    for u in units.values_mut() {
        for k in 0..u.signatures.len() {
            if u.signatures[k].kind != SignatureKind::Import {
                continue;
            }
            if let Some(mi) = explaining_move(u, &u.signatures[k], moves, analyses) {
                u.signatures[k] = moves[mi].signature();
                moves[mi].linked_units.push(u.id.clone());
                break;
            }
        }
    }
}

fn explaining_move(
    u: &Unit,
    sig: &Signature,
    moves: &[Move],
    analyses: &Analyses,
) -> Option<usize> {
    let mut parts = sig.key.splitn(3, '\0');
    let _ = parts.next();
    let old_mod = parts.next().unwrap_or("");
    let new_mod = parts.next().unwrap_or("");
    let alias = sig.detail.as_str();
    for (mi, m) in moves.iter().enumerate() {
        if !old_mod.is_empty() && !new_mod.is_empty() {
            let last_names: HashSet<&str> = m
                .names
                .iter()
                .map(|n| n.rsplit('.').next().unwrap_or(n))
                .collect();
            if last_names.contains(alias)
                && module_matches(old_mod, &m.old_path, &u.path)
                && module_matches(new_mod, &m.new_path, &u.path)
            {
                return Some(mi);
            }
        } else if !new_mod.is_empty() && u.path == m.new_path {
            if names_in(&analyses[&m.new_path].1, Some(m.new_range)).contains(alias) {
                return Some(mi);
            }
        } else if !old_mod.is_empty()
            && u.path == m.old_path
            && !names_in(&analyses[&m.old_path].1, None).contains(alias)
        {
            return Some(mi);
        }
    }
    None
}

fn names_in(analysis: &FileAnalysis, rng: Option<(u32, u32)>) -> HashSet<String> {
    let toks = match rng {
        None => &analysis.tokens[..],
        Some(r) => tokens_in_range(analysis, Some(r)).tokens(),
    };
    toks.iter()
        .filter(|t| t.kind == TokenKind::Name)
        .map(|t| t.value.clone())
        .collect()
}

/// What a relative import points at.
#[derive(Debug, PartialEq, Eq)]
enum RelativeTarget {
    /// Python: path parts.
    Parts(Vec<String>),
    /// TS/JS: an extensionless path.
    Path(String),
}

/// Whether `module` (a dotted Python module, possibly relative to `importer`'s package, or a
/// relative TS/JS specifier) names `path`.
pub fn module_matches(module: &str, path: &str, importer: &str) -> bool {
    if is_js_like(importer) {
        let Some(RelativeTarget::Path(target)) = relative_target(module, importer) else {
            return false;
        };
        return is_js_like(path) && js_stems(path).contains(&target);
    }
    if !(path.ends_with(".py") || path.ends_with(".pyi")) {
        return false;
    }
    let without_ext = path.rsplit_once('.').map_or(path, |(stem, _)| stem);
    let mut parts: Vec<String> = without_ext.split('/').map(String::from).collect();
    if parts.last().is_some_and(|p| p == "__init__") {
        parts.pop();
    }
    if module.starts_with('.') {
        return relative_target(module, importer) == Some(RelativeTarget::Parts(parts));
    }
    let mod_parts: Vec<&str> = module.split('.').collect();
    if mod_parts.len() > parts.len() {
        return false;
    }
    parts[parts.len() - mod_parts.len()..]
        .iter()
        .zip(&mod_parts)
        .all(|(a, b)| a == b)
}

/// What a relative import in `importer` points at: path parts for Python, an extensionless
/// path for TS/JS. `None` for an absolute import or a package specifier.
fn relative_target(module: &str, importer: &str) -> Option<RelativeTarget> {
    if is_js_like(importer) {
        if !(module == "."
            || module == ".."
            || module.starts_with("./")
            || module.starts_with("../"))
        {
            return None;
        }
        let dir = importer.rsplit_once('/').map_or("", |(d, _)| d);
        let joined = if dir.is_empty() {
            module.to_string()
        } else {
            format!("{dir}/{module}")
        };
        let mut target = normpath(&joined);
        for suffix in js_like() {
            // "./m.js" names m.ts under ESM conventions.
            if target.ends_with(suffix) {
                target.truncate(target.len() - suffix.len());
                break;
            }
        }
        return Some(RelativeTarget::Path(target));
    }
    if !module.starts_with('.') {
        return None;
    }
    let level = module.len() - module.trim_start_matches('.').len();
    let mut pkg: Vec<String> = importer.split('/').map(String::from).collect();
    pkg.pop();
    let base: Vec<String> = if level > 1 {
        // Python slicing: pkg[:len(pkg) - (level - 1)], which wraps when negative.
        let n = pkg.len() as isize - (level as isize - 1);
        if n >= 0 {
            pkg[..n as usize].to_vec()
        } else {
            pkg[..(pkg.len() as isize + n).max(0) as usize].to_vec()
        }
    } else {
        pkg
    };
    let mut parts = base;
    parts.extend(
        module
            .trim_start_matches('.')
            .split('.')
            .filter(|p| !p.is_empty())
            .map(String::from),
    );
    Some(RelativeTarget::Parts(parts))
}

/// `posixpath.normpath` for relative paths.
fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if stack.last().is_some_and(|c| *c != "..") {
                    stack.pop();
                } else if !absolute {
                    stack.push("..");
                }
            }
            c => stack.push(c),
        }
    }
    let joined = stack.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

fn js_stems(path: &str) -> HashSet<String> {
    let stem = js_like()
        .find(|s| path.ends_with(s))
        .map(|s| path[..path.len() - s.len()].to_string())
        .unwrap_or_else(|| path.to_string());
    let mut stems = HashSet::new();
    if stem == "index" || stem.ends_with("/index") {
        let dir = stem.rsplit_once('/').map_or("", |(d, _)| d);
        stems.insert(if dir.is_empty() {
            ".".to_string()
        } else {
            dir.to_string()
        });
    }
    stems.insert(stem);
    stems
}

// --- import churn caused by a renamed file -------------------------------------------------

/// Tag the import edits that only follow a renamed (moved) file: every edit on the unit
/// points the same name at the file's new module instead of its old one. Run after the units
/// are tagged as import-only.
pub fn link_file_renames(units: &mut IndexMap<String, Unit>, files: &[FileSummary]) {
    let renamed: Vec<&FileSummary> = files
        .iter()
        .filter(|f| f.status == FileStatus::Renamed && f.old_path.is_some())
        .collect();
    if renamed.is_empty() {
        return;
    }
    let old_path_of: HashMap<&str, &str> = renamed
        .iter()
        .map(|f| (f.path.as_str(), f.old_path.as_deref().unwrap()))
        .collect();
    for u in units.values_mut() {
        if !u.has_tag(UnitTag::Imports) || u.signatures.is_empty() {
            continue;
        }
        let importer_old = old_path_of.get(u.path.as_str()).copied().unwrap_or(&u.path);
        if u.signatures
            .iter()
            .all(|s| follows_rename(s, importer_old, &u.path, &renamed))
        {
            u.tags.push(UnitTag::FileMove);
        }
    }
}

fn follows_rename(
    sig: &Signature,
    importer_old: &str,
    importer: &str,
    renamed: &[&FileSummary],
) -> bool {
    if sig.kind != SignatureKind::Import {
        return false;
    }
    let mut parts = sig.key.splitn(3, '\0');
    let _ = parts.next();
    let old_mod = parts.next().unwrap_or("");
    let new_mod = parts.next().unwrap_or("");
    if old_mod.is_empty() || new_mod.is_empty() {
        return false; // a name was added or removed: more than a path update
    }
    if renamed.iter().any(|f| {
        module_matches(old_mod, f.old_path.as_deref().unwrap(), importer_old)
            && module_matches(new_mod, &f.path, importer)
    }) {
        return true;
    }
    // The importer moved, and its relative import was adjusted to keep the same target.
    if importer_old == importer {
        return false;
    }
    let old_target = relative_target(old_mod, importer_old);
    old_target.is_some() && old_target == relative_target(new_mod, importer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normpath_like_posixpath() {
        assert_eq!(normpath("a/./b/../c"), "a/c");
        assert_eq!(normpath("./m"), "m");
        assert_eq!(normpath("../x"), "../x");
        assert_eq!(normpath("a/.."), ".");
        assert_eq!(normpath("src/./util.js"), "src/util.js");
    }

    #[test]
    fn python_module_matching() {
        assert!(module_matches("pkg.a", "pkg/a.py", "pkg/b.py"));
        assert!(module_matches("a", "pkg/a.py", "pkg/b.py"));
        assert!(module_matches(".a", "pkg/a.py", "pkg/b.py"));
        assert!(module_matches("..a", "a.py", "pkg/b.py"));
        assert!(module_matches("pkg", "pkg/__init__.py", "x.py"));
        assert!(!module_matches("other", "pkg/a.py", "pkg/b.py"));
        assert!(!module_matches("a", "pkg/a.ts", "pkg/b.py"));
    }

    #[test]
    fn js_module_matching() {
        assert!(module_matches("./util", "src/util.ts", "src/app.ts"));
        assert!(module_matches("./util.js", "src/util.ts", "src/app.ts"));
        assert!(module_matches("../lib", "lib/index.ts", "src/app.ts"));
        assert!(module_matches("./lib", "src/lib/index.js", "src/app.js"));
        assert!(!module_matches(
            "lodash",
            "node_modules/lodash/index.js",
            "src/app.js"
        ));
        assert!(!module_matches("./util", "src/other.ts", "src/app.ts"));
    }
}
