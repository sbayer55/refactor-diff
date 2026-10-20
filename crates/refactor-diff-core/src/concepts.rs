//! The concept lens: identifier stems a change set adds or removes everywhere (`program`), the
//! units that removing the concept fully explains, and the mechanical groups that are the same
//! concept edit once literals are normalized. An overlay: never touches `Unit::explained`.
//!
//! Detection splits the identifiers on changed lines into words and scores each word by the
//! share of changed identifiers that contain it. For each concept, every unit is stripped of
//! the concept on both sides: the tokens that mention it, together with the construct around
//! them (object member, list element, JSX attribute, whole statement) and any guard that is
//! left with nothing to test. When the stripped sides are equal, removing (or adding) the
//! concept is the whole unit.
//!
//! Owned by workstream A (see docs/concept-lens-plan.md).

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::categories::Category;
use crate::engine::AnalyzeOptions;
use crate::lang::{FileAnalysis, LanguageAnalyzer, Token, TokenKind, analyzer_for};
use crate::model::{
    Concept, ConceptFile, ConceptPattern, ConceptRole, FileStatus, FileSummary, Group,
    SignatureKind, Unit,
};
use crate::moves::Analyses;
use crate::seqmatch::{SequenceMatcher, Tag};

/// Auto-detected concepts kept, strongest first.
const MAX_AUTO: usize = 3;
/// Share of changed identifiers a stem needs to count as a concept.
const MIN_SCORE: f64 = 0.15;
/// Tokens scanned around a unit when deciding whether it edits a hook's deps array.
const MAX_SCAN: usize = 4000;
/// Lines a pure deletion or insertion may slide to find balanced brackets.
const MAX_SLIDE: u32 = 12;

/// Words too common to name a concept.
#[rustfmt::skip]
const STOP_WORDS: &[&str] = &[
    "id", "ids", "get", "set", "use", "data", "name", "names", "type", "value", "props", "prop",
    "test", "mock", "current", "list", "new", "old", "is", "has", "to", "of", "on", "by", "the",
    "and", "for", "with", "from", "key", "item", "string", "number", "boolean", "null", "true",
    "false", "this", "return", "const", "let", "var", "function", "async", "await", "default",
    "export", "import", "error", "state", "index", "map", "filter", "length", "result",
    "params", "param", "options", "args", "label", "text", "react", "div", "span", "undefined",
    "void", "expect", "describe", "should", "render", "screen", "tsx", "jsx", "className",
    "class", "children", "event", "handle", "handler", "callback", "response", "request",
    "query", "res", "req", "err", "fn", "obj", "self", "cls", "none",
];

/// Names a dropped guard or a gutted statement may keep and still count as concept-only:
/// notifications, translations, navigation and control flow.
#[rustfmt::skip]
const NEUTRAL: &[&str] = &[
    "toaster", "toast", "t", "console", "log", "error", "warn", "info", "debug", "navigate",
    "router", "push", "replace", "Error", "useEffect", "useMemo", "useCallback", "useQuery",
    "useState", "enabled", "skip", "id", "length", "message", "success", "notify", "showToast",
    "pass", "raise", "None", "True", "False", "not", "and", "or",
];

/// Hooks whose last argument is a dependency array.
const DEPS_HOOKS: &[&str] = &[
    "useEffect",
    "useMemo",
    "useCallback",
    "useLayoutEffect",
    "useImperativeHandle",
];

/// Path segments of mock and fixture data.
const FIXTURE_DIRS: &[&str] = &["mocks", "__mocks__", "fixtures", "__fixtures__", "mock"];

/// Detect concepts and compute their overlay. `opts.concept` adds (or pins) one by name.
pub fn detect(
    units: &IndexMap<String, Unit>,
    groups: &[Group],
    files: &[FileSummary],
    analyses: &Analyses,
    opts: &AnalyzeOptions,
) -> Vec<Concept> {
    let whole: HashSet<&str> = files
        .iter()
        .filter(|f| matches!(f.status, FileStatus::Added | FileStatus::Deleted))
        .map(|f| f.path.as_str())
        .collect();
    let slices: Vec<UnitTokens<'_>> = units
        .values()
        .filter(|u| u.partner.is_none())
        .filter_map(|u| UnitTokens::of(u, analyses, whole.contains(u.path.as_str())))
        .collect();

    let mut stems: Vec<(String, f64, bool)> = Vec::new();
    let scores = word_scores(&slices);
    let mut ranked: Vec<(&String, f64)> = scores.iter().map(|(w, s)| (w, *s)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    for (word, score) in ranked.into_iter().take(MAX_AUTO) {
        if score >= MIN_SCORE {
            stems.push((word.clone(), score, true));
        }
    }
    if let Some(asked) = opts.concept.as_deref().and_then(normalize_stem) {
        match stems.iter_mut().find(|s| s.0 == asked) {
            Some(s) => s.2 = false,
            None => {
                let score = scores.get(&asked).copied().unwrap_or(0.0);
                stems.push((asked, score, false));
            }
        }
    }
    stems.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    stems
        .into_iter()
        .map(|(stem, score, auto)| {
            let lens = Lens::new(&stem);
            overlay(&lens, score, auto, units, &slices, groups, files, analyses)
        })
        .collect()
}

/// The tokens on a unit's changed lines, per side.
struct UnitTokens<'a> {
    unit: &'a Unit,
    old: &'a [Token],
    new: &'a [Token],
    analyzer: &'static dyn LanguageAnalyzer,
    /// The file was added or deleted as a whole.
    whole_file: bool,
}

impl<'a> UnitTokens<'a> {
    fn of(unit: &'a Unit, analyses: &'a Analyses, whole_file: bool) -> Option<Self> {
        let (old_an, new_an) = analyses.get(&unit.path)?;
        Some(Self {
            unit,
            old: slid_tokens(old_an, unit.old_start, unit.old.len(), unit.new.is_empty()),
            new: slid_tokens(new_an, unit.new_start, unit.new.len(), unit.old.is_empty()),
            analyzer: analyzer_for(&unit.path)?,
            whole_file,
        })
    }

    fn sides(&self) -> [&'a [Token]; 2] {
        [self.old, self.new]
    }
}

/// A pure deletion or insertion can slide when the lines around it repeat its own first or
/// last lines (the diff picked one of several equal alignments, often cutting through brackets:
/// `}),` / `http.get(…, () =>` instead of the whole handler). Take the tokens of the most
/// balanced placement, nearest first.
fn slid_tokens(an: &FileAnalysis, start: u32, len: usize, pure: bool) -> &[Token] {
    let here = lines_tokens(an, start, len);
    if !pure || len == 0 || imbalance(here) == 0 {
        return here;
    }
    let line = |n: u32| an.lines.get(n as usize - 1);
    let end = start + len as u32 - 1;
    let mut best = (imbalance(here), 0u32, here);
    for dir in [-1i64, 1] {
        let (mut s, mut e) = (start, end);
        for k in 1..=MAX_SLIDE {
            // Up: the line above equals the last line; down: the line below equals the first.
            let ok = if dir < 0 {
                s > 1 && line(s - 1) == line(e)
            } else {
                line(e + 1).is_some() && line(s) == line(e + 1)
            };
            if !ok {
                break;
            }
            (s, e) = if dir < 0 {
                (s - 1, e - 1)
            } else {
                (s + 1, e + 1)
            };
            let toks = lines_tokens(an, s, len);
            let score = imbalance(toks);
            if (score, k) < (best.0, best.1) {
                best = (score, k, toks);
            }
        }
    }
    best.2
}

/// Unmatched brackets in a token run.
fn imbalance(tokens: &[Token]) -> usize {
    let mut stack: Vec<&str> = Vec::new();
    let mut unmatched = 0;
    for t in tokens.iter().filter(|t| t.kind == TokenKind::Op) {
        match t.value.as_str() {
            "(" | "[" | "{" | "${" => stack.push(closer_of(&t.value).expect("opener")),
            ")" | "]" | "}" => {
                if stack.last() == Some(&t.value.as_str()) {
                    stack.pop();
                } else {
                    unmatched += 1;
                }
            }
            _ => {}
        }
    }
    unmatched + stack.len()
}

/// Tokens starting on lines `start..start + len` (binary search; tokens are in source order).
fn lines_tokens(an: &FileAnalysis, start: u32, len: usize) -> &[Token] {
    &an.tokens[lines_range(an, start, len)]
}

fn lines_range(an: &FileAnalysis, start: u32, len: usize) -> std::ops::Range<usize> {
    if len == 0 {
        return 0..0;
    }
    let last = start + len as u32 - 1;
    let lo = an.tokens.partition_point(|t| t.start.line < start);
    let hi = an.tokens.partition_point(|t| t.start.line <= last);
    lo..hi.max(lo)
}

/// Each word's score: the share of changed identifiers it accounts for on the side that has
/// more of it, net of the other side. A concept that is removed (or added) is one-sided; a word
/// that survives a rename (`getUser` → `fetchUser`) cancels out.
fn word_scores(slices: &[UnitTokens<'_>]) -> HashMap<String, f64> {
    let mut counts: HashMap<String, [usize; 2]> = HashMap::new();
    let mut total = 0;
    let mut cache: HashMap<&str, Vec<String>> = HashMap::new();
    for s in slices {
        for (side, tok) in s
            .sides()
            .into_iter()
            .enumerate()
            .flat_map(|(side, toks)| toks.iter().map(move |t| (side, t)))
        {
            if tok.kind != TokenKind::Name || s.analyzer.is_keyword(&tok.value) {
                continue;
            }
            total += 1;
            let ws = cache.entry(tok.value.as_str()).or_insert_with(|| {
                let mut ws: Vec<String> = words(&tok.value)
                    .into_iter()
                    .filter(|w| concept_word(w))
                    .collect();
                ws.sort();
                ws.dedup();
                ws
            });
            for w in ws.iter() {
                counts.entry(w.clone()).or_default()[side] += 1;
            }
        }
    }
    counts
        .into_iter()
        .map(|(w, [old, new])| (w, old.abs_diff(new) as f64 / total as f64))
        .collect()
}

fn concept_word(w: &str) -> bool {
    w.len() >= 3
        && w.chars().all(|c| c.is_ascii_alphabetic())
        && !STOP_WORDS.iter().any(|s| s.eq_ignore_ascii_case(w))
}

/// Split an identifier (or any text) into lowercase, singular words: camelCase, PascalCase,
/// snake_case, SCREAMING_CASE, kebab-case and dotted paths all split.
pub fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(fold_plural(&cur.to_lowercase()));
            cur.clear();
        }
    };
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            flush(&mut cur, &mut out);
            continue;
        }
        if let Some(&p) = i.checked_sub(1).map(|j| &chars[j]) {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (p.is_lowercase() && c.is_uppercase())
                || (p.is_uppercase() && c.is_uppercase() && next_lower)
                || (p.is_ascii_digit() != c.is_ascii_digit() && p.is_alphanumeric());
            if boundary {
                flush(&mut cur, &mut out);
            }
        }
        cur.push(c);
    }
    flush(&mut cur, &mut out);
    out
}

/// `programs` → `program`, `categories` → `category`; `status`, `access` stay.
fn fold_plural(w: &str) -> String {
    if w.len() > 4 && w.ends_with("ies") {
        return format!("{}y", &w[..w.len() - 3]);
    }
    if w.len() > 3 && w.ends_with('s') && !["ss", "us", "is", "os"].iter().any(|e| w.ends_with(e)) {
        return w[..w.len() - 1].to_string();
    }
    w.to_string()
}

/// The stem a user-supplied concept name stands for: its longest non-stop word.
fn normalize_stem(name: &str) -> Option<String> {
    let ws = words(name);
    ws.iter()
        .filter(|w| concept_word(w))
        .max_by_key(|w| w.len())
        .or_else(|| ws.iter().max_by_key(|w| w.len()))
        .cloned()
}

/// One concept stem and the cheap substring test that filters tokens before word splitting.
struct Lens {
    stem: String,
    /// Lowercase substrings of which at least one appears in any text mentioning the stem.
    needles: Vec<String>,
}

impl Lens {
    fn new(stem: &str) -> Self {
        let mut needles = vec![stem.to_string()];
        if let Some(base) = stem.strip_suffix('y') {
            needles.push(format!("{base}ies"));
        }
        Self {
            stem: stem.to_string(),
            needles,
        }
    }

    fn mentions(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.needles.iter().any(|n| lower.contains(n.as_str())) && words(text).contains(&self.stem)
    }

    /// A token that names the concept: an identifier, string or comment containing the stem.
    fn token(&self, t: &Token) -> bool {
        matches!(
            t.kind,
            TokenKind::Name | TokenKind::String | TokenKind::Comment
        ) && self.mentions(&t.value)
    }
}

#[allow(clippy::too_many_arguments)]
fn overlay(
    lens: &Lens,
    score: f64,
    auto: bool,
    units: &IndexMap<String, Unit>,
    slices: &[UnitTokens<'_>],
    groups: &[Group],
    files: &[FileSummary],
    analyses: &Analyses,
) -> Concept {
    let mut explained: Vec<String> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    for s in slices {
        match strip_unit(lens, s) {
            Some(true) => explained.push(s.unit.id.clone()),
            Some(false) => touched.push(s.unit.id.clone()),
            None => {}
        }
    }

    let explained_set: HashSet<&str> = explained.iter().map(String::as_str).collect();
    let mut lines: HashMap<&str, ConceptFile> = HashMap::new();
    for u in units.values() {
        let cf = lines.entry(u.path.as_str()).or_default();
        let n = (u.old.len() + u.new.len()) as u32;
        if explained_set.contains(u.id.as_str()) {
            cf.concept_lines += n;
        } else {
            cf.collateral_lines += n;
        }
    }
    // Files in report order; files without units (unanalyzed, pure renames) are left out.
    let by_file: IndexMap<String, ConceptFile> = files
        .iter()
        .filter_map(|f| lines.get(f.path.as_str()).map(|cf| (f.path.clone(), *cf)))
        .collect();

    Concept {
        stem: lens.stem.clone(),
        auto,
        score: (score * 1000.0).round() / 1000.0,
        explained_unit_ids: explained,
        touched_unit_ids: touched,
        patterns: patterns(lens, units, groups, files, analyses),
        files: by_file,
    }
}

// ---------------------------------------------------------------------------------------------
// Strip-equivalence

/// A token, or a bracketed group with its body.
enum Item<'a> {
    Tok(&'a Token),
    Group {
        open: &'a Token,
        body: Vec<Item<'a>>,
        close: &'a Token,
    },
}

impl<'a> Item<'a> {
    fn first_line(&self) -> u32 {
        match self {
            Item::Tok(t) => t.start.line,
            Item::Group { open, .. } => open.start.line,
        }
    }

    fn last_line(&self) -> u32 {
        match self {
            Item::Tok(t) => t.end.line,
            Item::Group { close, .. } => close.end.line,
        }
    }

    /// Every token, brackets included, in order.
    fn tokens(&self) -> Box<dyn Iterator<Item = &'a Token> + '_> {
        match self {
            Item::Tok(t) => Box::new(std::iter::once(*t)),
            Item::Group { open, body, close } => Box::new(
                std::iter::once(*open)
                    .chain(body.iter().flat_map(|it| it.tokens()))
                    .chain(std::iter::once(*close)),
            ),
        }
    }

    fn tok(&self) -> Option<&'a Token> {
        match self {
            Item::Tok(t) => Some(*t),
            Item::Group { .. } => None,
        }
    }
}

fn closer_of(open: &str) -> Option<&'static str> {
    match open {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" | "${" => Some("}"),
        "<" => Some(">"),
        _ => None,
    }
}

/// Whether `tokens[i]` opens a group. `<` only does right after a name (`Pick<`), where it
/// starts type arguments rather than a comparison or a JSX tag.
fn opens(tokens: &[Token], i: usize) -> bool {
    let t = &tokens[i];
    if t.kind != TokenKind::Op {
        return false;
    }
    if t.value == "<" {
        return i
            .checked_sub(1)
            .is_some_and(|p| tokens[p].kind == TokenKind::Name && tokens[p].end == t.start);
    }
    closer_of(&t.value).is_some()
}

/// Nest tokens by brackets. A unit is a slice of lines, so brackets may be unbalanced:
/// unmatched closers stay plain tokens and unclosed openers flatten into their parent.
fn nest(tokens: &[Token]) -> Vec<Item<'_>> {
    let mut stack: Vec<(&Token, Vec<Item<'_>>)> = Vec::new();
    let mut top: Vec<Item<'_>> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if opens(tokens, i) {
            stack.push((t, Vec::new()));
            continue;
        }
        if t.kind == TokenKind::Op
            && let Some(pos) = stack
                .iter()
                .rposition(|(o, _)| closer_of(&o.value) == Some(t.value.as_str()))
        {
            // Openers above the match never closed: flatten them into the match's body.
            while stack.len() > pos + 1 {
                let (o, body) = stack.pop().expect("above pos");
                let parent = &mut stack.last_mut().expect("pos exists").1;
                parent.push(Item::Tok(o));
                parent.extend(body);
            }
            let (open, body) = stack.pop().expect("matched");
            let group = Item::Group {
                open,
                body,
                close: t,
            };
            match stack.last_mut() {
                Some((_, b)) => b.push(group),
                None => top.push(group),
            }
            continue;
        }
        match stack.last_mut() {
            Some((_, b)) => b.push(Item::Tok(t)),
            None => top.push(Item::Tok(t)),
        }
    }
    while let Some((o, body)) = stack.pop() {
        let parent = match stack.last_mut() {
            Some((_, b)) => b,
            None => &mut top,
        };
        parent.push(Item::Tok(o));
        parent.extend(body);
    }
    top
}

/// Splits list elements and statements.
fn is_separator(t: &Token) -> bool {
    t.kind == TokenKind::Structural || (t.kind == TokenKind::Op && matches!(&*t.value, "," | ";"))
}

/// Ignored when comparing stripped sides.
fn is_noise(t: &Token) -> bool {
    is_separator(t) || t.kind == TokenKind::Comment
}

/// Statements whose removal changes control flow: dropped with the concept only when what
/// they still do is neutral.
const CONTROL: &[&str] = &[
    "if", "else", "elif", "for", "while", "do", "switch", "try", "with",
];

/// Error reporting reads as neutral whatever the helper is called (`setSubmitError`).
const NEUTRAL_WORDS: &[&str] = &[
    "error", "toast", "toaster", "warn", "warning", "notify", "log",
];

fn is_neutral(t: &Token, next: Option<&Token>, analyzer: &dyn LanguageAnalyzer) -> bool {
    t.kind != TokenKind::Name
        || analyzer.is_keyword(&t.value)
        || NEUTRAL.contains(&t.value.as_str())
        || STOP_WORDS.contains(&t.value.as_str())
        // An object key (`title: …`) names a slot, not an action.
        || next.is_some_and(|n| n.kind == TokenKind::Op && n.value == ":")
        || words(&t.value).iter().any(|w| NEUTRAL_WORDS.contains(&w.as_str()))
}

/// What stripping took out of a bracketed body.
#[derive(Clone, Copy, Debug, Default)]
struct Loss {
    segments: usize,
    dropped: usize,
    /// Damaged groups inside the segments that stayed.
    nested: usize,
}

impl Loss {
    /// Half its elements or more went with the concept, so what remains is broken
    /// (`expect()`, `if () {…}`, `http.get(, handler)`).
    fn damaged(self) -> bool {
        self.dropped > 0 && self.dropped * 2 >= self.segments
    }

    fn gutted(self) -> bool {
        self.dropped > 0 && self.dropped == self.segments
    }
}

/// How a statement whose arguments went with the concept is treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// It stays (`refetch(programId)` → `refetch()` still matches the other side).
    Keep,
    /// It goes too (a deleted `expect(x.programId).toBe("p1")`).
    Drop,
}

/// `Some(true)`: the unit mentions the concept and is nothing else; `Some(false)`: it
/// mentions it but changes more; `None`: no mention.
fn strip_unit(lens: &Lens, s: &UnitTokens<'_>) -> Option<bool> {
    // A whole file named after the concept goes with it.
    if s.whole_file && lens.mentions(&s.unit.path) {
        return Some(true);
    }
    let mut mentioned = false;
    let mut concept_vals: [Vec<&str>; 2] = [Vec::new(), Vec::new()];
    for (side, toks) in s.sides().into_iter().enumerate() {
        for t in toks {
            if lens.token(t) {
                mentioned = true;
                if t.kind != TokenKind::Comment {
                    concept_vals[side].push(&t.value);
                }
            }
        }
    }
    if !mentioned {
        return None;
    }
    // The concept itself must be added or removed, not just edited around: one side's concept
    // tokens are a strict superset of the other's (or only comments mention it).
    let [mut a, mut b] = concept_vals;
    a.sort_unstable();
    b.sort_unstable();
    let only_comments = a.is_empty() && b.is_empty();
    let one_way = is_sub_multiset(&a, &b) != is_sub_multiset(&b, &a);
    if !only_comments && !one_way {
        return Some(false);
    }
    let (old_items, new_items) = (nest(s.old), nest(s.new));
    for drop_strings in [true, false] {
        for mode in [Mode::Keep, Mode::Drop] {
            let cx = Cx {
                lens,
                analyzer: s.analyzer,
                mode,
                drop_strings,
            };
            if aligned(lens, &cx.body(&old_items).0, &cx.body(&new_items).0) {
                return Some(true);
            }
        }
    }
    let raw_old: Vec<&Token> = s.old.iter().collect();
    let raw_new: Vec<&Token> = s.new.iter().collect();
    Some(aligned(lens, &raw_old, &raw_new))
}

/// `a ⊆ b` for sorted slices.
fn is_sub_multiset(a: &[&str], b: &[&str]) -> bool {
    let mut j = 0;
    for x in a {
        while j < b.len() && b[j] < *x {
            j += 1;
        }
        if j == b.len() || b[j] != *x {
            return false;
        }
        j += 1;
    }
    true
}

/// Whether two token streams differ only by the concept: align them and accept only concept
/// tokens inserted or deleted, a string that mentions the concept reworded
/// (`"… without program mutations"` → `"…"`), or the concept's word cut out of an identifier
/// (`SiteProgramPicker` → `SitePicker`). Used on the stripped streams and, as the in-place
/// variant, on the raw ones.
fn aligned(lens: &Lens, old: &[&Token], new: &[&Token]) -> bool {
    let old: Vec<&Token> = old.iter().copied().filter(|t| !is_noise(t)).collect();
    let new: Vec<&Token> = new.iter().copied().filter(|t| !is_noise(t)).collect();
    if old
        .iter()
        .map(|t| &t.value)
        .eq(new.iter().map(|t| &t.value))
    {
        return true;
    }
    fn key<'t>(t: &&'t Token) -> (bool, &'t str) {
        (t.kind == TokenKind::String, t.value.as_str())
    }
    let ka: Vec<(bool, &str)> = old.iter().map(key).collect();
    let kb: Vec<(bool, &str)> = new.iter().map(key).collect();
    let sm = SequenceMatcher::new(&ka, &kb);
    let cut = |with: &Token, without: &Token| {
        with.kind == without.kind
            && matches!(with.kind, TokenKind::Name | TokenKind::String)
            && lens.token(with)
            && words(&with.value)
                .into_iter()
                .filter(|w| *w != lens.stem)
                .eq(words(&without.value))
    };
    // Prose only (titles, messages): a key like `"…OutOfProgram"` → `"…OutOfSite"` picks a
    // different message, which is a change of its own.
    let reworded = |a: &Token, b: &Token| {
        a.kind == TokenKind::String
            && b.kind == TokenKind::String
            && (lens.token(a) || lens.token(b))
            && a.value.contains(' ')
            && b.value.contains(' ')
    };
    sm.opcodes().iter().all(|op| {
        let (a, b) = (&old[op.i1..op.i2], &new[op.j1..op.j2]);
        match op.tag {
            Tag::Equal => true,
            Tag::Delete => a.iter().all(|t| lens.token(t)),
            Tag::Insert => b.iter().all(|t| lens.token(t)),
            Tag::Replace => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|(x, y)| cut(x, y) || cut(y, x) || reworded(x, y))
            }
        }
    })
}

struct Cx<'l> {
    lens: &'l Lens,
    analyzer: &'static dyn LanguageAnalyzer,
    mode: Mode,
    /// Strings naming the concept take their construct with them (`false`: they stay, and
    /// match any string on the other side, for reworded titles and messages).
    drop_strings: bool,
}

impl Cx<'_> {
    /// A token whose presence takes its construct with it.
    fn drops(&self, t: &Token) -> bool {
        match t.kind {
            TokenKind::Name => self.lens.token(t),
            TokenKind::String => self.drop_strings && self.lens.token(t),
            _ => false,
        }
    }

    /// Whether a JSX attribute can follow `prev`: the tag name, another attribute's value or
    /// a bare attribute (not `const x =`, `type X =` or `a.x =`).
    fn attr_follows(&self, prev: &Item<'_>) -> bool {
        match prev {
            Item::Group { open, .. } => open.value == "{",
            Item::Tok(t) => match t.kind {
                TokenKind::Name => !self.analyzer.is_keyword(&t.value),
                TokenKind::String => true,
                _ => false,
            },
        }
    }

    /// Strip a sequence: split it into segments (list elements, statements, lines) and strip
    /// each one.
    fn body<'a>(&self, items: &[Item<'a>]) -> (Vec<&'a Token>, Loss) {
        let mut out = Vec::new();
        let mut loss = Loss::default();
        let mut seg: Vec<&Item<'a>> = Vec::new();
        let mut prev_line: Option<u32> = None;
        let mut flush = |seg: &mut Vec<&Item<'a>>, out: &mut Vec<&'a Token>| {
            if has_content(seg) {
                loss.segments += 1;
                match self.segment(seg) {
                    Some((toks, damage)) => {
                        out.extend(toks);
                        loss.nested += damage;
                    }
                    None => loss.dropped += 1,
                }
            } else {
                out.extend(seg.iter().filter_map(|it| it.tok()));
            }
            seg.clear();
        };
        for item in items {
            let boundary = item.tok().is_some_and(is_separator);
            let new_line = prev_line.is_some_and(|l| item.first_line() > l)
                && !seg.last().is_some_and(|prev| continues(prev, item));
            if boundary || new_line {
                flush(&mut seg, &mut out);
            }
            if boundary {
                prev_line = None;
                continue;
            }
            prev_line = Some(item.last_line());
            seg.push(item);
        }
        flush(&mut seg, &mut out);
        (out, loss)
    }

    /// Strip one segment; `None` when the whole segment goes with the concept. Otherwise the
    /// stripped tokens and how many groups inside it the concept damaged.
    fn segment<'a>(&self, seg: &[&Item<'a>]) -> Option<(Vec<&'a Token>, usize)> {
        // A test block whose title names the concept tests the concept.
        if let Some(Item::Tok(first)) = seg.first()
            && matches!(first.value.as_str(), "it" | "test" | "describe")
            && let Some(Item::Group { open, body, .. }) =
                seg.iter().find(|it| matches!(it, Item::Group { .. }))
            && open.value == "("
            && first_segment(body).any(|t| self.lens.token(t))
        {
            return None;
        }
        // JSX attributes (`programId={x}`, `programId="x"`) go on their own.
        let mut items: Vec<&Item<'a>> = Vec::with_capacity(seg.len());
        let mut i = 0;
        while i < seg.len() {
            if let Some(t) = seg[i].tok()
                && t.kind == TokenKind::Name
                && self.drops(t)
                && seg
                    .get(i + 1)
                    .and_then(|n| n.tok())
                    .is_some_and(|n| n.value == "=")
                && i + 2 < seg.len()
                && i.checked_sub(1).is_none_or(|p| self.attr_follows(seg[p]))
            {
                i += 3;
                continue;
            }
            items.push(seg[i]);
            i += 1;
        }
        if !has_content(&items) {
            return None;
        }
        // A concept token directly in the segment takes the segment with it.
        if items
            .iter()
            .filter_map(|it| it.tok())
            .any(|t| self.drops(t))
        {
            return None;
        }
        let mut out: Vec<&'a Token> = Vec::new();
        let mut damaged = false;
        let mut damage = 0;
        for (k, it) in items.iter().enumerate() {
            match it {
                Item::Tok(t) => out.push(*t),
                Item::Group { open, body, close } => {
                    let (stripped, loss) = self.body(body);
                    if open.value == "${" && loss.gutted() {
                        // A template hole that only held the concept: drop it and the
                        // separator text in front of it.
                        let next_is_text = items
                            .get(k + 1)
                            .and_then(|n| n.tok())
                            .is_some_and(|n| n.kind == TokenKind::String);
                        if next_is_text && out.last().is_some_and(|p| p.kind == TokenKind::String) {
                            out.pop();
                        }
                        continue;
                    }
                    damaged |= loss.damaged();
                    damage += usize::from(loss.damaged()) + loss.nested;
                    out.push(*open);
                    out.extend(stripped);
                    out.push(*close);
                }
            }
        }
        if damaged && self.mode == Mode::Drop {
            let control = items
                .first()
                .and_then(|it| it.tok())
                .is_some_and(|t| CONTROL.contains(&t.value.as_str()));
            let neutral = || {
                out.iter()
                    .enumerate()
                    .all(|(k, t)| is_neutral(t, out.get(k + 1).copied(), self.analyzer))
            };
            // A guard goes when what it still does is neutral, or when its body was about the
            // concept too (`if (programId) rows = rows.filter(…programId…)`).
            if !control || damage >= 2 || neutral() {
                return None;
            }
        }
        Some((out, damage))
    }
}

/// Whether `next`, on a later line, continues the expression `prev` ends (`=>`, `=`, `&&` at the
/// end of a line, `.`/`?`/`:` at the start of the next) instead of starting a new statement or
/// JSX child.
fn continues(prev: &Item<'_>, next: &Item<'_>) -> bool {
    let op = |it: &Item<'_>, exclude: &[&str]| {
        it.tok()
            .is_some_and(|t| t.kind == TokenKind::Op && !exclude.contains(&t.value.as_str()))
    };
    op(prev, &[">", "/>", "}", ")", "]"]) || op(next, &["<", "</", "{", "(", "["])
}

/// The tokens of a body's first segment (a call's first argument).
fn first_segment<'b, 'a: 'b>(body: &'b [Item<'a>]) -> impl Iterator<Item = &'a Token> + 'b {
    body.iter()
        .take_while(|it| !it.tok().is_some_and(is_separator))
        .flat_map(|it| it.tokens())
}

fn has_content(items: &[&Item<'_>]) -> bool {
    items.iter().any(|it| match it {
        Item::Tok(t) => !is_noise(t),
        Item::Group { .. } => true,
    })
}

// ---------------------------------------------------------------------------------------------
// Patterns

/// Mechanical groups that mention the concept, merged by key once literals are normalized.
fn patterns(
    lens: &Lens,
    units: &IndexMap<String, Unit>,
    groups: &[Group],
    files: &[FileSummary],
    analyses: &Analyses,
) -> Vec<ConceptPattern> {
    let category: HashMap<&str, Category> = files
        .iter()
        .map(|f| (f.path.as_str(), f.category))
        .collect();
    let mut merged: IndexMap<String, (ConceptPattern, usize)> = IndexMap::new();
    for g in groups.iter().filter(|g| g.mechanical) {
        if !(lens.mentions(&g.label) || lens.mentions(&g.old) || lens.mentions(&g.new)) {
            continue;
        }
        let norm_key = normalize_literals(&g.key);
        let entry = merged.entry(norm_key.clone()).or_insert_with(|| {
            (
                ConceptPattern {
                    norm_key,
                    label: normalize_literals(&g.label),
                    role: role(lens, g, units, &category, analyses),
                    group_ids: Vec::new(),
                },
                0,
            )
        });
        entry.0.group_ids.push(g.id.clone());
        entry.1 += g.unit_ids.len();
    }
    let mut out: Vec<(ConceptPattern, usize)> = merged.into_values().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.norm_key.cmp(&b.0.norm_key)));
    out.into_iter().map(|(p, _)| p).collect()
}

/// Replace quoted strings with `<str>` and numeric literals with `<num>`.
pub fn normalize_literals(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '"' | '\'' | '`') {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            if j < chars.len() {
                out.push_str("<str>");
                i = j + 1;
                continue;
            }
        }
        let after_word =
            i > 0 && (chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '$'));
        if c.is_ascii_digit() && !after_word {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || matches!(chars[j], '.' | '_'))
            {
                j += 1;
            }
            out.push_str("<num>");
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn role(
    lens: &Lens,
    g: &Group,
    units: &IndexMap<String, Unit>,
    category: &HashMap<&str, Category>,
    analyses: &Analyses,
) -> ConceptRole {
    if g.kind == SignatureKind::Import {
        return ConceptRole::Import;
    }
    let lower: Vec<String> = g.files.iter().map(|p| p.to_lowercase()).collect();
    let i18n_file = |p: &String| {
        p.ends_with(".json")
            && p.split('/')
                .any(|s| matches!(s, "i18n" | "locales" | "locale"))
    };
    if g.kind == SignatureKind::I18n || (!lower.is_empty() && lower.iter().all(i18n_file)) {
        return ConceptRole::I18n;
    }
    let fixture_file = |p: &String| {
        let segs: Vec<&str> = p.split('/').collect();
        segs[..segs.len() - 1]
            .iter()
            .any(|s| FIXTURE_DIRS.contains(s))
            || segs
                .last()
                .is_some_and(|n| n.contains("mock") || n.contains("fixture"))
    };
    if g.kind == SignatureKind::Fixture || (!lower.is_empty() && lower.iter().all(fixture_file)) {
        return ConceptRole::Fixture;
    }
    if g.kind == SignatureKind::Test
        || (!g.files.is_empty()
            && g.files
                .iter()
                .all(|p| category.get(p.as_str()) == Some(&Category::Tests)))
    {
        return ConceptRole::Test;
    }
    let Some(unit) = g.unit_ids.first().and_then(|id| units.get(id)) else {
        return ConceptRole::Other;
    };
    if let Some((old_an, new_an)) = analyses.get(&unit.path) {
        let sides = [
            (old_an, unit.old_start, unit.old.len()),
            (new_an, unit.new_start, unit.new.len()),
        ];
        for (an, start, len) in sides {
            let rng = lines_range(an, start, len);
            if let Some(at) = rng.clone().find(|&k| lens.token(&an.tokens[k])) {
                if in_deps_array(&an.tokens, at) {
                    return ConceptRole::Deps;
                }
            }
        }
    }
    let line = unit
        .old
        .iter()
        .chain(&unit.new)
        .map(|l| l.text.trim())
        .find(|l| lens.mentions(l))
        .unwrap_or("");
    line_role(line)
}

/// Whether `tokens[at]` sits in the `[...]` that is the last argument of a deps hook.
fn in_deps_array(tokens: &[Token], at: usize) -> bool {
    let op = |t: &Token, v: &str| t.kind == TokenKind::Op && t.value == v;
    // Back to the unmatched `[`.
    let mut depth = 0i32;
    let mut open = None;
    for k in (at.saturating_sub(MAX_SCAN)..at).rev() {
        let t = &tokens[k];
        if op(t, ")") || op(t, "]") || op(t, "}") {
            depth += 1;
        } else if op(t, "(") || op(t, "[") || op(t, "{") || op(t, "${") {
            if depth == 0 {
                open = op(t, "[").then_some(k);
                break;
            }
            depth -= 1;
        }
    }
    let Some(open) = open else {
        return false;
    };
    // Forward to its `]`, which must close the call.
    let mut depth = 0i32;
    let mut close = None;
    for (k, t) in tokens.iter().enumerate().skip(open + 1).take(MAX_SCAN) {
        if op(t, "(") || op(t, "[") || op(t, "{") || op(t, "${") {
            depth += 1;
        } else if op(t, ")") || op(t, "]") || op(t, "}") {
            if depth == 0 {
                close = op(t, "]").then_some(k);
                break;
            }
            depth -= 1;
        }
    }
    let Some(close) = close else {
        return false;
    };
    let mut next = close + 1;
    if tokens.get(next).is_some_and(|t| op(t, ",")) {
        next += 1;
    }
    if !tokens.get(next).is_some_and(|t| op(t, ")")) {
        return false;
    }
    if !open
        .checked_sub(1)
        .and_then(|p| tokens.get(p))
        .is_some_and(|t| op(t, ","))
    {
        return false;
    }
    // Back from the `[` to the call's `(`.
    let mut depth = 0i32;
    for k in (open.saturating_sub(MAX_SCAN)..open).rev() {
        let t = &tokens[k];
        if op(t, ")") || op(t, "]") || op(t, "}") {
            depth += 1;
        } else if op(t, "(") || op(t, "[") || op(t, "{") || op(t, "${") {
            if depth == 0 {
                return op(t, "(")
                    && k.checked_sub(1)
                        .and_then(|p| tokens.get(p))
                        .is_some_and(|c| DEPS_HOOKS.contains(&c.value.as_str()));
            }
            depth -= 1;
        }
    }
    false
}

/// Classify a changed line by its shape.
fn line_role(line: &str) -> ConceptRole {
    let ident_end = line
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '$')))
        .map_or(line.len(), |(i, _)| i);
    if ident_end == 0 {
        return ConceptRole::Other;
    }
    let rest = &line[ident_end..];
    if rest.starts_with("?:") {
        return ConceptRole::Type;
    }
    if rest.starts_with("={") || rest.starts_with("=\"") || rest.starts_with("='") {
        return ConceptRole::Prop;
    }
    if let Some(value) = rest.strip_prefix(':') {
        let value = value.trim();
        let ends_member = value.ends_with(';') || !value.ends_with(',');
        let ty = value.trim_end_matches([';', ',']);
        let primitive = matches!(
            ty,
            "string" | "number" | "boolean" | "bigint" | "unknown" | "any" | "null" | "undefined"
        );
        let type_like = primitive
            || ty.ends_with("[]")
            || (ty.starts_with(|c: char| c.is_ascii_uppercase())
                && ty
                    .chars()
                    .all(|c| c.is_alphanumeric() || "<>[]|&_ ,.".contains(c)));
        if type_like && ends_member {
            return ConceptRole::Type;
        }
        return ConceptRole::Field;
    }
    if rest.is_empty() || rest == "," {
        return ConceptRole::Field;
    }
    ConceptRole::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_identifiers_into_singular_words() {
        assert_eq!(words("currentProgramId"), ["current", "program", "id"]);
        assert_eq!(words("PROGRAM_IDS"), ["program", "ids"]);
        assert_eq!(words("HTTPServer"), ["http", "server"]);
        assert_eq!(words("program-categories"), ["program", "category"]);
        assert_eq!(words("status"), ["status"]);
        assert_eq!(words("prog1"), ["prog", "1"]);
    }

    #[test]
    fn normalizes_literals() {
        assert_eq!(
            normalize_literals("programId: 'p1', n: 42, x1: \"a\\\"b\""),
            "programId: <str>, n: <num>, x1: <str>"
        );
    }

    #[test]
    fn line_roles() {
        assert_eq!(line_role("programId?: string;"), ConceptRole::Type);
        assert_eq!(line_role("program: Program;"), ConceptRole::Type);
        assert_eq!(line_role("programId: \"p1\","), ConceptRole::Field);
        assert_eq!(line_role("programId={programId}"), ConceptRole::Prop);
        assert_eq!(line_role("programId,"), ConceptRole::Field);
        assert_eq!(line_role("const x = 1;"), ConceptRole::Other);
    }

    #[test]
    fn user_stems_normalize() {
        assert_eq!(normalize_stem("Programs").as_deref(), Some("program"));
        assert_eq!(normalize_stem("programId").as_deref(), Some("program"));
    }

    #[test]
    fn finds_hook_deps_arrays() {
        let an = analyzer_for("a.ts").unwrap().analyze(
            "useEffect(() => {\n  load();\n}, [siteId, programId]);\nconst ids = [siteId, programId];\n",
        );
        let at: Vec<usize> = (0..an.tokens.len())
            .filter(|&i| an.tokens[i].value == "programId")
            .collect();
        assert!(in_deps_array(&an.tokens, at[0]));
        assert!(!in_deps_array(&an.tokens, at[1]));
    }

    #[test]
    fn nests_unbalanced_slices() {
        let an = analyzer_for("a.ts")
            .unwrap()
            .analyze("}),\nfoo(a, [b]);\nbar({\n");
        let items = nest(&an.tokens);
        let groups = items
            .iter()
            .filter(|it| matches!(it, Item::Group { .. }))
            .count();
        assert_eq!(groups, 1, "only foo(...) closes");
        assert_eq!(imbalance(&an.tokens), 4);
    }
}
