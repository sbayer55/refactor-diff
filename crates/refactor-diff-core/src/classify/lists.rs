//! List element changes: the whole unit only adds or only removes elements of one bracketed
//! list, plus whatever reflow the formatter did around it.
//!
//! Both sides are compared without layout: structural tokens and comments are dropped, and so
//! are trailing commas (a comma right before a closing bracket, except the one that makes a
//! one-element tuple). What is left must differ by whole elements of a single list:
//!
//! * a comma list in `[]` (array), `{}` (object literal, or destructure when bound with
//!   `const`/`let`/`var` or followed by `=`) or `()` (args);
//! * a type union (`A | B`), in an annotation or a `type X =` alias.
//!
//! The enclosing bracket is found in the whole file, so a one-sided unit that deletes the line
//! `programId: "p1",` inside a surviving object literal is a list removal too. A unit that
//! removes some elements and adds others is not a list change (it falls through to the token
//! classifier), and neither is one whose comments changed. When nothing is left to compare the
//! unit is formatting: Prettier re-wrapping a list and dropping its trailing comma.
//!
//! Key: `list\0{container}\0{±element}…`, elements sorted. In an object, a `key: <literal>`
//! element keys on the literal's kind (`programId : <str>`), so the same field dropped from
//! many records with different values shares one key.

use super::{Classification, LineRange, Side, formatting, highlight, render};
use crate::lang::{FileAnalysis, LanguageAnalyzer, Token, TokenKind};
use crate::model::{Signature, SignatureKind};
use crate::seqmatch::{Opcode, SequenceMatcher, Tag};

/// Longest element (in significant tokens) that still counts as a list element; a bigger
/// removal is more than a mechanical list edit.
pub const MAX_ELEMENT_TOKENS: usize = 32;
/// How far back the union check looks for the annotation colon or `type X =`.
const MAX_UNION_SCAN: usize = 200;

/// Classify the unit as a list element change, as formatting when only layout and trailing
/// commas differ, or `None` when neither applies.
pub(super) fn classify_list(
    analyzer: &dyn LanguageAnalyzer,
    a: &Side<'_>,
    b: &Side<'_>,
    old_range: LineRange,
    new_range: LineRange,
) -> Option<Classification> {
    if comments(a) != comments(b) {
        return None;
    }
    let (na, nb) = (normalized(a), normalized(b));
    let va: Vec<&str> = na
        .iter()
        .map(|&i| a.analysis.tokens[i].value.as_str())
        .collect();
    let vb: Vec<&str> = nb
        .iter()
        .map(|&i| b.analysis.tokens[i].value.as_str())
        .collect();
    let ops: Vec<_> = SequenceMatcher::new(&va, &vb)
        .opcodes()
        .into_iter()
        .filter(|op| op.tag != Tag::Equal)
        .collect();
    if ops.is_empty() {
        return Some(Classification::single(formatting()));
    }
    let removal = ops.iter().all(|op| op.tag == Tag::Delete);
    if !removal && !ops.iter().all(|op| op.tag == Tag::Insert) {
        return None;
    }
    let (side, idx, rng) = if removal {
        (a, &na, old_range)
    } else {
        (b, &nb, new_range)
    };
    let toks = &side.analysis.tokens;

    let mut list: Option<(usize, &'static str)> = None;
    let mut elements: Vec<Vec<usize>> = Vec::new();
    let mut hl = super::Highlights::new();
    for op in &ops {
        let span = if removal {
            &idx[op.i1..op.i2]
        } else {
            &idx[op.j1..op.j2]
        };
        let found = comma_list(analyzer, toks, span).or_else(|| union_list(toks, span))?;
        match list {
            Some(seen) if seen != (found.open, found.container) => return None,
            _ => list = Some((found.open, found.container)),
        }
        for &i in span {
            highlight(&mut hl, std::slice::from_ref(&toks[i]), rng);
        }
        elements.extend(found.removed);
    }
    let (_, container) = list?;
    if elements
        .iter()
        .any(|e| e.is_empty() || e.len() > MAX_ELEMENT_TOKENS)
    {
        return None;
    }

    let sign = if removal { '-' } else { '+' };
    let mut parts: Vec<String> = elements
        .iter()
        .map(|e| format!("{sign}{}", element_norm(toks, e, container)))
        .collect();
    parts.sort();
    let key = format!("list\0{container}\0{}", parts.join("\0"));
    let text = elements
        .iter()
        .map(|e| element_display(side.analysis, e, container))
        .collect::<Vec<_>>()
        .join(", ");
    let (old, new) = if removal {
        (text, String::new())
    } else {
        (String::new(), text)
    };
    let mut cls = Classification::single(
        Signature::new(SignatureKind::List, key, old, new).with_detail(container),
    );
    if removal {
        cls.old_hl = hl;
    } else {
        cls.new_hl = hl;
    }
    Some(cls)
}

/// The op only adds or drops trailing commas (and layout).
pub(super) fn trailing_comma_op(a: &Side<'_>, b: &Side<'_>, op: &Opcode) -> bool {
    let layout = |side: &Side<'_>, lo: usize, hi: usize| {
        (side.offset + lo..side.offset + hi).all(|i| {
            let toks = &side.analysis.tokens;
            !significant(&toks[i]) || is_trailing_comma(toks, i)
        })
    };
    let commas = |side: &Side<'_>, lo: usize, hi: usize| {
        side.tokens()[lo..hi].iter().any(|t| t.value == ",")
    };
    (commas(a, op.i1, op.i2) || commas(b, op.j1, op.j2))
        && layout(a, op.i1, op.i2)
        && layout(b, op.j1, op.j2)
}

/// The values of the side's comments, in order.
fn comments<'a>(side: &Side<'a>) -> Vec<&'a str> {
    side.tokens()
        .iter()
        .filter(|t| t.kind == TokenKind::Comment)
        .map(|t| t.value.as_str())
        .collect()
}

/// Indices (into the file's tokens) of the side's tokens that take part in the comparison.
fn normalized(side: &Side<'_>) -> Vec<usize> {
    let toks = &side.analysis.tokens;
    (side.offset..side.offset + side.len)
        .filter(|&i| significant(&toks[i]) && !is_trailing_comma(toks, i))
        .collect()
}

fn significant(t: &Token) -> bool {
    !matches!(t.kind, TokenKind::Structural | TokenKind::Comment)
}

fn is_open(t: &Token) -> bool {
    matches!(t.value.as_str(), "(" | "[" | "{" | "${") && t.kind != TokenKind::String
}

fn is_close(t: &Token) -> bool {
    matches!(t.value.as_str(), ")" | "]" | "}") && t.kind != TokenKind::String
}

fn next_sig(toks: &[Token], i: usize) -> Option<usize> {
    (i + 1..toks.len()).find(|&j| significant(&toks[j]))
}

fn prev_sig(toks: &[Token], i: usize) -> Option<usize> {
    (0..i).rev().find(|&j| significant(&toks[j]))
}

/// A comma right before a closing bracket. The comma of a one-element parenthesized tuple
/// (`(a,)`, not a call) is kept: it is not layout.
fn is_trailing_comma(toks: &[Token], i: usize) -> bool {
    if toks[i].value != "," || toks[i].kind == TokenKind::String {
        return false;
    }
    let Some(close) = next_sig(toks, i).filter(|&j| is_close(&toks[j])) else {
        return false;
    };
    if toks[close].value != ")" {
        return true;
    }
    let Some(open) = matching_open(toks, close) else {
        return true;
    };
    let is_call = prev_sig(toks, open).is_some_and(|p| {
        toks[p].kind == TokenKind::Name || matches!(toks[p].value.as_str(), ")" | "]")
    });
    is_call || top_level_commas(toks, open, close) > 1
}

fn top_level_commas(toks: &[Token], open: usize, close: usize) -> usize {
    let mut depth = 0i32;
    let mut n = 0;
    for t in &toks[open + 1..close] {
        if !significant(t) {
            continue;
        }
        if is_open(t) {
            depth += 1;
        } else if is_close(t) {
            depth -= 1;
        } else if depth == 0 && t.value == "," {
            n += 1;
        }
    }
    n
}

fn matching_open(toks: &[Token], close: usize) -> Option<usize> {
    enclosing_open(toks, close)
}

/// The innermost bracket opened before `before` and not closed by then.
fn enclosing_open(toks: &[Token], before: usize) -> Option<usize> {
    let mut depth = 0usize;
    for j in (0..before).rev() {
        let t = &toks[j];
        if !significant(t) {
            continue;
        }
        if is_close(t) {
            depth += 1;
        } else if is_open(t) {
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

fn matching_close(toks: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open + 1) {
        if !significant(t) {
            continue;
        }
        if is_open(t) {
            depth += 1;
        } else if is_close(t) {
            if depth == 0 {
                return Some(j);
            }
            depth -= 1;
        }
    }
    None
}

/// A list the span removes whole elements from.
struct Found {
    /// Token index of the opening bracket (or of the span start, for a union).
    open: usize,
    container: &'static str,
    removed: Vec<Vec<usize>>,
}

/// The span is whole elements of the comma list around it, with one separator each.
fn comma_list(analyzer: &dyn LanguageAnalyzer, toks: &[Token], span: &[usize]) -> Option<Found> {
    let (&first, &last) = (span.first()?, span.last()?);
    let open = enclosing_open(toks, first)?;
    if toks[open].value == "${" {
        return None;
    }
    let close = matching_close(toks, open)?;
    if close <= last {
        return None;
    }
    // Elements and separators of the list, without layout and the trailing comma.
    let mut elems: Vec<Vec<usize>> = vec![vec![]];
    let mut seps: Vec<usize> = Vec::new();
    let mut depth = 0i32;
    for j in open + 1..close {
        let t = &toks[j];
        if !significant(t) || is_trailing_comma(toks, j) {
            continue;
        }
        if depth == 0 && t.value == "," && t.kind != TokenKind::String {
            seps.push(j);
            elems.push(vec![]);
            continue;
        }
        if is_open(t) {
            depth += 1;
        } else if is_close(t) {
            depth -= 1;
        }
        elems.last_mut().expect("starts non-empty").push(j);
    }
    if elems.iter().any(Vec::is_empty) {
        return None;
    }
    let removed = whole_elements(&elems, &seps, span)?;
    let container = container_of(toks, open, close);
    if container == "object" && !elems.iter().all(|e| member_like(analyzer, toks, e)) {
        return None;
    }
    Some(Found {
        open,
        container,
        removed,
    })
}

/// Which elements `span` covers, if it is exactly elements `k..=m` with one separator each:
/// the ones after them (`b, c,` of `a, b, c, d`), the ones before them (`, b, c`), or none
/// when they end the list (`b, c` of `a, b, c`, leaving a trailing comma behind).
fn whole_elements(elems: &[Vec<usize>], seps: &[usize], span: &[usize]) -> Option<Vec<Vec<usize>>> {
    let first = span[0];
    let mut seq: Vec<usize> = Vec::new();
    if let Some(k) = elems.iter().position(|e| e[0] == first) {
        for m in k..elems.len() {
            seq.extend(&elems[m]);
            if m + 1 == elems.len() {
                return (seq == span).then(|| elems[k..=m].to_vec());
            }
            if seq.len() >= span.len() {
                // An element without its separator, mid-list: not a whole-element edit.
                return None;
            }
            seq.push(seps[m]);
            if seq.len() >= span.len() {
                return (seq == span).then(|| elems[k..=m].to_vec());
            }
        }
        return None;
    }
    let k = seps.iter().position(|&s| s == first)? + 1;
    for m in k..elems.len() {
        seq.push(seps[m - 1]);
        seq.extend(&elems[m]);
        if seq.len() >= span.len() {
            return (seq == span).then(|| elems[k..=m].to_vec());
        }
    }
    None
}

fn container_of(toks: &[Token], open: usize, close: usize) -> &'static str {
    match toks[open].value.as_str() {
        "(" => "args",
        v => {
            let bound = prev_sig(toks, open)
                .is_some_and(|p| matches!(toks[p].value.as_str(), "const" | "let" | "var"))
                || next_sig(toks, close).is_some_and(|n| toks[n].value == "=");
            match (v, bound) {
                (_, true) => "destructure",
                ("[", false) => "array",
                _ => "object",
            }
        }
    }
}

/// An object member: `key: value`, a spread, a shorthand name or `name = default`. Keeps a
/// block's only statement (`{ return; }`) from passing as an object.
fn member_like(analyzer: &dyn LanguageAnalyzer, toks: &[Token], elem: &[usize]) -> bool {
    let t0 = &toks[elem[0]];
    let second = elem.get(1).map(|&i| toks[i].value.as_str());
    match second {
        _ if matches!(t0.value.as_str(), "..." | "**") => true,
        Some(":") => true,
        Some("?") => elem.get(2).is_some_and(|&i| toks[i].value == ":"),
        _ if t0.kind == TokenKind::Name && analyzer.is_keyword(&t0.value) => false,
        None => t0.kind == TokenKind::Name || t0.kind == TokenKind::String,
        Some("=") => t0.kind == TokenKind::Name,
        Some(_) => t0.value == "[" && elem.iter().any(|&i| toks[i].value == ":"),
    }
}

/// The span is whole members of a type union: `| A | B` or `A | B |`, inside an annotation
/// (after `:`) or a `type X =` alias.
fn union_list(toks: &[Token], span: &[usize]) -> Option<Found> {
    let (&first, &last) = (span.first()?, span.last()?);
    let lead = toks[first].value == "|";
    let tail = toks[last].value == "|";
    if lead == tail {
        return None;
    }
    let mut pieces: Vec<Vec<usize>> = vec![vec![]];
    let mut depth = 0i32;
    for &i in span {
        let t = &toks[i];
        if depth == 0 && t.value == "|" {
            pieces.push(vec![]);
            continue;
        }
        if is_open(t) {
            depth += 1;
        } else if is_close(t) {
            depth -= 1;
            if depth < 0 {
                return None;
            }
        } else if depth == 0 && matches!(t.value.as_str(), "," | ";" | "=" | "=>" | ":" | "?") {
            return None;
        }
        pieces.last_mut().expect("starts non-empty").push(i);
    }
    if depth != 0 {
        return None;
    }
    if lead {
        pieces.remove(0);
        prev_sig(toks, first)?;
    } else {
        pieces.pop();
        let next = next_sig(toks, last)?;
        if is_close(&toks[next]) || matches!(toks[next].value.as_str(), "," | ";" | "=") {
            return None;
        }
    }
    if pieces.iter().any(Vec::is_empty) || !in_type_context(toks, first) {
        return None;
    }
    Some(Found {
        open: first,
        container: "union",
        removed: pieces,
    })
}

/// Walking back from `at` over the union's other members reaches an annotation colon or the
/// `=` of a `type X =` alias.
fn in_type_context(toks: &[Token], at: usize) -> bool {
    let mut depth = 0usize;
    let mut j = at;
    for _ in 0..MAX_UNION_SCAN {
        let Some(p) = prev_sig(toks, j) else {
            return false;
        };
        j = p;
        let t = &toks[p];
        if is_close(t) {
            depth += 1;
            continue;
        }
        if is_open(t) {
            if depth == 0 {
                return false;
            }
            depth -= 1;
            continue;
        }
        if depth > 0 {
            continue;
        }
        match t.value.as_str() {
            ":" => return true,
            "=" => {
                let name = prev_sig(toks, p);
                let kw = name.and_then(|n| prev_sig(toks, n));
                return name.is_some_and(|n| toks[n].kind == TokenKind::Name)
                    && kw.is_some_and(|k| toks[k].value == "type");
            }
            "," | ";" | "=>" | "?" | "&&" | "||" | "return" => return false,
            _ => {}
        }
    }
    false
}

/// The element's key text: token values, with an object member's literal value replaced by
/// its kind.
fn element_norm(toks: &[Token], elem: &[usize], container: &str) -> String {
    if let Some(lit) = literal_member(toks, elem, container) {
        return format!("{} : {lit}", toks[elem[0]].value);
    }
    elem.iter()
        .map(|&i| toks[i].value.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn element_display(an: &FileAnalysis, elem: &[usize], container: &str) -> String {
    let toks = &an.tokens;
    if let Some(lit) = literal_member(toks, elem, container) {
        return format!("{}: {lit}", toks[elem[0]].text);
    }
    let code: Vec<Token> = elem.iter().map(|&i| toks[i].clone()).collect();
    render(&code, &an.lines)
}

/// `<str>`/`<num>` for an object member `key: "literal"` / `key: 1`.
fn literal_member(toks: &[Token], elem: &[usize], container: &str) -> Option<&'static str> {
    if container != "object" || elem.len() != 3 || toks[elem[1]].value != ":" {
        return None;
    }
    match toks[elem[2]].kind {
        TokenKind::String => Some("<str>"),
        TokenKind::Number => Some("<num>"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::{Classification, classify};
    use crate::lang::FileAnalysis;
    use crate::model::SignatureKind;

    fn one(old: &str, new: &str) -> Classification {
        classify_texts(old, new).0
    }

    /// Classify lines `old_range` of `old` against `new_range` of `new`.
    fn ranged(
        old: &str,
        new: &str,
        old_range: Option<(u32, u32)>,
        new_range: Option<(u32, u32)>,
    ) -> (Classification, FileAnalysis, FileAnalysis) {
        let (a, b) = (analysis(old), analysis(new));
        let anchor_old = old_range.map_or(0, |r| r.0 - 1);
        let anchor_new = new_range.map_or(0, |r| r.0 - 1);
        let cls = classify(
            &TestLang, &a, &b, old_range, new_range, anchor_old, anchor_new,
        );
        (cls, a, b)
    }

    #[test]
    fn element_removed_from_an_array_with_reflow() {
        let cls = one("x = [\n  a,\n  b,\n  c,\n]\n", "x = [a, c]\n");
        assert_eq!(
            sigs(&cls),
            vec![("list", "b".into(), "".into(), "array".into())]
        );
        assert_eq!(cls.signatures[0].key, "list\0array\0-b");
        assert_eq!(cls.signatures[0].label(), "delete b");
        assert_eq!(cls.generic_tokens, 0);
        // Highlights cover the removed element and its separator.
        assert_eq!(cls.old_hl.get(&3), Some(&vec![[2, 3], [3, 4]]));
    }

    #[test]
    fn first_last_and_several_elements() {
        assert_eq!(
            one("x = [a, b, c]\n", "x = [b, c]\n").signatures[0].key,
            "list\0array\0-a"
        );
        assert_eq!(
            one("x = [a, b, c]\n", "x = [a, b]\n").signatures[0].key,
            "list\0array\0-c"
        );
        let cls = one("f(a, c, b, d)\n", "f(a, d)\n");
        assert_eq!(cls.signatures[0].key, "list\0args\0-b\0-c");
        assert_eq!(cls.signatures[0].old, "c, b");
        let cls = one("f(a, b, c, d)\n", "f(b, d)\n");
        assert_eq!(cls.signatures[0].key, "list\0args\0-a\0-c");
        assert_eq!(
            one("x = [a]\n", "x = []\n").signatures[0].key,
            "list\0array\0-a"
        );
        let added = one("x = [a]\n", "x = [a, b]\n");
        assert_eq!(added.signatures[0].key, "list\0array\0+b");
        assert_eq!(added.signatures[0].label(), "insert b");
    }

    #[test]
    fn object_members_key_on_the_literal_kind() {
        let a = one("x = {'a': 1, 'b': 'p1'}\n", "x = {'a': 1}\n");
        let b = one("y = {'b': 'p2', 'c': 3}\n", "y = {'c': 3}\n");
        assert_eq!(a.signatures[0].key, "list\0object\0-'b' : <str>");
        assert_eq!(a.signatures[0].key, b.signatures[0].key);
        assert_eq!(a.signatures[0].old, "'b': <str>");
        assert_eq!(a.signatures[0].detail, "object");
    }

    #[test]
    fn trailing_comma_only_is_formatting() {
        let cls = one("f(\n  a,\n  b,\n)\n", "f(a, b)\n");
        assert_eq!(cls.signatures[0].kind, SignatureKind::Formatting);
        // A one-element tuple's comma is not layout.
        let cls = one("x = (a,)\n", "x = (a)\n");
        assert_ne!(cls.signatures[0].kind, SignatureKind::Formatting);
    }

    #[test]
    fn mixed_and_partial_changes_fall_through() {
        let kinds = |o: &str, n: &str| -> Vec<SignatureKind> {
            one(o, n).signatures.iter().map(|s| s.kind).collect()
        };
        assert!(!kinds("x = [a, b, c]\n", "x = [a, d]\n").contains(&SignatureKind::List));
        assert!(!kinds("x = [a, b]\n", "x = [c, a]\n").contains(&SignatureKind::List));
        // Part of an element.
        assert!(!kinds("x = [a.b, c]\n", "x = [a, c]\n").contains(&SignatureKind::List));
        // Elements of two different lists.
        assert!(!kinds("f([a, b], [c, d])\n", "f([a], [c])\n").contains(&SignatureKind::List));
        // A comment changed too.
        assert!(!kinds("x = [a, b]  # one\n", "x = [a]  # two\n").contains(&SignatureKind::List));
        // A block's statement is not an object member.
        assert!(!kinds("if x {\n  return\n}\n", "if x {\n}\n").contains(&SignatureKind::List));
    }

    #[test]
    fn deleted_member_line_inside_a_surviving_object() {
        let old = "x = {\n  'a': 1,\n  'b': 'p1',\n  'c': 3,\n}\n";
        let new = "x = {\n  'a': 1,\n  'c': 3,\n}\n";
        let (cls, _, _) = ranged(old, new, Some((3, 3)), None);
        assert_eq!(cls.signatures[0].key, "list\0object\0-'b' : <str>");
        // The last member: its trailing comma goes with it.
        let new = "x = {\n  'a': 1,\n  'b': 'p1',\n}\n";
        let (cls, _, _) = ranged(old, new, Some((4, 4)), None);
        assert_eq!(cls.signatures[0].key, "list\0object\0-'c' : <num>");
        // Added member line.
        let (cls, _, _) = ranged(new, old, None, Some((4, 4)));
        assert_eq!(cls.signatures[0].key, "list\0object\0+'c' : <num>");
        // A whole statement line is not a list element.
        let (cls, _, _) = ranged("a = 1\nb = 2\n", "a = 1\n", Some((2, 2)), None);
        assert_ne!(cls.signatures[0].kind, SignatureKind::List);
    }

    #[test]
    fn destructure_and_union_containers() {
        let cls = one("const {a, b} = x\n", "const {a} = x\n");
        assert_eq!(cls.signatures[0].key, "list\0destructure\0-b");
        let cls = one("def f(x: int | str | None):\n", "def f(x: int | None):\n");
        assert_eq!(cls.signatures[0].key, "list\0union\0-str");
        assert_eq!(cls.signatures[0].label(), "delete str");
        let cls = one("type X = 'a' | 'b'\n", "type X = 'a'\n");
        assert_eq!(cls.signatures[0].key, "list\0union\0-'b'");
        // A bitwise or in a value is not a union.
        let cls = one("y = a | b\n", "y = a\n");
        assert_ne!(cls.signatures[0].kind, SignatureKind::List);
    }
}
