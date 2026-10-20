//! Whole test blocks (`it`/`test`/`describe`) deleted or added: tagged
//! [`UnitTag::TestRemoved`](crate::model::UnitTag::TestRemoved) and signed
//! [`SignatureKind::Test`](crate::model::SignatureKind::Test).
//!
//! Only deletions are handled: an added test is new code to review. In a test file, a unit
//! whose removed lines contain whole test-call statements (`it(...)`, `describe.each(...)(...)`,
//! `test.skip(...)`, ...) gets the tag. When those calls are all the unit removes, its
//! signatures become one `test` signature per block, keyed by the normalized title so the same
//! test deleted in several places groups (and, being `by count`, collapses only when it
//! repeats). Each signature's `detail` lists the imports the file dropped that the block used,
//! as `imports:a,b`.
//!
//! The line diff often slides a deleted block by a line or two (`});` and a blank line are
//! interchangeable at either end), so a unit that doesn't split into whole statements is
//! retried shifted up or down while the shifted lines read the same.
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::cascade::{
    closing, code_tokens, leading_statements, new_range, old_range, split_statements,
};
use crate::categories::{Category, categorize};
use crate::lang::{FileAnalysis, Token, TokenKind};
use crate::model::{Signature, SignatureKind, Unit, UnitTag};
use crate::moves::Analyses;

/// How far a deleted block may have slid.
const MAX_SLIDE: u32 = 6;

const TEST_FUNCTIONS: &[&str] = &[
    "it",
    "test",
    "describe",
    "suite",
    "xit",
    "fit",
    "xtest",
    "xdescribe",
    "fdescribe",
];

/// Tag and re-sign deleted test blocks.
pub fn tag(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    for u in units.values_mut() {
        // A block that moved elsewhere was not removed.
        if !is_test_file(&u.path)
            || u.partner.is_some()
            || u.signature_kinds().any(|k| k == SignatureKind::Move)
        {
            continue;
        }
        let Some((old_an, new_an)) = analyses.get(&u.path) else {
            continue;
        };
        let Some(rng) = old_range(u) else {
            continue;
        };
        // When the lines don't split into whole statements, the ones they start with still
        // count for the tag (a removed test followed by an edited one).
        let (stmts, whole) = match whole_statements(old_an, rng) {
            Some(s) => (s, true),
            None => (leading_statements(&code_tokens(old_an, Some(rng))).0, false),
        };
        let mut blocks: Vec<(String, &[&Token])> = Vec::new();
        let mut other = !whole;
        for s in &stmts {
            match test_title(s) {
                Some(title) => blocks.push((title, s.as_slice())),
                None => other = true,
            }
        }
        // A test whose title is still there afterwards was edited, not removed; one with a
        // new title may be the same test renamed, so leave those alone.
        let added: Vec<String> = new_range(u)
            .map(|r| leading_statements(&code_tokens(new_an, Some(r))).0)
            .unwrap_or_default()
            .iter()
            .filter_map(|s| test_title(s))
            .collect();
        blocks.retain(|(title, _)| !added.contains(title));
        if blocks.is_empty() || added.iter().any(|t| blocks.iter().all(|(b, _)| b != t)) {
            continue;
        }
        u.add_tag(UnitTag::TestRemoved);
        if other || new_range(u).is_some() {
            continue;
        }
        let dropped = dropped_imports(old_an, new_an);
        let mut sigs: Vec<Signature> = Vec::new();
        for (title, toks) in &blocks {
            let key = format!("test\0-{}", normalize(title));
            if sigs.iter().any(|s| s.key == key) {
                continue;
            }
            let mut used: Vec<&str> = Vec::new();
            for t in toks.iter().filter(|t| t.kind == TokenKind::Name) {
                if dropped.contains(t.value.as_str()) && !used.contains(&t.value.as_str()) {
                    used.push(&t.value);
                }
            }
            let detail = if used.is_empty() {
                String::new()
            } else {
                format!("imports:{}", used.join(","))
            };
            sigs.push(
                Signature::new(
                    SignatureKind::Test,
                    key,
                    format!("removed test: \"{title}\""),
                    "",
                )
                .with_detail(detail),
            );
        }
        u.signatures = sigs;
    }
}

/// Test files by category, plus `*.test.*`/`*.spec.*` anywhere.
pub(crate) fn is_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.contains(".test.") || name.contains(".spec.") || categorize(path, true) == Category::Tests
}

/// The statements on `rng`, or on the same text slid by a few lines when the diff cut the
/// block at an equivalent but unbalanced place.
fn whole_statements(an: &FileAnalysis, (a, b): (u32, u32)) -> Option<Vec<Vec<&Token>>> {
    let try_range =
        |r: (u32, u32)| split_statements(&code_tokens(an, Some(r))).filter(|s| !s.is_empty());
    if let Some(s) = try_range((a, b)) {
        return Some(s);
    }
    let line = |n: u32| an.lines.get(n as usize - 1);
    let total = an.lines.len() as u32;
    for k in 1..=MAX_SLIDE {
        // Up: the k lines before the block equal its last k lines.
        if a > k
            && (0..k).all(|i| line(a - k + i).is_some() && line(a - k + i) == line(b - k + 1 + i))
        {
            if let Some(s) = try_range((a - k, b - k)) {
                return Some(s);
            }
        }
        // Down: the k lines after the block equal its first k lines.
        if b + k <= total && (0..k).all(|i| line(a + i) == line(b + 1 + i)) {
            if let Some(s) = try_range((a + k, b + k)) {
                return Some(s);
            }
        }
    }
    None
}

/// The title of a whole `it(...)`-style call statement: the callee (with modifiers such as
/// `.each(...)`, `.skip`, `.skipIf(...)`) followed by one final argument list.
fn test_title(s: &[&Token]) -> Option<String> {
    let first = s.first()?;
    if first.kind != TokenKind::Name || !TEST_FUNCTIONS.contains(&first.value.as_str()) {
        return None;
    }
    let mut i = 1;
    // Modifiers: `.name`, `(...)` and tagged templates, up to the last argument list.
    let mut last_call = None;
    while i < s.len() {
        let t = s[i];
        match (t.kind, t.value.as_str()) {
            (TokenKind::Op, ".") => {
                if s.get(i + 1)?.kind != TokenKind::Name {
                    return None;
                }
                i += 2;
            }
            (TokenKind::Op, "(") => {
                let end = closing(s, i)?;
                last_call = Some(i);
                i = end + 1;
            }
            (TokenKind::String, _) => i += 1, // `.each` table
            _ => return None,
        }
    }
    let open = last_call?;
    if closing(s, open)? != s.len() - 1 {
        return None;
    }
    let arg = s.get(open + 1)?;
    let title = if arg.kind == TokenKind::String {
        unquote(&arg.text)
    } else {
        arg.text.clone()
    };
    Some(title)
}

fn unquote(text: &str) -> String {
    let t = text.trim();
    let mut chars = t.chars();
    match (chars.next(), chars.next_back()) {
        (Some(a), Some(b)) if a == b && matches!(a, '"' | '\'' | '`') => chars.as_str().into(),
        _ => t.to_string(),
    }
}

/// Lowercase, single-spaced.
fn normalize(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Names the file imported in the base and no longer imports.
fn dropped_imports<'a>(old_an: &'a FileAnalysis, new_an: &FileAnalysis) -> HashSet<&'a str> {
    let kept: HashSet<&str> = new_an
        .imports
        .iter()
        .flat_map(|s| s.bindings.iter().map(|b| b.alias.as_str()))
        .collect();
    old_an
        .imports
        .iter()
        .flat_map(|s| s.bindings.iter().map(|b| b.alias.as_str()))
        .filter(|a| !a.is_empty() && !kept.contains(a))
        .collect()
}
