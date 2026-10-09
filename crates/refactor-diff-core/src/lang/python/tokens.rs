//! The token stream: CST leaves rendered the way CPython's `tokenize` reports them, plus
//! the zero-width `<NEWLINE>` / `<INDENT>` / `<DEDENT>` structural tokens it synthesizes.

use std::collections::HashSet;

use tree_sitter::{Node, Point, Tree};
use unicode_general_category::{GeneralCategory, get_general_category};

use super::super::pyrepr::{eval_python_string, py_repr};
use super::super::{Token, TokenKind, pos_of};
use super::{is_format_string, text};
use crate::text::{Pos, char_len};

pub(super) struct Scan {
    pub tokens: Vec<Token>,
    /// A dedent landed on a column that is not on the indentation stack (CPython raises
    /// `IndentationError: unindent does not match any outer indentation level`).
    pub indent_error: bool,
}

pub(super) fn tokenize(tree: &Tree, src: &str, lines: &[String]) -> Scan {
    let mut walker = Walker {
        src,
        lines,
        out: Vec::new(),
        continuations: HashSet::new(),
    };
    walker.walk(tree.root_node());
    structural(walker.out, &walker.continuations, lines)
}

struct Walker<'a> {
    src: &'a str,
    lines: &'a [String],
    out: Vec<Token>,
    /// 1-based lines ending in a backslash continuation.
    continuations: HashSet<u32>,
}

impl Walker<'_> {
    fn pos(&self, p: Point) -> Pos {
        pos_of(self.lines, p)
    }

    fn push(&mut self, kind: TokenKind, value: &str, text: &str, start: Point, end: Point) {
        self.out.push(Token::new(
            kind,
            value,
            text,
            self.pos(start),
            self.pos(end),
        ));
    }

    fn push_node(&mut self, kind: TokenKind, value: &str, node: Node<'_>) {
        let t = text(self.src, node);
        self.push(kind, value, t, node.start_position(), node.end_position());
    }

    /// The point at `byte`, which lies inside `base`.
    fn point_at(&self, base: Node<'_>, byte: usize) -> Point {
        let start = base.start_byte();
        let sp = base.start_position();
        let seg = &self.src[start..byte];
        match seg.rfind('\n') {
            None => Point::new(sp.row, sp.column + (byte - start)),
            Some(i) => Point::new(sp.row + seg.matches('\n').count(), byte - (start + i + 1)),
        }
    }

    fn walk(&mut self, node: Node<'_>) {
        match node.kind() {
            "comment" => {
                // A comment is one line; CRLF files leave the `\r` inside the node.
                let t = text(self.src, node).trim_end_matches('\r');
                let start = node.start_position();
                let end = Point::new(start.row, start.column + t.len());
                self.push(TokenKind::Comment, t.trim_end(), t, start, end);
            }
            "line_continuation" => {
                self.continuations
                    .insert(node.start_position().row as u32 + 1);
            }
            "string" => self.string(node),
            "import_prefix" => self.import_prefix(node),
            _ if node.child_count() == 0 => self.leaf(node),
            _ => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    self.walk(child);
                }
            }
        }
    }

    fn leaf(&mut self, node: Node<'_>) {
        if node.is_missing() {
            return;
        }
        let t = text(self.src, node);
        if t.is_empty() {
            return;
        }
        let kind = match node.kind() {
            "integer" | "float" => TokenKind::Number,
            "string_start" | "string_content" | "string_end" => TokenKind::Other,
            "identifier" => TokenKind::Name,
            _ if is_identifier(t) => TokenKind::Name,
            _ => TokenKind::Op,
        };
        self.push_node(kind, t, node);
    }

    /// The dots of `from ... import x`: tree-sitter keeps one leaf per dot, CPython's lexer
    /// greedily takes three adjacent dots as one `...` token (`....` is `...` then `.`).
    fn import_prefix(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        let dots: Vec<Node<'_>> = node.children(&mut cursor).collect();
        let mut i = 0;
        while i < dots.len() {
            let mut run = 1;
            while run < 3
                && i + run < dots.len()
                && dots[i + run].start_byte() == dots[i + run - 1].end_byte()
                && dots[i + run].kind() == "."
                && dots[i].kind() == "."
            {
                run += 1;
            }
            if run == 3 {
                let (first, last) = (dots[i], dots[i + 2]);
                self.push(
                    TokenKind::Op,
                    "...",
                    "...",
                    first.start_position(),
                    last.end_position(),
                );
                i += 3;
            } else {
                self.walk(dots[i]);
                i += 1;
            }
        }
    }

    /// A `string` node: one STRING token for a plain literal, or the FSTRING_START /
    /// FSTRING_MIDDLE / FSTRING_END pieces CPython ≥ 3.12 emits for f- and t-strings.
    fn string(&mut self, node: Node<'_>) {
        let t = text(self.src, node);
        if !is_format_string(self.src, node) {
            let value = eval_python_string(t)
                .map(|lit| py_repr(&lit))
                .unwrap_or_else(|| t.to_string());
            self.push(
                TokenKind::String,
                &value,
                t,
                node.start_position(),
                node.end_position(),
            );
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "string_start" | "string_end" => {
                    self.push_node(TokenKind::Other, text(self.src, child), child)
                }
                "string_content" => self.content(child),
                "interpolation" => self.interpolation(child),
                _ => self.walk(child),
            }
        }
    }

    /// Literal text between interpolations. CPython ends a FSTRING_MIDDLE after the first
    /// brace of a doubled `{{` / `}}` and resumes after the second one.
    fn content(&mut self, node: Node<'_>) {
        let mut cur = node.start_byte();
        let mut cursor = node.walk();
        for esc in node.named_children(&mut cursor) {
            if esc.kind() != "escape_interpolation" {
                continue;
            }
            self.piece(node, cur, esc.start_byte() + 1);
            cur = esc.end_byte();
        }
        if cur < node.end_byte() {
            self.piece(node, cur, node.end_byte());
        }
    }

    fn piece(&mut self, base: Node<'_>, start: usize, end: usize) {
        let t = &self.src[start..end];
        let (s, e) = (self.point_at(base, start), self.point_at(base, end));
        self.push(TokenKind::Other, t, t, s, e);
    }

    fn interpolation(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "type_conversion" => {
                    // `!r` is OP `!` followed by NAME `r`.
                    let t = text(self.src, child);
                    let s = child.start_position();
                    let mid = Point::new(s.row, s.column + 1);
                    self.push(TokenKind::Op, "!", "!", s, mid);
                    self.push(TokenKind::Name, &t[1..], &t[1..], mid, child.end_position());
                }
                "format_specifier" => self.format_spec(child),
                _ => self.walk(child),
            }
        }
    }

    /// `:spec` where the literal pieces of the spec are not CST nodes (only the nested
    /// `format_expression`s are): they are the gaps between the children. CPython emits a
    /// FSTRING_MIDDLE for the text before the closing brace even when it is empty.
    fn format_spec(&mut self, node: Node<'_>) {
        let mut cur = node.start_byte();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                ":" => {
                    self.leaf(child);
                    cur = child.end_byte();
                }
                "format_expression" => {
                    if cur < child.start_byte() {
                        self.piece(node, cur, child.start_byte());
                    }
                    self.interpolation(child);
                    cur = child.end_byte();
                }
                _ => self.walk(child),
            }
        }
        self.piece(node, cur, node.end_byte());
    }
}

/// `[_\p{XID_Start}][_\p{XID_Continue}]*` (approximated with the Alphabetic/Numeric
/// properties plus combining and connector marks).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| {
        c == '_'
            || c.is_alphanumeric()
            || matches!(
                get_general_category(c),
                GeneralCategory::NonspacingMark
                    | GeneralCategory::SpacingMark
                    | GeneralCategory::ConnectorPunctuation
            )
    })
}

/// CPython's indentation width of a line: tabs advance to the next multiple of 8, a form
/// feed resets the column.
fn indent_width(line: &str) -> u32 {
    let mut col = 0u32;
    for c in line.chars() {
        match c {
            ' ' => col += 1,
            '\t' => col = (col / 8 + 1) * 8,
            '\x0c' => col = 0,
            _ => break,
        }
    }
    col
}

/// Interleave NEWLINE / INDENT / DEDENT the way `tokenize` does: a NEWLINE closes every
/// logical line (at the end of its last physical line, after any trailing comment), INDENT
/// sits at column 0 of a block's first line, DEDENTs sit at the first token of the statement
/// that closes the block (or at `(last line + 1, 0)` at EOF). Comment-only lines take no part
/// in indentation. CPython's NL tokens are dropped.
fn structural(toks: Vec<Token>, continuations: &HashSet<u32>, lines: &[String]) -> Scan {
    let mut out = Vec::with_capacity(toks.len() + toks.len() / 4);
    let mut depth = 0i32;
    let mut indents = vec![0u32];
    let mut in_line = false;
    let mut last_line = 0u32;
    let mut indent_error = false;
    let line_end = |line: u32| {
        let len = lines
            .get(line as usize - 1)
            .map(|l| char_len(l))
            .unwrap_or(0);
        Pos::new(line, len)
    };

    for tok in toks {
        let ends_line = in_line
            && depth == 0
            && tok.start.line > last_line
            && !(last_line..tok.start.line).all(|l| continuations.contains(&l));
        if ends_line {
            out.push(Token::structural("<NEWLINE>", line_end(last_line)));
            in_line = false;
        }
        if tok.kind == TokenKind::Comment {
            out.push(tok);
            continue;
        }
        if !in_line {
            let col = lines
                .get(tok.start.line as usize - 1)
                .map(|l| indent_width(l))
                .unwrap_or(0);
            let top = *indents.last().unwrap_or(&0);
            if col > top {
                indents.push(col);
                out.push(Token::structural("<INDENT>", Pos::new(tok.start.line, 0)));
            } else {
                while indents.len() > 1 && col < *indents.last().unwrap_or(&0) {
                    indents.pop();
                    out.push(Token::structural("<DEDENT>", tok.start));
                }
                if col != *indents.last().unwrap_or(&0) {
                    indent_error = true;
                }
            }
            in_line = true;
        }
        if tok.kind == TokenKind::Op {
            match tok.value.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = (depth - 1).max(0),
                _ => {}
            }
        }
        last_line = tok.end.line;
        out.push(tok);
    }
    if in_line {
        out.push(Token::structural("<NEWLINE>", line_end(last_line)));
    }
    let eof = Pos::new(lines.len() as u32 + 1, 0);
    for _ in 1..indents.len() {
        out.push(Token::structural("<DEDENT>", eof));
    }
    Scan {
        tokens: out,
        indent_error,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::LanguageAnalyzer;
    use super::super::PythonAnalyzer;
    use super::*;

    /// `kind value @l.c-l.c` per token, as CPython's tokenize module reported them (see the
    /// probe cases in the port notes); NL and ENDMARKER omitted.
    fn stream(src: &str) -> Vec<String> {
        PythonAnalyzer::new()
            .analyze(src)
            .tokens
            .iter()
            .map(|t| {
                format!(
                    "{} {:?} @{}.{}-{}.{}",
                    t.kind.as_str(),
                    t.value,
                    t.start.line,
                    t.start.col,
                    t.end.line,
                    t.end.col
                )
            })
            .collect()
    }

    #[test]
    fn newline_sits_after_trailing_whitespace_and_comments() {
        assert_eq!(
            stream("x = 1   \n"),
            [
                "name \"x\" @1.0-1.1",
                "op \"=\" @1.2-1.3",
                "number \"1\" @1.4-1.5",
                "structural \"<NEWLINE>\" @1.8-1.8"
            ]
        );
        assert_eq!(
            stream("x = 1;  # c   \n")[3..],
            [
                "op \";\" @1.5-1.6",
                "comment \"# c\" @1.8-1.14",
                "structural \"<NEWLINE>\" @1.14-1.14"
            ]
        );
    }

    #[test]
    fn eof_dedent_counts_trailing_blank_lines() {
        let s = stream("if x:\n    y\n\n\n");
        assert_eq!(s.last().unwrap(), "structural \"<DEDENT>\" @5.0-5.0");
        let s = stream("if x:\n    y");
        assert_eq!(
            &s[s.len() - 2..],
            [
                "structural \"<NEWLINE>\" @2.5-2.5",
                "structural \"<DEDENT>\" @3.0-3.0"
            ]
        );
    }

    #[test]
    fn comments_do_not_take_part_in_indentation() {
        let s = stream("def f():\n    return 1\n    # trailing\n# col0\nx = 2\n");
        let tail: Vec<&str> = s[9..].iter().map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "structural \"<NEWLINE>\" @2.12-2.12",
                "comment \"# trailing\" @3.4-3.14",
                "comment \"# col0\" @4.0-4.6",
                "structural \"<DEDENT>\" @5.0-5.0",
                "name \"x\" @5.0-5.1",
                "op \"=\" @5.2-5.3",
                "number \"2\" @5.4-5.5",
                "structural \"<NEWLINE>\" @5.5-5.5",
            ]
        );
    }

    #[test]
    fn dedent_position_with_tabs_and_formfeed() {
        let s = stream("if x:\n\tif y:\n\t\tz\n\tw\n");
        assert!(s.contains(&"structural \"<INDENT>\" @3.0-3.0".to_string()));
        assert!(s.contains(&"structural \"<DEDENT>\" @4.1-4.1".to_string()));
        let s = stream("if x:\n    y\n\x0cz\n");
        assert!(s.contains(&"structural \"<DEDENT>\" @3.1-3.1".to_string()));
    }

    #[test]
    fn backslash_continuation_joins_lines() {
        let s = stream("x = 1 + \\\n    2\ny = 3\n");
        assert_eq!(s[4], "number \"2\" @2.4-2.5");
        assert_eq!(s[5], "structural \"<NEWLINE>\" @2.5-2.5");
        assert_eq!(s.iter().filter(|t| t.contains("<NEWLINE>")).count(), 2);
    }

    #[test]
    fn multiline_string_does_not_start_a_logical_line() {
        let s = stream("x = \"\"\"abc\ndef\"\"\" + 1\ny\n");
        assert_eq!(s[2], "string \"'abc\\\\ndef'\" @1.4-2.6");
        assert_eq!(s[3], "op \"+\" @2.7-2.8");
        assert!(!s.iter().any(|t| t.contains("<INDENT>")));
    }

    #[test]
    fn inconsistent_dedent_is_not_parsed() {
        let an = PythonAnalyzer::new().analyze("if a:\n        b\n    c\n");
        assert!(!an.parsed);
    }

    #[test]
    fn fstring_doubled_braces_split_the_middle() {
        assert_eq!(
            stream("f\"a{{b}}c{x}\"\n"),
            [
                "other \"f\\\"\" @1.0-1.2",
                "other \"a{\" @1.2-1.4",
                "other \"b}\" @1.5-1.7",
                "other \"c\" @1.8-1.9",
                "op \"{\" @1.9-1.10",
                "name \"x\" @1.10-1.11",
                "op \"}\" @1.11-1.12",
                "other \"\\\"\" @1.12-1.13",
                "structural \"<NEWLINE>\" @1.13-1.13",
            ]
        );
    }

    #[test]
    fn fstring_format_spec_pieces() {
        // Empty spec, and a spec that ends with a nested field: an empty FSTRING_MIDDLE.
        let s = stream("f\"{x:}\"\n");
        assert_eq!(
            s[3..6],
            [
                "op \":\" @1.4-1.5",
                "other \"\" @1.5-1.5",
                "op \"}\" @1.5-1.6"
            ]
        );
        let s = stream("f\"{x:{a}{b}}\"\n");
        assert_eq!(
            s[9..12],
            [
                "op \"}\" @1.10-1.11",
                "other \"\" @1.11-1.11",
                "op \"}\" @1.11-1.12"
            ]
        );
        // Literal text after a nested field is kept; none is inserted before it.
        let s = stream("f\"{x:{a}abc}\"\n");
        assert_eq!(s[7], "other \"abc\" @1.8-1.11");
        let s = stream("f\"{x:abc{a}}\"\n");
        assert_eq!(s[4], "other \"abc\" @1.5-1.8");
        assert_eq!(s[8], "other \"\" @1.11-1.11");
    }

    #[test]
    fn nested_fstring_and_escapes() {
        let s = stream("f\"{f\"{x}\"}\"\n");
        assert_eq!(s[2], "other \"f\\\"\" @1.3-1.5");
        assert_eq!(s[6], "other \"\\\"\" @1.8-1.9");
        let s = stream("f\"a\\n{x}\\t\"\n");
        assert_eq!(s[1], "other \"a\\\\n\" @1.2-1.5");
        assert_eq!(s[5], "other \"\\\\t\" @1.8-1.10");
    }

    #[test]
    fn not_in_and_is_not_are_separate_names() {
        let s = stream("a not  in b\n");
        assert_eq!(s[1], "name \"not\" @1.2-1.5");
        assert_eq!(s[2], "name \"in\" @1.7-1.9");
    }

    #[test]
    fn relative_import_dots_lex_like_cpython() {
        let s = stream("from .. import a\nfrom ....b import c\nfrom . . import d\n");
        assert_eq!(s[1], "op \".\" @1.5-1.6");
        assert_eq!(s[2], "op \".\" @1.6-1.7");
        assert_eq!(s[7], "op \"...\" @2.5-2.8");
        assert_eq!(s[8], "op \".\" @2.8-2.9");
        assert_eq!(s[9], "name \"b\" @2.9-2.10");
        assert_eq!(s[14], "op \".\" @3.5-3.6");
        assert_eq!(s[15], "op \".\" @3.7-3.8");
    }

    #[test]
    fn crlf_comment_excludes_the_carriage_return() {
        let s = stream("x = 1  # c\r\ny = 2\r\n");
        assert_eq!(s[3], "comment \"# c\" @1.7-1.10");
        assert_eq!(s[4], "structural \"<NEWLINE>\" @1.10-1.10");
    }

    #[test]
    fn identifier_shape() {
        assert!(is_identifier("_") && is_identifier("héllo") && is_identifier("x1"));
        assert!(!is_identifier("1x") && !is_identifier("") && !is_identifier("a-b"));
        assert_eq!(indent_width("\t\tx"), 16);
        assert_eq!(indent_width("  \tx"), 8);
        assert_eq!(indent_width("    x"), 4);
    }
}
