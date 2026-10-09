//! Grouping nearby ops into one edit, and the `replace` signatures (plain or templated) a
//! cluster becomes.

use super::{MAX_WRAP_GAP, PLACEHOLDER, Side, render};
use crate::lang::{Token, TokenKind};
use crate::model::{Signature, SignatureKind};
use crate::seqmatch::Opcode;
use crate::text::Pos;

/// Value of a template hole token; `PLACEHOLDER` is its display text.
const HOLE_VALUE: &str = "\x02";

/// Unchanged tokens that may separate two ops of one edit (`cfg.get("x")` -> `settings.x`).
const JOIN_GAPS: &[&str] = &[".", "="];

fn bracket(value: &str) -> i32 {
    match value {
        "(" | "[" | "{" => 1,
        ")" | "]" | "}" => -1,
        _ => 0,
    }
}

/// Net bracket depth opened by the operator tokens of `tokens`.
fn depth(tokens: &[Token]) -> i32 {
    tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Op)
        .map(|t| bracket(&t.value))
        .sum()
}

/// Whether an op may join a cluster: unclassified, or a rename.
fn joinable(sig: Option<&Signature>) -> bool {
    sig.is_none_or(|s| s.kind == SignatureKind::Rename)
}

/// Group nearby ops that form one edit, so it gets one signature instead of fragments:
///
/// * wraps - an op leaves a bracket open and a later op closes it around unchanged code:
///   `actor` -> `str(actor.user_id)`, `role="x"` -> `roles=("x",)`
/// * ops separated by a single `.` or `=`: `cfg.get("x")` -> `settings.x`
///
/// Retypes never join a cluster; a cluster made only of renames is reported as renames.
pub(super) fn clusters(
    a: &Side<'_>,
    b: &Side<'_>,
    ops: &[Opcode],
    classified: &[Option<Signature>],
) -> Vec<Vec<usize>> {
    let (a_toks, b_toks) = (a.tokens(), b.tokens());
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let (mut open_old, mut open_new) = (0i32, 0i32);
    for (k, op) in ops.iter().enumerate() {
        if let Some(current) = clusters.last_mut() {
            let last_k = *current.last().expect("clusters are never empty");
            let last = &ops[last_k];
            // The gap is measured on the old side for both depth counters, as in Python.
            let gap = &a_toks[last.i2..op.i1];
            let joinable = joinable(classified[k].as_ref())
                && joinable(classified[last_k].as_ref())
                && !gap.iter().any(|t| t.kind == TokenKind::Structural);
            let wrap = (open_old > 0 || open_new > 0) && gap.len() <= MAX_WRAP_GAP;
            let short = gap.len() == 1 && JOIN_GAPS.contains(&gap[0].value.as_str());
            if joinable && (wrap || short) {
                current.push(k);
                open_old += depth(gap) + depth(&a_toks[op.i1..op.i2]);
                open_new += depth(gap) + depth(&b_toks[op.j1..op.j2]);
                continue;
            }
        }
        clusters.push(vec![k]);
        open_old = depth(&a_toks[op.i1..op.i2]);
        open_new = depth(&b_toks[op.j1..op.j2]);
    }
    clusters
}

/// `"replace"` joined with the old values, `"\x01"`, and the new values.
fn replace_key<'t>(
    old: impl IntoIterator<Item = &'t Token>,
    new: impl IntoIterator<Item = &'t Token>,
) -> String {
    let mut parts: Vec<&str> = vec!["replace"];
    parts.extend(old.into_iter().map(|t| t.value.as_str()));
    parts.push("\x01");
    parts.extend(new.into_iter().map(|t| t.value.as_str()));
    parts.join("\0")
}

/// The plain replace signature of one op.
pub(super) fn replace_sig(a: &Side<'_>, b: &Side<'_>, op: &Opcode) -> Signature {
    let old_toks = &a.tokens()[op.i1..op.i2];
    let new_toks = &b.tokens()[op.j1..op.j2];
    Signature::new(
        SignatureKind::Replace,
        replace_key(old_toks, new_toks),
        render(old_toks, &a.analysis.lines),
        render(new_toks, &b.analysis.lines),
    )
}

/// One signature for a cluster of ops. Unchanged code between the ops becomes a "…"
/// placeholder when it holds names or literals, so `role="admin"` and `role="faculty"` share
/// the template `role=… -> roles=(…,)`.
pub(super) fn template_sig(a: &Side<'_>, b: &Side<'_>, cluster_ops: &[Opcode]) -> Signature {
    let (a_toks, b_toks) = (a.tokens(), b.tokens());
    let mut old: Vec<Token> = Vec::new();
    let mut new: Vec<Token> = Vec::new();
    for (n, op) in cluster_ops.iter().enumerate() {
        if n > 0 {
            let prev = &cluster_ops[n - 1];
            let gap_a = &a_toks[prev.i2..op.i1];
            let gap_b = &b_toks[prev.j2..op.j1];
            if gap_a.iter().any(|t| is_hole_kind(t.kind)) {
                old.push(hole(gap_a, prev_end(&old, a_toks, prev.i2)));
                new.push(hole(gap_b, prev_end(&new, b_toks, prev.j2)));
            } else {
                old.extend_from_slice(gap_a);
                new.extend_from_slice(gap_b);
            }
        }
        old.extend_from_slice(&a_toks[op.i1..op.i2]);
        new.extend_from_slice(&b_toks[op.j1..op.j2]);
    }
    Signature::new(
        SignatureKind::Replace,
        replace_key(&old, &new),
        render(&old, &a.analysis.lines),
        render(&new, &b.analysis.lines),
    )
}

fn is_hole_kind(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Name | TokenKind::String | TokenKind::Number
    )
}

/// Where a hole for an empty gap would sit: after the last token collected so far. (The gaps
/// between consecutive ops are equal blocks and so the same length on both sides, so this
/// only guards against a malformed op list; Python would raise.)
fn prev_end(collected: &[Token], side: &[Token], index: usize) -> Pos {
    collected
        .last()
        .map(|t| t.end)
        .or_else(|| side.get(index).map(|t| t.start))
        .unwrap_or(Pos::new(1, 0))
}

/// A placeholder token spanning `tokens`.
fn hole(tokens: &[Token], fallback: Pos) -> Token {
    let (start, end) = match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => (first.start, last.end),
        _ => (fallback, fallback),
    };
    Token::new(TokenKind::Other, HOLE_VALUE, PLACEHOLDER, start, end)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::seqmatch::Tag;

    fn sides<'a>(
        a: &'a crate::lang::FileAnalysis,
        b: &'a crate::lang::FileAnalysis,
    ) -> (Side<'a>, Side<'a>) {
        (
            super::super::tokens_in_range(a, whole(a)),
            super::super::tokens_in_range(b, whole(b)),
        )
    }

    #[test]
    fn depth_counts_brackets_in_op_tokens_only() {
        let toks = tokenize("f((x, '(') + [1]");
        assert_eq!(depth(&toks), 2 - 1 + 1 - 1);
        assert_eq!(depth(&toks[..3]), 2);
        assert_eq!(depth(&[]), 0);
    }

    #[test]
    fn short_gap_joins_and_structural_gap_splits() {
        let rename = Signature::new(SignatureKind::Rename, "rename\0a\0b", "a", "b");
        let (a, b) = (analysis("a.b\nc\n"), analysis("x.y\nz\n"));
        let (sa, sb) = sides(&a, &b);
        // a->x, b->y (gap "."), c->z (gap: "<NEWLINE>").
        let ops = [
            Opcode::new(Tag::Replace, 0, 1, 0, 1),
            Opcode::new(Tag::Replace, 2, 3, 2, 3),
            Opcode::new(Tag::Replace, 4, 5, 4, 5),
        ];
        let classified = [Some(rename.clone()), None, None];
        assert_eq!(
            clusters(&sa, &sb, &ops, &classified),
            vec![vec![0, 1], vec![2]]
        );
        // A retype never joins.
        let retype = Signature::new(SignatureKind::Retype, "retype\0a\0b", "a", "b");
        let classified = [Some(retype), None, None];
        assert_eq!(
            clusters(&sa, &sb, &ops, &classified),
            vec![vec![0], vec![1], vec![2]]
        );
    }

    #[test]
    fn wrap_joins_while_a_bracket_is_open_within_the_gap_limit() {
        let (a, b) = (analysis("x = v + w"), analysis("x = f(v) + w"));
        let (sa, sb) = sides(&a, &b);
        // insert "f(" before v, insert ")" after v
        let ops = [
            Opcode::new(Tag::Insert, 2, 2, 2, 4),
            Opcode::new(Tag::Insert, 3, 3, 5, 6),
        ];
        assert_eq!(clusters(&sa, &sb, &ops, &[None, None]), vec![vec![0, 1]]);
        let sig = template_sig(&sa, &sb, &ops);
        assert_eq!((sig.old.as_str(), sig.new.as_str()), ("…", "f(…)"));
        assert_eq!(sig.key, "replace\0\x02\0\x01\0f\0(\0\x02\0)");

        // The same wrap around more than MAX_WRAP_GAP unchanged tokens stays split.
        let long_old = format!("x = {}", vec!["v"; 14].join(" + "));
        let long_new = format!("x = f({})", vec!["v"; 14].join(" + "));
        let (a, b) = (analysis(&long_old), analysis(&long_new));
        let (sa, sb) = sides(&a, &b);
        let gap = 27; // 14 names and 13 pluses
        let ops = [
            Opcode::new(Tag::Insert, 2, 2, 2, 4),
            Opcode::new(Tag::Insert, 2 + gap, 2 + gap, 4 + gap, 5 + gap),
        ];
        assert_eq!(
            clusters(&sa, &sb, &ops, &[None, None]),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn replace_sig_renders_and_keys_on_values() {
        let (a, b) = (analysis("x = 'a'"), analysis("x = \"b\""));
        let (sa, sb) = sides(&a, &b);
        let sig = replace_sig(&sa, &sb, &Opcode::new(Tag::Replace, 2, 3, 2, 3));
        assert_eq!(sig.key, "replace\0'a'\0\x01\0'b'");
        assert_eq!((sig.old.as_str(), sig.new.as_str()), ("'a'", "\"b\""));
        assert_eq!(sig.detail, "");
    }

    #[test]
    fn template_keeps_operator_gaps_and_holes_literals() {
        let (a, b) = (analysis("k = cfg.get(1)"), analysis("k = settings.one"));
        let (sa, sb) = sides(&a, &b);
        // cfg -> settings, [get ( 1 )] -> [one]; the gap is the operator "." (kept verbatim)
        let ops = [
            Opcode::new(Tag::Replace, 2, 3, 2, 3),
            Opcode::new(Tag::Replace, 4, 8, 4, 5),
        ];
        let sig = template_sig(&sa, &sb, &ops);
        assert_eq!(sig.old, "cfg.get(1)");
        assert_eq!(sig.new, "settings.one");
        assert_eq!(
            sig.key,
            "replace\0cfg\0.\0get\0(\x001\0)\0\x01\0settings\0.\0one"
        );
    }
}
