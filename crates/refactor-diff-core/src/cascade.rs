//! Cascade cleanup: an import or local deleted because its last use was deleted in the same
//! file is re-signed [`SignatureKind::Cascade`](crate::model::SignatureKind::Cascade).
//!
//! A name qualifies when it is gone from the head side of the file entirely (no token spells
//! it), and every base-side mention of it sits on a line this diff removes or rewrites, with at
//! least one mention besides the binding itself. A unit is re-signed only when all it does is
//! drop such names: import bindings (whole statements or names out of a list), or
//! `const`/`let`/`var` declarations whose initializer does nothing visible except call hooks
//! (`useX(...)`). Hook calls still count, with detail `hook`, so the UI can flag them: dropping
//! one stops whatever it fetched or subscribed to.
//!
//! The check is token-based and conservative: any other mention of the name at head (even a
//! property of the same name) keeps the unit out.
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::classify::tokens_in_range;
use crate::lang::{FileAnalysis, ImportSite, Lang, Token, TokenKind};
use crate::model::{Signature, SignatureKind, Unit};
use crate::moves::Analyses;

const NEWLINE: &str = "<NEWLINE>";

/// Re-sign units that only drop names whose last use the diff deleted.
pub fn link(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    let mut by_path: IndexMap<String, Vec<String>> = IndexMap::new();
    for u in units.values() {
        by_path
            .entry(u.path.clone())
            .or_default()
            .push(u.id.clone());
    }
    for (path, ids) in by_path {
        let Some((old_an, new_an)) = analyses.get(&path) else {
            continue;
        };
        // A deleted (or added) file has no "afterwards" to be unused in; broken trees can't be
        // trusted to tell statements apart.
        if !old_an.parsed || !new_an.parsed || old_an.tokens.is_empty() || new_an.tokens.is_empty()
        {
            continue;
        }
        let changed: HashSet<u32> = ids
            .iter()
            .filter_map(|id| old_range(&units[id]))
            .flat_map(|(a, b)| a..=b)
            .collect();
        let gone = gone_names(old_an, new_an, &changed);
        if gone.is_empty() {
            continue;
        }
        for id in ids {
            let u = &units[&id];
            if u.partner.is_some() || u.signature_kinds().any(|k| k == SignatureKind::Move) {
                continue;
            }
            if let Some(sigs) = cascade_signatures(u, old_an, new_an, &gone) {
                units[&id].signatures = sigs;
            }
        }
    }
}

/// Names that no head token spells and that the base mentions at least twice, only on
/// `changed` lines.
fn gone_names<'a>(
    old_an: &'a FileAnalysis,
    new_an: &FileAnalysis,
    changed: &HashSet<u32>,
) -> HashSet<&'a str> {
    let head: HashSet<&str> = new_an
        .tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Name)
        .map(|t| t.value.as_str())
        .collect();
    let mut seen: HashMap<&str, (usize, bool)> = HashMap::new();
    for t in old_an.tokens.iter().filter(|t| t.kind == TokenKind::Name) {
        let e = seen.entry(t.value.as_str()).or_insert((0, true));
        e.0 += 1;
        e.1 &= changed.contains(&t.start.line);
    }
    seen.into_iter()
        .filter(|(name, (count, all_changed))| *count >= 2 && *all_changed && !head.contains(name))
        .map(|(name, _)| name)
        .collect()
}

fn cascade_signatures(
    u: &Unit,
    old_an: &FileAnalysis,
    new_an: &FileAnalysis,
    gone: &HashSet<&str>,
) -> Option<Vec<Signature>> {
    let old_rng = old_range(u)?;
    let new_rng = new_range(u);
    let (names, detail) =
        if let Some(names) = dropped_imports(old_an, new_an, old_rng, new_rng, gone) {
            (names, "import")
        } else if new_rng.is_none() {
            let python = Lang::for_path(&u.path).is_some_and(Lang::is_python);
            let (names, hook) = dropped_locals(python, old_an, old_rng, gone)?;
            (names, if hook { "hook" } else { "local" })
        } else {
            return None;
        };
    Some(
        names
            .into_iter()
            .map(|n| {
                Signature::new(SignatureKind::Cascade, format!("cascade\0{n}"), n, "")
                    .with_detail(detail)
            })
            .collect(),
    )
}

/// The gone names an import-only unit drops, when dropping them is all it does.
fn dropped_imports(
    old_an: &FileAnalysis,
    new_an: &FileAnalysis,
    old_rng: (u32, u32),
    new_rng: Option<(u32, u32)>,
    gone: &HashSet<&str>,
) -> Option<Vec<String>> {
    let old_sites = sites_covering(old_an, old_rng)?;
    if let Some(r) = new_rng {
        sites_covering(new_an, r)?;
    }
    let bound: HashSet<&str> = old_sites
        .iter()
        .flat_map(|s| s.bindings.iter().map(|b| b.alias.as_str()))
        .collect();
    let old_toks = code_tokens(old_an, Some(old_rng));
    let mut removed: Vec<String> = Vec::new();
    for t in &old_toks {
        let v = t.value.as_str();
        if t.kind == TokenKind::Name
            && gone.contains(v)
            && bound.contains(v)
            && !removed.iter().any(|r| r == v)
        {
            removed.push(v.to_string());
        }
    }
    if removed.is_empty() {
        return None;
    }
    // Whole statements whose every binding is dropped vanish with their keywords and module.
    let emptied: Vec<(u32, u32)> = old_sites
        .iter()
        .filter(|s| {
            old_rng.0 <= s.start
                && s.end <= old_rng.1
                && !s.bindings.is_empty()
                && s.bindings.iter().all(|b| removed.contains(&b.alias))
        })
        .map(|s| (s.start, s.end))
        .collect();
    let skeleton = |t: &&Token| {
        t.kind != TokenKind::Structural && !matches!(t.value.as_str(), "," | "(" | ")")
    };
    let old_rest: Vec<&str> = old_toks
        .iter()
        .copied()
        .filter(skeleton)
        .filter(|t| {
            !emptied
                .iter()
                .any(|&(a, b)| a <= t.start.line && t.start.line <= b)
        })
        .filter(|t| !(t.kind == TokenKind::Name && removed.contains(&t.value)))
        .map(|t| t.value.as_str())
        .collect();
    let new_rest: Vec<&str> = code_tokens(new_an, new_rng)
        .into_iter()
        .filter(skeleton)
        .map(|t| t.value.as_str())
        .collect();
    (old_rest == new_rest).then_some(removed)
}

/// The import statements overlapping `rng`, when every non-blank line in it belongs to one.
fn sites_covering(an: &FileAnalysis, (a, b): (u32, u32)) -> Option<Vec<&ImportSite>> {
    let sites: Vec<&ImportSite> = an
        .imports
        .iter()
        .filter(|s| s.start <= b && a <= s.end)
        .collect();
    let covered = (a..=b).all(|n| {
        sites.iter().any(|s| s.start <= n && n <= s.end)
            || an
                .lines
                .get(n as usize - 1)
                .is_none_or(|l| l.trim().is_empty())
    });
    (covered && !sites.is_empty()).then_some(sites)
}

/// The gone names a deletion-only unit of plain declarations binds, and whether any
/// initializer calls a hook.
fn dropped_locals(
    python: bool,
    old_an: &FileAnalysis,
    rng: (u32, u32),
    gone: &HashSet<&str>,
) -> Option<(Vec<String>, bool)> {
    let toks = code_tokens(old_an, Some(rng));
    let stmts = split_statements(&toks)?;
    if stmts.is_empty() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut hook = false;
    for s in &stmts {
        let (bound, init) = if python {
            py_declaration(s)?
        } else {
            ts_declaration(s)?
        };
        if bound.is_empty() || !bound.iter().all(|n| gone.contains(n)) {
            return None;
        }
        hook |= if python {
            py_pure(init)?
        } else {
            ts_effects(init)?
        };
        for n in bound {
            if !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        }
    }
    Some((names, hook))
}

/// `const|let|var <pattern> [: type] = <init>`: the bound names and the initializer. Patterns
/// may destructure (`{ a, b }`, `[a, b]`) but not rename, default or collect the rest.
fn ts_declaration<'a, 't>(s: &'a [&'t Token]) -> Option<(Vec<&'t str>, &'a [&'t Token])> {
    if !matches!(s.first()?.value.as_str(), "const" | "let" | "var") {
        return None;
    }
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut i = 1;
    loop {
        let t = s.get(i)?;
        match (t.kind, t.value.as_str()) {
            (TokenKind::Op, "{" | "[") => depth += 1,
            (TokenKind::Op, "}" | "]") => depth -= 1,
            (TokenKind::Op, ",") if depth > 0 => {}
            (TokenKind::Name, v) => names.push(v),
            (TokenKind::Op, "=") if depth == 0 => break,
            (TokenKind::Op, ":") if depth == 0 => {
                // A type annotation: skip to the `=` outside its brackets.
                i += depth0_position(&s[i..], "=")?;
                break;
            }
            _ => return None,
        }
        i += 1;
    }
    Some((names, &s[i + 1..]))
}

/// `name [: type] = <init>`.
fn py_declaration<'a, 't>(s: &'a [&'t Token]) -> Option<(Vec<&'t str>, &'a [&'t Token])> {
    let name = s.first().filter(|t| t.kind == TokenKind::Name)?;
    let eq = match s.get(1)?.value.as_str() {
        "=" => 1,
        ":" => 1 + depth0_position(&s[1..], "=")?,
        _ => return None,
    };
    Some((vec![name.value.as_str()], &s[eq + 1..]))
}

/// Index of the first `value` token outside brackets.
fn depth0_position(toks: &[&Token], value: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        if t.kind != TokenKind::Op {
            continue;
        }
        match t.value.as_str() {
            "(" | "[" | "{" | "${" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            v if v == value && depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Index of the bracket closing the one at `open`.
pub(crate) fn closing(toks: &[&Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if t.kind != TokenKind::Op {
            continue;
        }
        match t.value.as_str() {
            "(" | "[" | "{" | "${" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_assignment(op: &str) -> bool {
    op.ends_with('=') && !matches!(op, "==" | "===" | "!=" | "!==" | "<=" | ">=" | "=>")
}

enum Callee<'t> {
    /// `(` opens a group or a parameter list, not a call.
    Grouping,
    Name(&'t str),
    /// A call of something computed (`a[0](...)`, `f()(...)`, `f?.(...)`).
    Other,
}

/// What the `(` at `i` calls.
fn callee<'t>(toks: &[&'t Token], i: usize) -> Callee<'t> {
    let Some(prev) = i.checked_sub(1).map(|j| toks[j]) else {
        return Callee::Grouping;
    };
    match (prev.kind, prev.value.as_str()) {
        (TokenKind::Name, v) => Callee::Name(v),
        (TokenKind::Op, ")" | "]" | "?.") | (TokenKind::String, _) => Callee::Other,
        (TokenKind::Op, ">") => {
            // `f<T>(...)`: the name before the matching `<`; a comparison otherwise.
            let mut depth = 0i32;
            for j in (0..i).rev() {
                match toks[j].value.as_str() {
                    ">" => depth += 1,
                    "<" => {
                        depth -= 1;
                        if depth == 0 {
                            return match j.checked_sub(1).map(|k| toks[k]) {
                                Some(t) if t.kind == TokenKind::Name => Callee::Name(&t.value),
                                _ => Callee::Other,
                            };
                        }
                    }
                    _ => {}
                }
            }
            Callee::Grouping
        }
        _ => Callee::Grouping,
    }
}

/// `Some(true)` when the initializer calls hooks, `Some(false)` when it calls nothing, `None`
/// when it might do anything else (other calls, `new`, `await`, assignments). Arguments of a
/// hook call are the hook's business; a function value runs nothing.
fn ts_effects(init: &[&Token]) -> Option<bool> {
    let first = init.first()?;
    let arrow_after = |i: usize| init.get(i + 1).is_some_and(|t| t.value == "=>");
    if matches!(first.value.as_str(), "function" | "async")
        || (first.kind == TokenKind::Name && arrow_after(0))
        || (first.value == "(" && closing(init, 0).is_some_and(arrow_after))
    {
        return Some(false);
    }
    let mut depth = 0i32;
    let mut hook_depth: Option<i32> = None;
    let mut hook = false;
    for (i, t) in init.iter().enumerate() {
        let v = t.value.as_str();
        if hook_depth.is_none() {
            if t.kind == TokenKind::Name && matches!(v, "await" | "new" | "delete" | "yield") {
                return None;
            }
            if t.kind == TokenKind::Op && (matches!(v, "++" | "--") || is_assignment(v)) {
                return None;
            }
        }
        if t.kind != TokenKind::Op {
            continue;
        }
        match v {
            "(" => {
                if hook_depth.is_none() {
                    match callee(init, i) {
                        Callee::Grouping => {}
                        Callee::Name(n) if is_hook(n) => {
                            hook = true;
                            hook_depth = Some(depth);
                        }
                        _ => return None,
                    }
                }
                depth += 1;
            }
            "[" | "{" | "${" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if v == ")" && hook_depth == Some(depth) {
                    hook_depth = None;
                }
            }
            _ => {}
        }
    }
    Some(hook)
}

/// Python initializers count only when they call nothing at all.
fn py_pure(init: &[&Token]) -> Option<bool> {
    if init.is_empty() {
        return None;
    }
    for (i, t) in init.iter().enumerate() {
        let v = t.value.as_str();
        if t.kind == TokenKind::Name && matches!(v, "await" | "yield") {
            return None;
        }
        if t.kind == TokenKind::Op
            && (v == ":=" || (v == "(" && !matches!(callee(init, i), Callee::Grouping)))
        {
            return None;
        }
    }
    Some(false)
}

/// `useThing`, `use2D`: React's hook naming rule.
pub(crate) fn is_hook(name: &str) -> bool {
    name.strip_prefix("use")
        .and_then(|r| r.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A unit's old lines as an inclusive range.
pub(crate) fn old_range(u: &Unit) -> Option<(u32, u32)> {
    (!u.old.is_empty()).then(|| (u.old_start, u.old_start + u.old.len() as u32 - 1))
}

/// A unit's new lines as an inclusive range.
pub(crate) fn new_range(u: &Unit) -> Option<(u32, u32)> {
    (!u.new.is_empty()).then(|| (u.new_start, u.new_start + u.new.len() as u32 - 1))
}

/// Tokens on `rng` without comments and indentation markers; `<NEWLINE>` ends a statement.
pub(crate) fn code_tokens(an: &FileAnalysis, rng: Option<(u32, u32)>) -> Vec<&Token> {
    tokens_in_range(an, rng)
        .tokens()
        .iter()
        .filter(|t| match t.kind {
            TokenKind::Comment => false,
            TokenKind::Structural => t.value == NEWLINE,
            _ => true,
        })
        .collect()
}

/// Split at `<NEWLINE>`s outside brackets. `None` when the brackets don't balance, i.e. the
/// tokens are not a run of whole statements.
pub(crate) fn split_statements<'t>(toks: &[&'t Token]) -> Option<Vec<Vec<&'t Token>>> {
    let (stmts, whole) = leading_statements(toks);
    whole.then_some(stmts)
}

/// The whole statements at the start of `toks`, and whether that is all of them.
pub(crate) fn leading_statements<'t>(toks: &[&'t Token]) -> (Vec<Vec<&'t Token>>, bool) {
    let mut out: Vec<Vec<&Token>> = Vec::new();
    let mut cur: Vec<&Token> = Vec::new();
    let mut depth = 0i32;
    for t in toks {
        if t.kind == TokenKind::Structural {
            if depth == 0 {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                continue;
            }
            cur.push(t);
            continue;
        }
        if t.kind == TokenKind::Op {
            match t.value.as_str() {
                "(" | "[" | "{" | "${" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth < 0 {
                        return (out, false);
                    }
                }
                _ => {}
            }
        }
        cur.push(t);
    }
    if depth != 0 {
        return (out, false);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    (out, true)
}
