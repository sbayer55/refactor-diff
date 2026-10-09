//! Prove that a mechanical-looking change really is one.
//!
//! For every statement (function, class method, top-level statement) that contains changes
//! classified only as formatting, docs, rename or retype, the old and new versions are parsed
//! and compared after normalization: docstrings dropped, the diff's mechanical renames applied
//! to the old side, annotations dropped when the span contains a retype. Equal trees mean the
//! statement is the same program, and every unit inside it is marked `verified`.
//!
//! Verification is a property of the whole statement: a function that has one rename and one
//! logic change verifies neither. It never changes whether a unit is *explained*; it only
//! tells the reviewer which collapsed changes are safe beyond doubt.

use std::collections::{BTreeMap, HashMap, HashSet};

use indexmap::IndexMap;

use crate::lang::{FileAnalysis, StmtSpan, analyzer_for};
use crate::model::{Group, SignatureKind, Unit};

const VERIFIABLE: &[SignatureKind] = &[
    SignatureKind::Formatting,
    SignatureKind::Docs,
    SignatureKind::Rename,
    SignatureKind::Retype,
];
const TRIVIAL: &[SignatureKind] = &[SignatureKind::Formatting, SignatureKind::Docs];

fn subset(kinds: &HashSet<SignatureKind>, of: &[SignatureKind]) -> bool {
    kinds.iter().all(|k| of.contains(k))
}

pub fn verify_units(
    units: &mut IndexMap<String, Unit>,
    analyses: &BTreeMap<String, (FileAnalysis, FileAnalysis)>,
    groups: &[Group],
) {
    let mechanical: HashSet<&str> = groups
        .iter()
        .filter(|g| g.mechanical)
        .map(|g| g.key.as_str())
        .collect();
    let mut by_path: IndexMap<&str, Vec<String>> = IndexMap::new();
    for u in units.values() {
        by_path
            .entry(u.path.as_str())
            .or_default()
            .push(u.id.clone());
    }
    let by_path: Vec<(String, Vec<String>)> = by_path
        .into_iter()
        .map(|(p, ids)| (p.to_string(), ids))
        .collect();

    for (path, mine) in by_path {
        let Some(analyzer) = analyzer_for(&path) else {
            continue;
        };
        let Some(verifier) = analyzer.verifier() else {
            continue;
        };
        let Some((old_an, new_an)) = analyses.get(&path) else {
            continue;
        };
        let (Some(old_syntax), Some(new_syntax)) = (&old_an.syntax, &new_an.syntax) else {
            continue;
        };
        // Group the file's units by the old-side statement they sit in.
        let mut by_span: SpanGroups<'_> = IndexMap::new();
        for id in &mine {
            let u = &units[id];
            if u.partner.is_some() {
                continue; // moves are verified against their partner below
            }
            let rng =
                (!u.old.is_empty()).then(|| (u.old_start, u.old_start + u.old.len() as u32 - 1));
            let span = span_for(&old_an.statements, rng, u.old_start);
            by_span
                .entry(span.map(|s| (s.start, s.end)))
                .or_insert_with(|| (span, Vec::new()))
                .1
                .push(id.clone());
        }
        for (span, span_units) in by_span.values() {
            let kinds: HashSet<SignatureKind> = span_units
                .iter()
                .flat_map(|id| units[id].signature_kinds())
                .collect();
            if kinds.is_empty() || !subset(&kinds, VERIFIABLE) {
                continue;
            }
            let Some(span) = span else {
                // Blank or comment lines between statements.
                let ok = subset(&kinds, TRIVIAL);
                for id in span_units {
                    units[id].verified = ok;
                }
                continue;
            };
            let span_refs: Vec<&Unit> = span_units.iter().map(|id| &units[id]).collect();
            let renames = rename_map(&span_refs, &mechanical);
            let new_rng = new_range(&span_refs);
            let Some(other) = counterpart(span, &new_an.statements, &renames, new_rng) else {
                continue;
            };
            let (Some(node), Some(other_node)) = (span.node, other.node) else {
                continue;
            };
            let strip = kinds.contains(&SignatureKind::Retype);
            let ok = verifier.normalized_dump(old_syntax, Some(node), &renames, strip)
                == verifier.normalized_dump(new_syntax, Some(other_node), &HashMap::new(), strip);
            for id in span_units {
                units[id].verified = ok;
            }
        }
    }

    verify_moves(units, &mechanical);
}

/// Units grouped by the old-side statement (keyed by its line span) they sit in.
type SpanGroups<'a> = IndexMap<Option<(u32, u32)>, (Option<&'a StmtSpan>, Vec<String>)>;

fn verify_moves(units: &mut IndexMap<String, Unit>, mechanical: &HashSet<&str>) {
    let ids: Vec<String> = units.keys().cloned().collect();
    for id in ids {
        let u = &units[&id];
        let Some(partner) = u.partner.clone() else {
            continue;
        };
        if u.new.is_empty() {
            continue; // handle each move once, from its new side
        }
        let Some(d) = units.get(&partner) else {
            continue;
        };
        let kinds: HashSet<SignatureKind> = u
            .signature_kinds()
            .filter(|k| *k != SignatureKind::Move)
            .collect();
        if !subset(&kinds, VERIFIABLE) {
            continue;
        }
        let Some(verifier) = analyzer_for(&u.path).and_then(|a| a.verifier()) else {
            continue;
        };
        let old_text: Vec<&str> = d.old.iter().map(|l| l.text.as_str()).collect();
        let new_text: Vec<&str> = u.new.iter().map(|l| l.text.as_str()).collect();
        let (Some(old_tree), Some(new_tree)) = (
            verifier.parse_block(&old_text.join("\n")),
            verifier.parse_block(&new_text.join("\n")),
        ) else {
            continue;
        };
        let renames = rename_map(&[u], mechanical);
        let strip = kinds.contains(&SignatureKind::Retype);
        let ok = verifier.normalized_dump(&old_tree, None, &renames, strip)
            == verifier.normalized_dump(&new_tree, None, &HashMap::new(), strip);
        units[&id].verified = ok;
        units[&partner].verified = ok;
    }
}

/// The smallest statement containing the lines (a class when they cross its methods).
pub fn span_for(
    statements: &[StmtSpan],
    rng: Option<(u32, u32)>,
    anchor: u32,
) -> Option<&StmtSpan> {
    let (first, last) = rng.unwrap_or((anchor, anchor));
    for s in statements {
        if s.start <= first && last <= s.end {
            for c in &s.children {
                if c.start <= first && last <= c.end {
                    return Some(c);
                }
            }
            return Some(s);
        }
    }
    None
}

/// The new-side statement matching `span`: by (renamed) name for defs and classes,
/// otherwise the statement at the units' new position.
fn counterpart<'a>(
    span: &StmtSpan,
    statements: &'a [StmtSpan],
    renames: &HashMap<String, String>,
    new_rng: Option<(u32, u32)>,
) -> Option<&'a StmtSpan> {
    if !span.qualname.is_empty() {
        let wanted: Vec<&str> = span
            .qualname
            .split('.')
            .map(|p| renames.get(p).map_or(p, String::as_str))
            .collect();
        let wanted = wanted.join(".");
        return statements
            .iter()
            .flat_map(|s| s.flatten())
            .find(|s| s.qualname == wanted && s.kind == span.kind);
    }
    let rng = new_rng?;
    span_for(statements, Some(rng), rng.0)
}

fn new_range(span_units: &[&Unit]) -> Option<(u32, u32)> {
    let with_new: Vec<&&Unit> = span_units.iter().filter(|u| !u.new.is_empty()).collect();
    if with_new.is_empty() {
        let anchor = span_units.iter().map(|u| u.new_start).min()?;
        return Some((anchor, anchor));
    }
    let start = with_new.iter().map(|u| u.new_start).min()?;
    let end = with_new
        .iter()
        .map(|u| u.new_start + u.new.len() as u32 - 1)
        .max()?;
    Some((start, end))
}

/// Old name → new name for the mechanical renames in these units, where unambiguous.
pub fn rename_map(span_units: &[&Unit], mechanical: &HashSet<&str>) -> HashMap<String, String> {
    let mut targets: IndexMap<&str, IndexMap<&str, ()>> = IndexMap::new();
    for u in span_units {
        for s in &u.signatures {
            if s.kind == SignatureKind::Rename && mechanical.contains(s.key.as_str()) {
                targets.entry(&s.old).or_default().insert(&s.new, ());
            }
        }
    }
    targets
        .into_iter()
        .filter(|(_, news)| news.len() == 1)
        .map(|(old, news)| (old.to_string(), news.keys().next().unwrap().to_string()))
        .collect()
}
