//! Per-op classification: docs-only changes, retypes inside annotations and single-identifier
//! renames. Anything else stays unclassified for the args and cluster passes.

use super::Side;
use crate::lang::{Annotation, FileAnalysis, LanguageAnalyzer, Token, TokenKind};
use crate::model::{Signature, SignatureKind};
use crate::seqmatch::{Opcode, Tag};

/// Punctuation that introduces an annotation and is ignored when deciding whether every
/// changed token sits inside one.
const ANNOTATION_PUNCT: &[&str] = &[":", "->"];

/// The signature of a single op, or `None` when it needs the args/cluster passes.
pub(super) fn classify_op(
    analyzer: &dyn LanguageAnalyzer,
    a: &Side<'_>,
    b: &Side<'_>,
    op: &Opcode,
) -> Option<Signature> {
    let old_toks = &a.tokens()[op.i1..op.i2];
    let new_toks = &b.tokens()[op.j1..op.j2];
    if is_docs(old_toks, a.analysis) && is_docs(new_toks, b.analysis) {
        let visible = old_toks
            .iter()
            .chain(new_toks)
            .any(|t| t.kind != TokenKind::Structural);
        return Some(if visible {
            Signature::new(SignatureKind::Docs, "docs", "", "")
        } else {
            // Only indentation / logical-line structure changed (e.g. a block moved into a
            // class).
            Signature::new(SignatureKind::Formatting, "formatting", "", "")
        });
    }
    if let Some(retype) = retype_sig(a, b, op) {
        return Some(retype);
    }
    if op.tag != Tag::Replace {
        return None;
    }
    let ([old], [new]) = (old_toks, new_toks) else {
        return None;
    };
    if old.kind != TokenKind::Name
        || new.kind != TokenKind::Name
        || analyzer.is_keyword(&old.value)
        || analyzer.is_keyword(&new.value)
    {
        return None;
    }
    let detail = rename_context(analyzer, &a.analysis.tokens, a.offset + op.i1);
    let key = format!("rename\0{}\0{}", old.value, new.value);
    Some(Signature::new(SignatureKind::Rename, key, &old.value, &new.value).with_detail(detail))
}

/// Every token is a comment, a docstring, or layout.
fn is_docs(tokens: &[Token], analysis: &FileAnalysis) -> bool {
    tokens.iter().all(|t| {
        matches!(t.kind, TokenKind::Comment | TokenKind::Structural)
            || t.kind == TokenKind::String
                && analysis
                    .docstrings
                    .iter()
                    .any(|(s, e)| *s <= t.start && t.end <= *e)
    })
}

/// Where the renamed identifier at `tokens[index]` sits: `"definition"`, `"attribute"`,
/// `"import"`, `"call"`, `"keyword"` or `"name"`.
pub fn rename_context(
    analyzer: &dyn LanguageAnalyzer,
    tokens: &[Token],
    index: usize,
) -> &'static str {
    let prev = index
        .checked_sub(1)
        .map_or("", |i| tokens[i].value.as_str());
    let next = tokens.get(index + 1).map_or("", |t| t.value.as_str());
    if analyzer.definition_keywords().contains(&prev) {
        return "definition";
    }
    if prev == "." {
        return "attribute";
    }
    let mut k = index;
    while k > 0 && tokens[k - 1].kind != TokenKind::Structural {
        k -= 1;
    }
    if analyzer
        .import_keywords()
        .contains(&tokens[k].value.as_str())
    {
        return "import";
    }
    if next == "(" {
        return "call";
    }
    if next == "=" && (prev == "(" || prev == ",") {
        return "keyword";
    }
    "name"
}

/// A retype when the changed tokens of each side sit inside one type annotation.
fn retype_sig(a: &Side<'_>, b: &Side<'_>, op: &Opcode) -> Option<Signature> {
    let old_sig: Vec<&Token> = a.tokens()[op.i1..op.i2]
        .iter()
        .filter(|t| !ANNOTATION_PUNCT.contains(&t.value.as_str()))
        .collect();
    let new_sig: Vec<&Token> = b.tokens()[op.j1..op.j2]
        .iter()
        .filter(|t| !ANNOTATION_PUNCT.contains(&t.value.as_str()))
        .collect();
    if old_sig.is_empty() && new_sig.is_empty() {
        return None;
    }
    let old_ann = enclosing(&a.analysis.annotations, &old_sig, a.tokens(), op.i1, op.i2);
    let new_ann = enclosing(&b.analysis.annotations, &new_sig, b.tokens(), op.j1, op.j2);
    if (!old_sig.is_empty() && old_ann.is_none()) || (!new_sig.is_empty() && new_ann.is_none()) {
        return None;
    }
    let target = new_ann.or(old_ann)?.target.clone();
    let old_text = old_ann.map_or("(untyped)", |ann| ann.text.as_str());
    let new_text = new_ann.map_or("(untyped)", |ann| ann.text.as_str());
    let key = format!("retype\0{old_text}\0{new_text}");
    Some(Signature::new(SignatureKind::Retype, key, old_text, new_text).with_detail(target))
}

/// The annotation containing every changed token, or - for a pure insertion on this side -
/// the annotation around the insertion point.
fn enclosing<'a>(
    annotations: &'a [Annotation],
    changed: &[&Token],
    unit_tokens: &[Token],
    i1: usize,
    i2: usize,
) -> Option<&'a Annotation> {
    if !changed.is_empty() {
        return annotations
            .iter()
            .find(|ann| changed.iter().all(|t| ann.contains(t)));
    }
    let lo = i1.saturating_sub(1);
    let hi = (i2 + 1).min(unit_tokens.len());
    let neighbors = &unit_tokens[lo.min(hi)..hi];
    annotations
        .iter()
        .find(|ann| neighbors.iter().any(|t| ann.contains(t)))
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::text::Pos;

    fn ctx(src: &str, index: usize) -> &'static str {
        rename_context(&TestLang, &tokenize(src), index)
    }

    #[test]
    fn rename_context_by_neighbours() {
        assert_eq!(ctx("def f():", 1), "definition");
        assert_eq!(ctx("class C:", 1), "definition");
        assert_eq!(ctx("a.b", 2), "attribute");
        assert_eq!(ctx("from m import a", 3), "import");
        assert_eq!(ctx("x = 1\nfrom m import a", 7), "import");
        assert_eq!(ctx("x = 1\nf(a)", 4), "call");
        assert_eq!(ctx("f(a=1)", 2), "keyword");
        assert_eq!(ctx("f(x, a=1)", 4), "keyword");
        assert_eq!(ctx("x = a", 2), "name");
        assert_eq!(ctx("a", 0), "name");
        // The statement start is found by walking back to a structural token, so a name
        // before an `import` keyword in a later statement is not an import.
        assert_eq!(ctx("x = 1\nimport os", 0), "name");
    }

    #[test]
    fn docs_checks_docstring_spans() {
        let mut an = analysis("'doc'\nx = 'not doc'\n");
        an.docstrings = vec![(Pos::new(1, 0), Pos::new(1, 5))];
        assert!(is_docs(&an.tokens[..1], &an));
        assert!(
            is_docs(&an.tokens[..2], &an),
            "structural tokens count as docs"
        );
        assert!(!is_docs(&an.tokens[2..5], &an));
        assert!(is_docs(&[], &an));
    }

    #[test]
    fn retype_requires_every_changed_token_inside_one_annotation() {
        let ann = |s, e, text: &str| Annotation {
            start: Pos::new(1, s),
            end: Pos::new(1, e),
            text: text.into(),
            target: "param x".into(),
        };
        let mut a = analysis("def f(x: int, y):");
        let mut b = analysis("def f(x: str, z):");
        a.annotations = vec![ann(9, 12, "int")];
        b.annotations = vec![ann(9, 12, "str")];
        let sa = tokens_in_range(&a, Some((1, 1)));
        let sb = tokens_in_range(&b, Some((1, 1)));
        // int -> str (token index 5 on both sides).
        let op = Opcode::new(Tag::Replace, 5, 6, 5, 6);
        let sig = retype_sig(&sa, &sb, &op).unwrap();
        assert_eq!((sig.old.as_str(), sig.new.as_str()), ("int", "str"));
        // y -> z is outside any annotation.
        let op = Opcode::new(Tag::Replace, 7, 8, 7, 8);
        assert!(retype_sig(&sa, &sb, &op).is_none());
        // A deletion of only ":" has no annotation tokens on either side.
        let op = Opcode::new(Tag::Delete, 4, 5, 4, 4);
        assert!(retype_sig(&sa, &sb, &op).is_none());
    }

    #[test]
    fn retype_removal_uses_neighbours_on_the_empty_side() {
        let mut a = analysis("def f(x: int):");
        let b = analysis("def f(x):");
        a.annotations = vec![Annotation {
            start: Pos::new(1, 9),
            end: Pos::new(1, 12),
            text: "int".into(),
            target: "param x".into(),
        }];
        let sa = tokens_in_range(&a, Some((1, 1)));
        let sb = tokens_in_range(&b, Some((1, 1)));
        // delete [":", "int"] after "x"
        let op = Opcode::new(Tag::Delete, 4, 6, 4, 4);
        let sig = retype_sig(&sa, &sb, &op).unwrap();
        assert_eq!(sig.key, "retype\0int\0(untyped)");
        assert_eq!(sig.detail, "param x");
    }

    fn tokens_in_range<'a>(an: &'a FileAnalysis, rng: super::super::LineRange) -> Side<'a> {
        super::super::tokens_in_range(an, rng)
    }
}
