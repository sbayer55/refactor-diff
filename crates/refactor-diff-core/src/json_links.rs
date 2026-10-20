//! JSON-aware patterns: fixture fields dropped at the same path
//! ([`SignatureKind::Fixture`](crate::model::SignatureKind::Fixture)) and i18n keys paired with
//! the `t("key")` calls that used them ([`SignatureKind::I18n`](crate::model::SignatureKind::I18n)).
//!
//! Owned by workstream D (see docs/concept-lens-plan.md).
//!
//! * A JSON unit that only deletes whole `"key": value` members (of an object that survives)
//!   is re-signed one signature per deleted member: `fixture\0{path}` with indices as `[]`, so
//!   `"programId": "prog-1"` and `"programId": "prog-2"` dropped from two records group.
//! * In locale files (`**/i18n/**`, `**/locales/**`) the signature is `i18n\0{key path}`
//!   instead. A locale unit that also edits values of surviving keys keeps its signatures and
//!   gains the i18n ones; a unit that adds a key (a rename) is left alone.
//! * A JS/TS unit that only deletes `t("a.b")` / `i18n.t("a.b")` / `i18nKey="a.b"` uses of
//!   deleted keys (plus a little glue such as `label:` or `<span>`) is re-signed to the same
//!   `i18n\0{key path}` keys, so the key and its last uses group.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use indexmap::IndexMap;
use tree_sitter::Node;

use crate::classify::tokens_in_range;
use crate::lang::json::{Member, members};
use crate::lang::{FileAnalysis, Lang, TokenKind, analyzer_for, pos_of};
use crate::model::{FileSummary, Signature, SignatureKind, Unit, UnitTag};
use crate::moves::Analyses;
use crate::text::Pos;

/// Non-call tokens a deleted t() line may carry per call (`label: t("x"),`,
/// `<span>{t("x")}</span>`); more means the deletion took real code with it.
const GLUE_PER_CALL: usize = 12;

pub fn link(units: &mut IndexMap<String, Unit>, files: &[FileSummary], analyses: &Analyses) {
    let _ = files;
    let deleted_keys = link_json(units, analyses);
    if !deleted_keys.is_empty() {
        link_calls(units, analyses, &deleted_keys);
    }
}

/// Whether a path holds translations.
pub fn is_locale_path(path: &str) -> bool {
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    dirs.iter().any(|d| matches!(*d, "i18n" | "locales"))
}

fn line_range(start: u32, len: usize) -> Option<(u32, u32)> {
    (len > 0).then(|| (start, start + len as u32 - 1))
}

fn inside(start: Pos, end: Pos, lo: Pos, hi: Pos) -> bool {
    lo <= start && end <= hi
}

/// Re-sign JSON units; returns the i18n key paths given a signature.
fn link_json(units: &mut IndexMap<String, Unit>, analyses: &Analyses) -> BTreeSet<String> {
    let mut deleted_keys = BTreeSet::new();
    let paths: Vec<String> = analyses
        .keys()
        .filter(|p| analyzer_for(p).is_some_and(|a| a.name() == "json"))
        .cloned()
        .collect();
    for path in paths {
        let (old_an, new_an) = &analyses[&path];
        let Some(old_syntax) = &old_an.syntax else {
            continue;
        };
        let new_members = match &new_an.syntax {
            Some(s) => members(s, &new_an.lines),
            None if new_an.lines.iter().all(|l| l.trim().is_empty()) => Vec::new(),
            None => continue, // the new side doesn't parse: paths can't be compared
        };
        let old_members = members(old_syntax, &old_an.lines);
        let old_paths: HashSet<&str> = old_members.iter().map(|m| m.path.as_str()).collect();
        let new_paths: HashSet<&str> = new_members.iter().map(|m| m.path.as_str()).collect();
        let locale = is_locale_path(&path);

        for u in units.values_mut().filter(|u| u.path == path) {
            if u.partner.is_some() {
                continue;
            }
            let Some((first, last)) = line_range(u.old_start, u.old.len()) else {
                continue;
            };
            let new_rng = line_range(u.new_start, u.new.len());
            let mut dropped: Vec<&Member> = Vec::new();
            for m in &old_members {
                let deleted = m.key.is_some()
                    && first <= m.first_line
                    && m.last_line <= last
                    && !new_paths.contains(m.path.as_str())
                    && new_paths.contains(m.parent.as_str());
                if deleted
                    && !dropped
                        .iter()
                        .any(|d| inside(m.start, m.end, d.start, d.end))
                {
                    dropped.push(m);
                }
            }
            if dropped.is_empty() {
                continue;
            }
            let pure = only_drops(old_an, new_an, (first, last), new_rng, &dropped);
            let adds_keys = new_rng.is_some_and(|(nf, nl)| {
                new_members.iter().any(|m| {
                    m.key.is_some()
                        && nf <= m.first_line
                        && m.last_line <= nl
                        && !old_paths.contains(m.path.as_str())
                })
            });
            if !pure && (!locale || adds_keys) {
                continue;
            }
            let sigs: Vec<Signature> = dropped
                .iter()
                .map(|m| {
                    if locale {
                        deleted_keys.insert(m.norm.clone());
                        i18n_signature(&m.norm, "key")
                    } else {
                        Signature::new(
                            SignatureKind::Fixture,
                            format!("fixture\0{}", m.norm),
                            m.key.clone().unwrap_or_default(),
                            "",
                        )
                        .with_detail(m.norm.clone())
                    }
                })
                .collect();
            if pure {
                u.signatures.clear();
            }
            for sig in sigs {
                if u.signatures.iter().all(|s| s.key != sig.key) {
                    u.signatures.push(sig);
                }
            }
        }
    }
    deleted_keys
}

fn i18n_signature(key: &str, detail: &str) -> Signature {
    Signature::new(SignatureKind::I18n, format!("i18n\0{key}"), key, "").with_detail(detail)
}

/// Whether the unit's change is exactly the deletion of `dropped`: the old tokens outside
/// them equal the new tokens.
fn only_drops(
    old_an: &FileAnalysis,
    new_an: &FileAnalysis,
    old_rng: (u32, u32),
    new_rng: Option<(u32, u32)>,
    dropped: &[&Member],
) -> bool {
    let kept = tokens_in_range(old_an, Some(old_rng))
        .tokens()
        .iter()
        .filter(|t| {
            !dropped
                .iter()
                .any(|m| inside(t.start, t.end, m.start, m.end))
        })
        .map(|t| t.value.as_str());
    let new = tokens_in_range(new_an, new_rng)
        .tokens()
        .iter()
        .map(|t| t.value.as_str());
    kept.eq(new)
}

/// A translation lookup in code: its key and the span of the call (or attribute).
struct KeyUse {
    key: String,
    start: Pos,
    end: Pos,
}

/// Re-sign JS/TS units that only delete uses of deleted i18n keys.
fn link_calls(
    units: &mut IndexMap<String, Unit>,
    analyses: &Analyses,
    deleted_keys: &BTreeSet<String>,
) {
    let mut trees: BTreeMap<String, Option<tree_sitter::Tree>> = BTreeMap::new();
    for u in units.values_mut() {
        if u.partner.is_some() || !u.new.is_empty() || u.old.is_empty() {
            continue;
        }
        // A removed guard changes what runs even when its message keys go too: keep it in review.
        if u.has_tag(UnitTag::PreconditionRemoved)
            || u.signature_kinds().any(|k| k.never_mechanical())
        {
            continue;
        }
        let Some(lang) = Lang::for_path(&u.path).filter(|l| !l.is_python()) else {
            continue;
        };
        if !u
            .old
            .iter()
            .any(|l| l.text.contains("t(") || l.text.contains("i18nKey"))
        {
            continue;
        }
        let Some((old_an, _)) = analyses.get(&u.path) else {
            continue;
        };
        let source = old_an.lines.join("\n");
        let tree = trees
            .entry(u.path.clone())
            .or_insert_with(|| crate::lang::parse(&lang.grammar(), &source));
        let Some(tree) = tree else {
            continue;
        };
        let (first, last) = (u.old_start, u.old_start + u.old.len() as u32 - 1);
        let mut uses = Vec::new();
        key_uses(
            tree.root_node(),
            &source,
            &old_an.lines,
            first,
            last,
            &mut uses,
        );
        if uses.is_empty() {
            continue;
        }
        let mut keys: Vec<&str> = Vec::new();
        for k in &uses {
            let Some(key) = deleted_key(&k.key, deleted_keys) else {
                keys.clear();
                break;
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        if keys.is_empty() {
            continue;
        }
        let glue = tokens_in_range(old_an, Some((first, last)))
            .tokens()
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Structural | TokenKind::Comment))
            .filter(|t| !uses.iter().any(|k| inside(t.start, t.end, k.start, k.end)))
            .count();
        if glue > GLUE_PER_CALL * uses.len() {
            continue;
        }
        u.signatures = keys
            .into_iter()
            .map(|k| i18n_signature(k, "t() call"))
            .collect();
    }
}

/// The deleted key path a lookup resolves to: the key itself or the deleted subtree holding
/// it. A `namespace:` prefix is ignored.
fn deleted_key<'a>(key: &str, deleted: &'a BTreeSet<String>) -> Option<&'a str> {
    let key = key.split_once(':').map_or(key, |(_, k)| k);
    let mut probe = key;
    loop {
        if let Some(k) = deleted.get(probe) {
            return Some(k.as_str());
        }
        probe = &probe[..probe.rfind('.')?];
    }
}

/// `t("…")`, `x.t("…")` and `i18nKey="…"` lying wholly on lines `first..=last`.
fn key_uses(
    node: Node<'_>,
    source: &str,
    lines: &[String],
    first: u32,
    last: u32,
    out: &mut Vec<KeyUse>,
) {
    let (row_lo, row_hi) = (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    );
    if row_hi < first || row_lo > last {
        return;
    }
    if first <= row_lo && row_hi <= last {
        if let Some(key) = lookup_key(node, source) {
            out.push(KeyUse {
                key,
                start: pos_of(lines, node.start_position()),
                end: pos_of(lines, node.end_position()),
            });
            return;
        }
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    for child in children {
        key_uses(child, source, lines, first, last, out);
    }
}

fn text<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    source.get(node.start_byte()..node.end_byte()).unwrap_or("")
}

/// The key a translation lookup node passes, if it is one.
fn lookup_key(node: Node<'_>, source: &str) -> Option<String> {
    match node.kind() {
        "call_expression" => {
            let callee = node.child_by_field_name("function")?;
            let name = match callee.kind() {
                "identifier" => text(source, callee),
                "member_expression" => text(source, callee.child_by_field_name("property")?),
                _ => return None,
            };
            if name != "t" {
                return None;
            }
            let args = node.child_by_field_name("arguments")?;
            let mut cursor = args.walk();
            let arg = args
                .named_children(&mut cursor)
                .find(|c| c.kind() != "comment")?;
            string_literal(arg, source)
        }
        "jsx_attribute" => {
            let mut cursor = node.walk();
            let parts: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            let (name, value) = (parts.first()?, parts.get(1)?);
            if text(source, *name) != "i18nKey" {
                return None;
            }
            string_literal(*value, source)
        }
        _ => None,
    }
}

/// The body of a string literal or a template literal without substitutions.
fn string_literal(node: Node<'_>, source: &str) -> Option<String> {
    let plain = match node.kind() {
        "string" => true,
        "template_string" => {
            let mut cursor = node.walk();
            let subst = node
                .children(&mut cursor)
                .any(|c| c.kind() == "template_substitution");
            !subst
        }
        _ => false,
    };
    if !plain {
        return None;
    }
    let t = text(source, node);
    let mut chars = t.chars();
    chars.next()?;
    chars.next_back()?;
    Some(chars.as_str().to_string())
}
