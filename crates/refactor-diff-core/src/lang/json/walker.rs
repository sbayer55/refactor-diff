//! The token walk: every leaf becomes a token, strings whole; each object member or array
//! element ends a logical line with a `<NEWLINE>` structural token and its separating comma
//! is dropped.

use tree_sitter::Node;

use crate::lang::pyrepr::py_repr_str;
use crate::lang::{Token, TokenKind, pos_of};

const NEWLINE: &str = "<NEWLINE>";

/// Containers whose named children are members.
const CONTAINERS: &[&str] = &["object", "array", "document"];

pub(super) fn walk(root: Node<'_>, source: &str, lines: &[String]) -> Vec<Token> {
    let mut walker = Walker {
        source,
        lines,
        tokens: Vec::new(),
    };
    walker.visit(root, false);
    walker.tokens
}

struct Walker<'a> {
    source: &'a str,
    lines: &'a [String],
    tokens: Vec<Token>,
}

impl Walker<'_> {
    /// `in_container`: the node's parent is an object, array or the document (tree-sitter's
    /// `parent()` walks down from the root, so it is passed along instead).
    fn visit(&mut self, node: Node<'_>, in_container: bool) {
        if node.child_count() == 0 || node.kind() == "string" {
            self.leaf(node, in_container);
            return;
        }
        let container = CONTAINERS.contains(&node.kind());
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for child in children {
            self.visit(child, container);
            if container && child.is_named() && child.kind() != "comment" {
                self.end_member(child);
            }
        }
    }

    fn end_member(&mut self, child: Node<'_>) {
        if self
            .tokens
            .last()
            .is_some_and(|t| t.kind != TokenKind::Structural)
        {
            let end = pos_of(self.lines, child.end_position());
            self.tokens.push(Token::structural(NEWLINE, end));
        }
    }

    fn leaf(&mut self, node: Node<'_>, in_container: bool) {
        if node.is_missing() || node.start_byte() == node.end_byte() {
            return;
        }
        if node.kind() == "," && in_container {
            return; // a member separator: folded into the member's <NEWLINE>
        }
        let Some(text) = self.source.get(node.start_byte()..node.end_byte()) else {
            return;
        };
        let (kind, value) = match node.kind() {
            "string" => (TokenKind::String, normalize_string(text)),
            "number" => (TokenKind::Number, text.to_string()),
            "true" | "false" | "null" => (TokenKind::Name, text.to_string()),
            "comment" => (TokenKind::Comment, text.trim_end().to_string()),
            _ => (TokenKind::Op, text.to_string()),
        };
        let start = pos_of(self.lines, node.start_position());
        let end = pos_of(self.lines, node.end_position());
        self.tokens.push(Token::new(kind, value, text, start, end));
    }
}

/// A JSON string as CPython's `repr()` of its value, so it compares like a TypeScript string
/// token; the raw text when an escape is invalid.
pub(super) fn normalize_string(text: &str) -> String {
    super::string_value(text)
        .map(|v| py_repr_str(&v))
        .unwrap_or_else(|| text.to_string())
}

/// Resolve JSON escapes (`\"`, `\\`, `\/`, `\b`, `\f`, `\n`, `\r`, `\t`, `\uXXXX` with
/// surrogate pairs). `None` on an invalid escape.
pub(super) fn unescape(body: &str) -> Option<String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let hi = hex4(&mut chars)?;
                let code = if (0xD800..0xDC00).contains(&hi) {
                    if chars.next()? != '\\' || chars.next()? != 'u' {
                        return None;
                    }
                    let lo = hex4(&mut chars)?;
                    if !(0xDC00..0xE000).contains(&lo) {
                        return None;
                    }
                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                } else {
                    hi
                };
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn hex4(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let mut v = 0;
    for _ in 0..4 {
        v = v * 16 + chars.next()?.to_digit(16)?;
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_normalize_by_value() {
        assert_eq!(normalize_string("\"hi\""), "'hi'");
        assert_eq!(normalize_string("\"it's\""), "\"it's\"");
        assert_eq!(normalize_string("\"a\\/b\\n\""), "'a/b\\n'");
        assert_eq!(normalize_string("\"\\u0041\\ud83d\\ude00\""), "'A😀'");
        // Invalid escapes keep the raw text.
        assert_eq!(normalize_string("\"\\q\""), "\"\\q\"");
        assert_eq!(normalize_string("\"\\ud83d\""), "\"\\ud83d\"");
    }
}
