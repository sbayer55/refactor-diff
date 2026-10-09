//! Classify one change unit into mechanical edit signatures.
//!
//! The old and new token streams of the unit are aligned with the difflib port. Each differing
//! span becomes a signature:
//!
//! * rename     - a single identifier swapped for another (`get_user` -> `fetch_user`)
//! * retype     - every changed token sits inside a type annotation
//! * replace    - any other token-level substitution, insertion or deletion
//! * formatting - token streams are identical (only whitespace/layout changed)
//! * docs       - only comments or docstrings changed
//! * args       - a call's or def's argument list changed shape (see [`args`])
//! * import     - an import changed module path or gained/lost a name (see [`imports`])
//!
//! This is a port of `refactor_diff/patterns.py`; keys, labels and highlight ranges must match
//! it exactly because report ids and groups are derived from them.

mod args;
mod clusters;
mod imports;
mod rename;

use std::collections::BTreeMap;

use crate::lang::{FileAnalysis, LanguageAnalyzer, Token, TokenKind};
use crate::model::{Line, Signature, SignatureKind, Unit};
use crate::seqmatch::{Opcode, SequenceMatcher, Tag};
use crate::short_hash;

pub use imports::imports_only;
pub use rename::rename_context;

/// Longest display text for a signature's old/new side (in characters).
pub const MAX_LABEL: usize = 160;
/// Unchanged tokens a bracket wrap may enclose.
pub const MAX_WRAP_GAP: usize = 12;
/// Display text of a template hole.
pub const PLACEHOLDER: &str = "…";

/// 1-based inclusive `(first, last)` line range; `None` when the side is empty.
pub type LineRange = Option<(u32, u32)>;

/// The tokens of one side of a unit: a contiguous slice of `analysis.tokens`.
#[derive(Clone, Copy, Debug)]
pub struct Side<'a> {
    pub analysis: &'a FileAnalysis,
    /// Index of `tokens()[0]` in `analysis.tokens`.
    pub offset: usize,
    pub len: usize,
}

impl<'a> Side<'a> {
    /// The tokens overlapping the unit's lines.
    pub fn tokens(&self) -> &'a [Token] {
        &self.analysis.tokens[self.offset..self.offset + self.len]
    }

    fn empty(analysis: &'a FileAnalysis) -> Self {
        Self {
            analysis,
            offset: 0,
            len: 0,
        }
    }
}

/// The contiguous run of tokens that overlap `rng` (a token counts when it starts on or
/// before the last line and ends on or after the first).
pub fn tokens_in_range<'a>(analysis: &'a FileAnalysis, rng: LineRange) -> Side<'a> {
    let Some((first, last)) = rng else {
        return Side::empty(analysis);
    };
    let overlaps = |t: &Token| t.start.line <= last && t.end.line >= first;
    let Some(lo) = analysis.tokens.iter().position(overlaps) else {
        return Side::empty(analysis);
    };
    let hi = analysis
        .tokens
        .iter()
        .rposition(overlaps)
        .expect("a token matched above");
    Side {
        analysis,
        offset: lo,
        len: hi + 1 - lo,
    }
}

/// Highlight ranges per 1-based line: `[start_col, end_col)` pairs, unmerged.
pub type Highlights = BTreeMap<u32, Vec<[u32; 2]>>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Classification {
    pub signatures: Vec<Signature>,
    pub old_hl: Highlights,
    pub new_hl: Highlights,
    /// Tokens covered by "replace" signatures; lower = cleaner.
    pub generic_tokens: usize,
}

impl Classification {
    fn single(sig: Signature) -> Self {
        Self {
            signatures: vec![sig],
            ..Default::default()
        }
    }

    /// Append `sig` unless a signature with the same key is already present (first wins).
    fn add(&mut self, sig: Signature) {
        if self.signatures.iter().all(|s| s.key != sig.key) {
            self.signatures.push(sig);
        }
    }
}

/// Classify the change between `old_range` and `new_range`. For a one-sided unit the
/// `*_anchor` is the 1-based line before the insertion/deletion point on the empty side.
pub fn classify(
    analyzer: &dyn LanguageAnalyzer,
    old: &FileAnalysis,
    new: &FileAnalysis,
    old_range: LineRange,
    new_range: LineRange,
    old_anchor: u32,
    new_anchor: u32,
) -> Classification {
    let a = tokens_in_range(old, old_range);
    let b = tokens_in_range(new, new_range);
    let a_vals: Vec<&str> = a.tokens().iter().map(|t| t.value.as_str()).collect();
    let b_vals: Vec<&str> = b.tokens().iter().map(|t| t.value.as_str()).collect();
    if a_vals == b_vals {
        return Classification::single(formatting());
    }

    let ops: Vec<Opcode> = SequenceMatcher::new(&a_vals, &b_vals)
        .opcodes()
        .into_iter()
        .filter(|op| op.tag != Tag::Equal)
        .collect();
    if let Some(imports) = imports::import_classification(&a, &b, &ops, old_range, new_range) {
        return imports;
    }
    let mut classified: Vec<Option<Signature>> = ops
        .iter()
        .map(|op| rename::classify_op(analyzer, &a, &b, op))
        .collect();
    args::classify_args(&a, &b, &ops, &mut classified, old_anchor, new_anchor);

    let mut result = Classification::default();
    for cluster in clusters::clusters(&a, &b, &ops, &classified) {
        if cluster.iter().all(|&k| classified[k].is_some()) {
            for &k in &cluster {
                result.add(classified[k].clone().expect("checked above"));
            }
        } else if let [k] = cluster[..] {
            let op = &ops[k];
            result.generic_tokens += op.i2 - op.i1 + op.j2 - op.j1;
            result.add(clusters::replace_sig(&a, &b, op));
        } else {
            let cluster_ops: Vec<Opcode> = cluster.iter().map(|&k| ops[k]).collect();
            result.generic_tokens += cluster_ops
                .iter()
                .map(|op| op.i2 - op.i1 + op.j2 - op.j1)
                .sum::<usize>();
            result.add(clusters::template_sig(&a, &b, &cluster_ops));
        }
        for &k in &cluster {
            let op = &ops[k];
            highlight(&mut result.old_hl, &a.tokens()[op.i1..op.i2], old_range);
            highlight(&mut result.new_hl, &b.tokens()[op.j1..op.j2], new_range);
        }
    }
    result
}

fn formatting() -> Signature {
    Signature::new(SignatureKind::Formatting, "formatting", "", "")
}

/// Build the report unit for a line opcode from its classification.
pub fn make_unit(
    path: &str,
    hunk_id: &str,
    op: &Opcode,
    cls: Classification,
    old_lines: &[String],
    new_lines: &[String],
) -> Unit {
    let lines = |texts: &[String], lo: usize, hi: usize, hl: &Highlights| -> Vec<Line> {
        (lo..hi)
            .map(|i| Line {
                text: texts[i].clone(),
                hl: merge_ranges(hl.get(&(i as u32 + 1)).cloned().unwrap_or_default()),
            })
            .collect()
    };
    Unit {
        id: short_hash!(path, op.i1, op.i2, op.j1, op.j2),
        path: path.to_string(),
        hunk_id: hunk_id.to_string(),
        old_start: op.i1 as u32 + 1,
        new_start: op.j1 as u32 + 1,
        old: lines(old_lines, op.i1, op.i2, &cls.old_hl),
        new: lines(new_lines, op.j1, op.j2, &cls.new_hl),
        signatures: cls.signatures,
        explained: false,
        partner: None,
        verified: false,
        near: vec![],
        tags: vec![],
    }
}

/// Sort `[start, end]` ranges and merge the ones that touch or overlap.
pub fn merge_ranges(mut ranges: Vec<[u32; 2]>) -> Vec<[u32; 2]> {
    ranges.sort_unstable();
    let mut merged: Vec<[u32; 2]> = Vec::with_capacity(ranges.len());
    for [start, end] in ranges {
        match merged.last_mut() {
            Some(last) if start <= last[1] => last[1] = last[1].max(end),
            _ => merged.push([start, end]),
        }
    }
    merged
}

/// Column `10**6`: "to the end of the line" for a token that continues past it.
const LINE_END: u32 = 1_000_000;

/// Record the columns of `tokens` on every line of `rng` they touch.
fn highlight(target: &mut Highlights, tokens: &[Token], rng: LineRange) {
    let Some((first, last)) = rng else {
        return;
    };
    for t in tokens {
        if t.kind == TokenKind::Structural {
            continue;
        }
        for line in t.start.line.max(first)..=t.end.line.min(last) {
            let start = if line == t.start.line { t.start.col } else { 0 };
            let end = if line == t.end.line {
                t.end.col
            } else {
                LINE_END
            };
            if end > start {
                target.entry(line).or_default().push([start, end]);
            }
        }
    }
}

/// Reconstruct readable source text for a token run (single line, truncated).
pub fn render(tokens: &[Token], lines: &[String]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Token> = None;
    for t in tokens {
        if t.kind == TokenKind::Structural {
            continue;
        }
        if let Some(p) = prev {
            let line = (t.start.line as usize)
                .checked_sub(1)
                .and_then(|i| lines.get(i));
            match line {
                Some(src) if p.end.line == t.start.line => {
                    out.extend(
                        src.chars()
                            .skip(p.end.col as usize)
                            .take((t.start.col as usize).saturating_sub(p.end.col as usize)),
                    );
                }
                _ => out.push(' '),
            }
        }
        if t.text.contains('\n') {
            out.push_str(&crate::lang::collapse_ws(&t.text));
        } else {
            out.push_str(&t.text);
        }
        prev = Some(t);
    }
    let mut text = out.trim().to_string();
    if text.is_empty() && !tokens.is_empty() {
        text = "(indentation)".to_string();
    }
    if text.chars().count() <= MAX_LABEL {
        text
    } else {
        let mut cut: String = text.chars().take(MAX_LABEL - 1).collect();
        cut.push_str(PLACEHOLDER);
        cut
    }
}

/// A test-only tokenizer and language so the classifier can be exercised on hand-built
/// analyses without the real tree-sitter analyzers.
#[cfg(test)]
pub(crate) mod testutil {
    use crate::lang::{FileAnalysis, LanguageAnalyzer, Token, TokenKind};
    use crate::text::{Pos, split_lines};

    /// Keywords of the test language (a Python-ish subset).
    const KEYWORDS: &[&str] = &[
        "and", "or", "not", "if", "else", "def", "class", "import", "from", "return", "pass", "as",
    ];

    pub struct TestLang;

    impl LanguageAnalyzer for TestLang {
        fn name(&self) -> &'static str {
            "test"
        }
        fn globs(&self) -> &'static [&'static str] {
            &["*.test"]
        }
        fn handles(&self, path: &str) -> bool {
            path.ends_with(".test")
        }
        fn analyze(&self, text: &str) -> FileAnalysis {
            analysis(text)
        }
        fn is_keyword(&self, value: &str) -> bool {
            KEYWORDS.contains(&value)
        }
        fn is_builtin(&self, _value: &str) -> bool {
            false
        }
        fn import_keywords(&self) -> &'static [&'static str] {
            &["import", "from"]
        }
        fn definition_keywords(&self) -> &'static [&'static str] {
            &["def", "class"]
        }
    }

    /// Tokens for `text`: names, numbers, `"…"`/`'…'` strings, `#` comments, two-character
    /// and single-character operators, and a `<NEWLINE>` structural token at the end of each
    /// non-blank line. Strings keep their quotes in `text` but compare by their contents.
    pub fn tokenize(text: &str) -> Vec<Token> {
        let mut out = Vec::new();
        for (row, line) in split_lines(text).iter().enumerate() {
            let line_no = row as u32 + 1;
            let chars: Vec<char> = line.chars().collect();
            let mut col = 0usize;
            let mut any = false;
            while col < chars.len() {
                let c = chars[col];
                let start = col;
                let (kind, value, text): (TokenKind, String, String);
                if c.is_whitespace() {
                    col += 1;
                    continue;
                } else if c == '#' {
                    let s: String = chars[start..].iter().collect();
                    col = chars.len();
                    text = s.clone();
                    value = s.trim_end().to_string();
                    kind = TokenKind::Comment;
                } else if c == '"' || c == '\'' {
                    col += 1;
                    while col < chars.len() && chars[col] != c {
                        col += 1;
                    }
                    col = (col + 1).min(chars.len());
                    text = chars[start..col].iter().collect();
                    // Like Python's `repr(ast.literal_eval(s))`: quote style is normalized.
                    let inner: String = chars[start + 1..col.saturating_sub(1).max(start + 1)]
                        .iter()
                        .collect();
                    value = format!("'{inner}'");
                    kind = TokenKind::String;
                } else if c.is_alphabetic() || c == '_' {
                    while col < chars.len() && (chars[col].is_alphanumeric() || chars[col] == '_') {
                        col += 1;
                    }
                    text = chars[start..col].iter().collect();
                    value = text.clone();
                    kind = TokenKind::Name;
                } else if c.is_ascii_digit() {
                    while col < chars.len() && (chars[col].is_alphanumeric() || chars[col] == '.') {
                        col += 1;
                    }
                    text = chars[start..col].iter().collect();
                    value = text.clone();
                    kind = TokenKind::Number;
                } else {
                    let two: String = chars[start..(start + 2).min(chars.len())].iter().collect();
                    if ["->", "**", "==", "!=", "<=", ">=", ":="].contains(&two.as_str()) {
                        col += 2;
                    } else {
                        col += 1;
                    }
                    text = chars[start..col].iter().collect();
                    value = text.clone();
                    kind = TokenKind::Op;
                }
                any = true;
                out.push(Token::new(
                    kind,
                    value,
                    text,
                    Pos::new(line_no, start as u32),
                    Pos::new(line_no, col as u32),
                ));
            }
            if any {
                let at = Pos::new(line_no, chars.len() as u32);
                out.push(Token::structural("<NEWLINE>", at));
            }
        }
        out
    }

    /// An analysis with lines and tokens only.
    pub fn analysis(text: &str) -> FileAnalysis {
        FileAnalysis {
            lines: split_lines(text),
            tokens: tokenize(text),
            parsed: true,
            ..Default::default()
        }
    }

    /// The whole file as a range, or `None` for an empty file.
    pub fn whole(an: &FileAnalysis) -> super::LineRange {
        let n = an.lines.len() as u32;
        (n > 0).then_some((1, n))
    }

    /// Classify two whole files.
    pub fn classify_texts(
        old: &str,
        new: &str,
    ) -> (super::Classification, FileAnalysis, FileAnalysis) {
        let (a, b) = (analysis(old), analysis(new));
        let cls = super::classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        (cls, a, b)
    }

    /// `(kind, old, new, detail)` of every signature, like the Python tests' `sigs()`.
    pub fn sigs(cls: &super::Classification) -> Vec<(&'static str, String, String, String)> {
        cls.signatures
            .iter()
            .map(|s| {
                (
                    s.kind.as_str(),
                    s.old.clone(),
                    s.new.clone(),
                    s.detail.clone(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::lang::{Annotation, Binding, CallSite, ImportSite};
    use crate::text::Pos;

    fn sig_tuples(old: &str, new: &str) -> Vec<(&'static str, String, String, String)> {
        sigs(&classify_texts(old, new).0)
    }

    fn t(old: &str, new: &str, detail: &str) -> (&'static str, String, String, String) {
        ("", old.into(), new.into(), detail.into())
    }

    fn with_kind(
        kind: &'static str,
        tuple: (&'static str, String, String, String),
    ) -> (&'static str, String, String, String) {
        (kind, tuple.1, tuple.2, tuple.3)
    }

    #[test]
    fn tokens_in_range_is_a_contiguous_slice() {
        let an = analysis("a = 1\nb = 2\nc = 3\n");
        let side = tokens_in_range(&an, Some((2, 2)));
        assert_eq!(side.offset, 4);
        assert_eq!(
            side.tokens()
                .iter()
                .map(|t| t.value.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "=", "2", "<NEWLINE>"]
        );
        let empty = tokens_in_range(&an, None);
        assert_eq!((empty.offset, empty.len), (0, 0));
        let beyond = tokens_in_range(&an, Some((9, 9)));
        assert_eq!(beyond.len, 0);
    }

    #[test]
    fn identical_token_values_are_formatting() {
        assert_eq!(
            sig_tuples("x = {'a':1}\n", "x = {\"a\": 1}\n"),
            vec![with_kind("formatting", t("", "", ""))]
        );
        // The test tokenizer ends every line with a structural token, so the rewrapped call
        // differs by one `<NEWLINE>`: a structural-only op is still formatting.
        assert_eq!(
            sig_tuples("f(a,\n  b)\n", "f(a, b)\n"),
            vec![with_kind("formatting", t("", "", ""))]
        );
        assert_eq!(sig_tuples("", "")[0].0, "formatting");
    }

    #[test]
    fn rename_at_call_site_definition_attribute_and_keyword() {
        assert_eq!(
            sig_tuples("x = get_user(1)\n", "x = fetch_user(1)\n"),
            vec![with_kind("rename", t("get_user", "fetch_user", "call"))]
        );
        assert_eq!(
            sig_tuples(
                "def get_user():\n    pass\n",
                "def fetch_user():\n    pass\n"
            )[0],
            with_kind("rename", t("get_user", "fetch_user", "definition"))
        );
        assert_eq!(
            sig_tuples("y = obj.old_name\n", "y = obj.new_name\n"),
            vec![with_kind("rename", t("old_name", "new_name", "attribute"))]
        );
        assert_eq!(
            sig_tuples("f(a, role=1)\n", "f(a, roles=1)\n"),
            vec![with_kind("rename", t("role", "roles", "keyword"))]
        );
        assert_eq!(
            sig_tuples("f(a) + f(b)\n", "g(a) + g(b)\n"),
            vec![with_kind("rename", t("f", "g", "call"))],
            "repeated renames dedupe by key"
        );
    }

    #[test]
    fn keyword_swap_is_a_replace() {
        let found = sig_tuples("x = a and b\n", "x = a or b\n");
        assert_eq!(found[0], with_kind("replace", t("and", "or", "")));
        let (cls, _, _) = classify_texts("x = a and b\n", "x = a or b\n");
        assert_eq!(cls.generic_tokens, 2);
        assert_eq!(cls.signatures[0].key, "replace\0and\0\x01\0or");
    }

    #[test]
    fn rename_plus_logic_change_has_both_signatures() {
        let kinds: Vec<_> = sig_tuples(
            "if get_user(u):\n    pass\n",
            "if fetch_user(u) and ok:\n    pass\n",
        )
        .into_iter()
        .map(|s| s.0)
        .collect();
        assert_eq!(kinds, vec!["rename", "replace"]);
    }

    #[test]
    fn dotted_replacement_and_wraps_are_one_signature() {
        assert_eq!(
            sig_tuples("t = cfg.get(\"timeout\")\n", "t = settings.timeout\n"),
            vec![with_kind(
                "replace",
                t("cfg.get(\"timeout\")", "settings.timeout", "")
            )]
        );
        assert_eq!(
            sig_tuples("f(role=\"admin\")\n", "f(roles=(\"admin\",))\n"),
            vec![with_kind("replace", t("role=…", "roles=(…,)", ""))]
        );
        assert_eq!(
            sig_tuples(
                "save(created_by=actor)\n",
                "save(created_by=str(actor.user_id))\n"
            ),
            vec![with_kind("replace", t("…", "str(….user_id)", ""))]
        );
        let a = classify_texts("f(role=\"admin\")\n", "f(roles=(\"admin\",))\n").0;
        let b = classify_texts("g(x, role=\"faculty\")\n", "g(x, roles=(\"faculty\",))\n").0;
        assert_eq!(a.signatures[0].key, b.signatures[0].key);
        assert_eq!(
            a.signatures[0].key,
            "replace\0role\0=\0\x02\0\x01\0roles\0=\0(\0\x02\0,\0)"
        );
    }

    #[test]
    fn comments_are_docs_and_mix_with_code_signatures() {
        assert_eq!(
            sig_tuples("x = 1  # old note\n", "x = 1  # new note\n"),
            vec![with_kind("docs", t("", "", ""))]
        );
        assert_eq!(
            sig_tuples("", "# explain the next line\n"),
            vec![with_kind("docs", t("", "", ""))]
        );
        let kinds: std::collections::BTreeSet<_> = sig_tuples("f(a)  # old\n", "g(a)  # new\n")
            .into_iter()
            .map(|s| s.0)
            .collect();
        assert_eq!(
            kinds.into_iter().collect::<Vec<_>>(),
            vec!["docs", "rename"]
        );
        assert_eq!(sig_tuples("x = \"old\"\n", "x = \"new\"\n")[0].0, "replace");
    }

    #[test]
    fn docstring_change_is_docs_and_layout_only_is_formatting() {
        let old = "def f():\n    \"Old summary.\"\n    return 1\n";
        let new = "def f():\n    \"New summary.\"\n    return 1\n";
        let (mut a, mut b) = (analysis(old), analysis(new));
        a.docstrings = vec![(Pos::new(2, 4), Pos::new(2, 18))];
        b.docstrings = vec![(Pos::new(2, 4), Pos::new(2, 18))];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(sigs(&cls), vec![with_kind("docs", t("", "", ""))]);

        // Only structural tokens differ: the block gained an indentation level.
        let mut a = analysis("x = 1\n");
        let mut b = analysis("x = 1\n");
        a.tokens
            .insert(0, Token::structural("<INDENT>", Pos::new(1, 0)));
        b.tokens
            .insert(0, Token::structural("<INDENT>", Pos::new(1, 0)));
        b.tokens
            .insert(0, Token::structural("<INDENT>", Pos::new(1, 0)));
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(sigs(&cls), vec![with_kind("formatting", t("", "", ""))]);
    }

    #[test]
    fn retype_via_annotations() {
        let ann = |line, s, e, text: &str, target: &str| Annotation {
            start: Pos::new(line, s),
            end: Pos::new(line, e),
            text: text.into(),
            target: target.into(),
        };
        let (mut a, mut b) = (
            analysis("def f(x: int):\n    pass\n"),
            analysis("def f(x: str):\n    pass\n"),
        );
        a.annotations = vec![ann(1, 9, 12, "int", "param x")];
        b.annotations = vec![ann(1, 9, 12, "str", "param x")];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(
            sigs(&cls),
            vec![with_kind("retype", t("int", "str", "param x"))]
        );
        assert_eq!(cls.signatures[0].key, "retype\0int\0str");

        // Annotation added: the old side has only the ":" punctuation, so the neighbours of
        // the insertion point decide.
        let (a, mut b) = (
            analysis("def f(x):\n    pass\n"),
            analysis("def f(x: int):\n    pass\n"),
        );
        b.annotations = vec![ann(1, 9, 12, "int", "param x")];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(
            sigs(&cls),
            vec![with_kind("retype", t("(untyped)", "int", "param x"))]
        );

        // Return annotation: the "->" is dropped from the comparison.
        let (mut a, mut b) = (
            analysis("def f() -> List[int]:\n    pass\n"),
            analysis("def f() -> list[int]:\n    pass\n"),
        );
        a.annotations = vec![ann(1, 11, 20, "List[int]", "return of f")];
        b.annotations = vec![ann(1, 11, 20, "list[int]", "return of f")];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(
            sigs(&cls),
            vec![with_kind(
                "retype",
                t("List[int]", "list[int]", "return of f")
            )]
        );

        // A change outside every annotation is not a retype.
        let (mut a, mut b) = (analysis("count: int = 0\n"), analysis("count: int = 1\n"));
        a.annotations = vec![ann(1, 7, 10, "int", "variable count")];
        b.annotations = vec![ann(1, 7, 10, "int", "variable count")];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(cls.signatures[0].kind, SignatureKind::Replace);
    }

    #[test]
    fn highlights_cover_changed_tokens_only() {
        let (cls, _, _) = classify_texts("x = get_user(1)\n", "x = fetch_user(1)\n");
        assert_eq!(cls.old_hl.get(&1), Some(&vec![[4, 12]]));
        assert_eq!(cls.new_hl.get(&1), Some(&vec![[4, 14]]));
    }

    #[test]
    fn highlight_splits_multi_line_tokens_and_clips_to_the_range() {
        let tok = Token::new(
            TokenKind::String,
            "s",
            "'''a\nb\nc'''",
            Pos::new(1, 4),
            Pos::new(3, 4),
        );
        let mut hl = Highlights::new();
        highlight(&mut hl, std::slice::from_ref(&tok), Some((1, 3)));
        assert_eq!(hl.get(&1), Some(&vec![[4, LINE_END]]));
        assert_eq!(hl.get(&2), Some(&vec![[0, LINE_END]]));
        assert_eq!(hl.get(&3), Some(&vec![[0, 4]]));
        let mut clipped = Highlights::new();
        highlight(&mut clipped, std::slice::from_ref(&tok), Some((2, 2)));
        assert_eq!(clipped.keys().collect::<Vec<_>>(), vec![&2]);
        let mut none = Highlights::new();
        highlight(&mut none, &[tok], None);
        assert!(none.is_empty());
        // A zero-width token on its end line adds nothing.
        let mut zero = Highlights::new();
        highlight(
            &mut zero,
            &[Token::new(
                TokenKind::Op,
                "",
                "",
                Pos::new(1, 2),
                Pos::new(1, 2),
            )],
            Some((1, 1)),
        );
        assert!(zero.is_empty());
    }

    #[test]
    fn merge_ranges_sorts_and_merges_touching_ranges() {
        assert_eq!(
            merge_ranges(vec![[5, 7], [1, 3], [3, 4], [6, 9], [12, 13]]),
            vec![[1, 4], [5, 9], [12, 13]]
        );
        assert_eq!(merge_ranges(vec![]), Vec::<[u32; 2]>::new());
    }

    #[test]
    fn make_unit_hashes_the_opcode_and_merges_highlights() {
        let (cls, a, b) = classify_texts("x = get_user(1)\n", "x = fetch_user(1)\n");
        let op = Opcode::new(Tag::Replace, 0, 1, 0, 1);
        let unit = make_unit("a.py", "h1", &op, cls, &a.lines, &b.lines);
        assert_eq!(unit.id, short_hash!("a.py", 0, 1, 0, 1));
        assert_eq!((unit.old_start, unit.new_start), (1, 1));
        assert_eq!(unit.old[0].text, "x = get_user(1)");
        assert_eq!(unit.old[0].hl, vec![[4, 12]]);
        assert_eq!(unit.new[0].hl, vec![[4, 14]]);
        assert_eq!(unit.signatures[0].kind, SignatureKind::Rename);
        assert!(!unit.explained && unit.partner.is_none() && unit.tags.is_empty());
    }

    #[test]
    fn render_reconstructs_source_between_tokens() {
        let an = analysis("t = cfg.get( \"timeout\" )  # c\n");
        let code: Vec<Token> = an
            .tokens
            .iter()
            .filter(|t| t.kind != TokenKind::Comment)
            .cloned()
            .collect();
        assert_eq!(render(&code, &an.lines), "t = cfg.get( \"timeout\" )");
        // Tokens on different lines are joined with a single space.
        let two = analysis("a\nb\n");
        assert_eq!(render(&two.tokens, &two.lines), "a b");
        // Only structural tokens: "(indentation)"; no tokens at all: "".
        assert_eq!(
            render(&[Token::structural("<INDENT>", Pos::new(1, 0))], &[]),
            "(indentation)"
        );
        assert_eq!(render(&[], &[]), "");
        // Multi-line token text collapses to single spaces.
        let multi = Token::new(
            TokenKind::String,
            "s",
            "'''a\n   b\tc'''",
            Pos::new(1, 0),
            Pos::new(2, 7),
        );
        assert_eq!(
            render(&[multi], &["'''a".into(), "   b\tc'''".into()]),
            "'''a b c'''"
        );
    }

    #[test]
    fn render_slices_by_character_and_truncates_by_character() {
        let an = analysis("é = \"ü\" + x\n");
        let code: Vec<Token> = an.tokens.clone();
        assert_eq!(render(&code, &an.lines), "é = \"ü\" + x");
        let long = "é".repeat(200);
        let tok = Token::new(
            TokenKind::Name,
            long.clone(),
            long.clone(),
            Pos::new(1, 0),
            Pos::new(1, 200),
        );
        let out = render(&[tok], &[long]);
        assert_eq!(out.chars().count(), MAX_LABEL);
        assert!(out.ends_with(PLACEHOLDER));
        assert_eq!(out.chars().filter(|&c| c == 'é').count(), MAX_LABEL - 1);
    }

    #[test]
    fn import_units_classify_by_bindings() {
        let site = |start, end, bindings| ImportSite {
            start,
            end,
            bindings,
        };
        let (mut a, mut b) = (analysis("from a import x\n"), analysis("from b import x\n"));
        a.imports = vec![site(1, 1, vec![Binding::new("a", Some("x".into()), "x")])];
        b.imports = vec![site(1, 1, vec![Binding::new("b", Some("x".into()), "x")])];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(sigs(&cls), vec![with_kind("import", t("a", "b", "x"))]);
        assert_eq!(cls.signatures[0].key, "import\0a\0b");
        assert_eq!(cls.old_hl.get(&1), Some(&vec![[5, 6]]));
        assert_eq!(cls.generic_tokens, 0);

        // Mixed with code: not an import change (falls through to the token classifier).
        let (mut a, mut b) = (
            analysis("from a import x\nz = 1\n"),
            analysis("from b import x\nz = 2\n"),
        );
        a.imports = vec![site(1, 1, vec![Binding::new("a", Some("x".into()), "x")])];
        b.imports = vec![site(1, 1, vec![Binding::new("b", Some("x".into()), "x")])];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert!(
            cls.signatures
                .iter()
                .all(|s| s.kind != SignatureKind::Import)
        );
    }

    #[test]
    fn args_change_at_a_call_site() {
        let call = |name: &str, args: Vec<(u32, u32, Option<&str>)>, end: u32| CallSite {
            name: name.into(),
            start: Pos::new(1, 0),
            end: Pos::new(1, end),
            args_start: Pos::new(1, name.len() as u32),
            args_end: Pos::new(1, end),
            args: args
                .into_iter()
                .map(|(s, e, kw)| crate::lang::Arg {
                    start: Pos::new(1, s),
                    end: Pos::new(1, e),
                    keyword: match kw {
                        None => crate::lang::ArgKeyword::Positional,
                        Some(k) => crate::lang::ArgKeyword::Named(k.into()),
                    },
                })
                .collect(),
        };
        let (mut a, mut b) = (
            analysis("fetch(1, 2)\n"),
            analysis("fetch(1, 2, timeout=5)\n"),
        );
        a.calls = vec![call("fetch", vec![(6, 7, None), (9, 10, None)], 11)];
        b.calls = vec![call(
            "fetch",
            vec![(6, 7, None), (9, 10, None), (12, 21, Some("timeout"))],
            22,
        )];
        let cls = classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(
            sigs(&cls),
            vec![with_kind(
                "args",
                t("fetch(…)", "fetch(…, timeout=…)", "call")
            )]
        );
        assert_eq!(cls.signatures[0].key, "args\0fetch\0+kw:timeout");
        assert_eq!(cls.generic_tokens, 0);
        assert_eq!(
            cls.new_hl.get(&1),
            Some(&vec![[10, 11], [12, 19], [19, 20], [20, 21]])
        );
    }
}
