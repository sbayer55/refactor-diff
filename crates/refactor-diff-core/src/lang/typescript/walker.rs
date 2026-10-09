//! The token walk: every leaf of the tree becomes a token, logical line ends become
//! `<NEWLINE>` structural tokens, and `type_annotation` nodes are recorded as annotations.

use tree_sitter::{Node, Point};

use super::{children, named_children, node_text, slice_text};
use crate::lang::pyrepr::py_repr_str;
use crate::lang::{Annotation, Token, TokenKind, collapse_ws, pos_of};
use crate::text::Pos;

/// Nodes tokenized whole: their insides are text, not code.
const LEAVES: &[&str] = &[
    "string",
    "regex",
    "comment",
    "html_comment",
    "hash_bang_line",
    "jsx_text",
];

/// Nodes whose children are statements or members: each child ends a logical line.
const BODIES: &[&str] = &[
    "program",
    "statement_block",
    "class_body",
    "interface_body",
    "object_type",
    "enum_body",
    "switch_body",
    "switch_case",
    "switch_default",
];

const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function_signature",
    "generator_function_declaration",
    "method_definition",
    "method_signature",
    "abstract_method_signature",
];

const NEWLINE: &str = "<NEWLINE>";

/// Tokens and annotations for the whole tree.
pub(super) fn walk(
    root: Node<'_>,
    source: &str,
    lines: &[String],
) -> (Vec<Token>, Vec<Annotation>) {
    let mut walker = Walker {
        source,
        lines,
        tokens: Vec::new(),
        annotations: Vec::new(),
    };
    walker.visit(root);
    (walker.tokens, walker.annotations)
}

struct Walker<'a> {
    source: &'a str,
    lines: &'a [String],
    tokens: Vec<Token>,
    annotations: Vec<Annotation>,
}

impl Walker<'_> {
    fn pos(&self, point: Point) -> Pos {
        pos_of(self.lines, point)
    }

    fn visit(&mut self, node: Node<'_>) {
        if node.kind() == "type_annotation" {
            self.annotation(node);
        }
        if node.child_count() == 0 || LEAVES.contains(&node.kind()) || plain_template(node) {
            self.leaf(node);
            return;
        }
        let body = BODIES.contains(&node.kind());
        for child in children(node) {
            self.visit(child);
            if body && child.is_named() {
                self.end_statement(child);
            }
        }
    }

    /// Mark a logical line end. A terminating `;` folds into it, so adding or dropping
    /// optional semicolons reads as formatting.
    fn end_statement(&mut self, child: Node<'_>) {
        let end = self.pos(child.end_position());
        if self
            .tokens
            .last()
            .is_some_and(|t| t.kind == TokenKind::Op && t.value == ";" && t.end == end)
        {
            self.tokens.pop();
        }
        if self
            .tokens
            .last()
            .is_some_and(|t| t.kind != TokenKind::Structural)
        {
            self.tokens.push(Token::structural(NEWLINE, end));
        }
    }

    fn leaf(&mut self, node: Node<'_>) {
        let text = node_text(self.source, node);
        if text.is_empty() || node.is_missing() {
            return;
        }
        if node.kind() == ";" && node.parent().is_some_and(|p| BODIES.contains(&p.kind())) {
            return; // an empty statement or a class member separator
        }
        let kind = kind_of(node, &text);
        let value = match kind {
            TokenKind::Comment => text.trim_end().to_string(),
            TokenKind::String => normalize_string(node.kind(), &text),
            _ => text.to_string(),
        };
        let start = self.pos(node.start_position());
        let end = self.pos(node.end_position());
        self.tokens
            .push(Token::new(kind, value, text.into_owned(), start, end));
    }

    fn annotation(&mut self, node: Node<'_>) {
        let types: Vec<Node<'_>> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() != "comment")
            .collect();
        let (Some(first), Some(last)) = (types.first(), types.last()) else {
            return;
        };
        let Some(target) = annotation_target(self.source, node) else {
            return;
        };
        let text = collapse_ws(&slice_text(
            self.source,
            first.start_byte(),
            last.end_byte(),
        ));
        self.annotations.push(Annotation {
            start: self.pos(first.start_position()),
            end: self.pos(last.end_position()),
            text,
            target,
        });
    }
}

fn kind_of(node: Node<'_>, text: &str) -> TokenKind {
    match node.kind() {
        "comment" | "html_comment" | "hash_bang_line" => TokenKind::Comment,
        "string" | "template_string" | "regex" | "string_fragment" | "jsx_text" => {
            TokenKind::String
        }
        "number" if node.is_named() => TokenKind::Number,
        _ if is_identifier(text) => TokenKind::Name,
        _ => TokenKind::Op,
    }
}

/// `[A-Za-z_$][\w$]*`: an ASCII-led identifier (keywords included).
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// A template literal without `${}` is just a string.
fn plain_template(node: Node<'_>) -> bool {
    node.kind() == "template_string"
        && !children(node)
            .iter()
            .any(|c| c.kind() == "template_substitution")
}

/// Treat `'x'`, `"x"` and `` `x` `` as the same token so quote-style churn reads as
/// formatting: the body is unescaped and rendered as CPython's `repr()` would.
pub(super) fn normalize_string(node_type: &str, text: &str) -> String {
    if !matches!(node_type, "string" | "template_string") {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 2 {
        return text.to_string();
    }
    let mut body: String = chars[1..chars.len() - 1].iter().collect();
    if node_type == "template_string" {
        body = body.replace("\\`", "`");
    }
    match unescape(&body) {
        Some(value) => py_repr_str(&value),
        None => text.to_string(),
    }
}

/// Resolve `\u{...}`, `\uXXXX`, `\xXX`, the single-letter escapes and line continuations;
/// any other escaped character stands for itself. `None` when a code point is not a
/// character (Python raises there and the raw text is kept).
fn unescape(body: &str) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let esc = chars[i + 1];
        if esc == 'u' && chars.get(i + 2) == Some(&'{') {
            let digits = chars[i + 3..]
                .iter()
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if digits > 0 && chars.get(i + 3 + digits) == Some(&'}') {
                let hex: String = chars[i + 3..i + 3 + digits].iter().collect();
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
                i += 4 + digits;
                continue;
            }
        }
        let fixed = match esc {
            'u' => 4,
            'x' => 2,
            _ => 0,
        };
        if fixed > 0 {
            let hex = &chars[(i + 2).min(chars.len())..(i + 2 + fixed).min(chars.len())];
            if hex.len() == fixed && hex.iter().all(|c| c.is_ascii_hexdigit()) {
                let hex: String = hex.iter().collect();
                let code = u32::from_str_radix(&hex, 16).ok()?;
                out.push(char::from_u32(code)?);
                i += 2 + fixed;
                continue;
            }
        }
        match esc {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '\n' => {}
            other => out.push(other),
        }
        i += 2;
    }
    Some(out)
}

fn annotation_target(source: &str, node: Node<'_>) -> Option<String> {
    let parent = node.parent()?;
    let label = parent
        .child_by_field_name("name")
        .map(|n| node_text(source, n))
        .filter(|t| !t.is_empty())
        .map_or_else(|| "?".to_string(), |t| t.into_owned());
    if matches!(parent.kind(), "required_parameter" | "optional_parameter") {
        let pattern = parent
            .child_by_field_name("pattern")
            .map(|p| node_text(source, p))
            .filter(|t| !t.is_empty());
        return Some(format!(
            "param {}",
            pattern.as_deref().unwrap_or(label.as_str())
        ));
    }
    if parent.child_by_field_name("return_type") == Some(node) {
        if parent.kind() == "arrow_function" {
            if let Some(holder) = parent
                .parent()
                .filter(|h| h.kind() == "variable_declarator")
            {
                if let Some(held) = holder.child_by_field_name("name") {
                    let held = node_text(source, held);
                    if !held.is_empty() {
                        return Some(format!("return of {held}"));
                    }
                }
            }
            return Some("return of arrow function".to_string());
        }
        if FUNCTIONS.contains(&parent.kind()) {
            return Some(format!("return of {label}"));
        }
        return None;
    }
    match parent.kind() {
        "variable_declarator" => Some(format!("variable {label}")),
        "public_field_definition" | "property_signature" => Some(format!("property {label}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_shape() {
        assert!(is_identifier("getUser"));
        assert!(is_identifier("$x"));
        assert!(is_identifier("_a1"));
        assert!(is_identifier("a日"));
        assert!(!is_identifier("日本"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("a-b"));
    }

    #[test]
    fn string_normalization() {
        assert_eq!(normalize_string("string", "'hi'"), "'hi'");
        assert_eq!(normalize_string("string", "\"hi\""), "'hi'");
        assert_eq!(normalize_string("template_string", "`hi`"), "'hi'");
        assert_eq!(normalize_string("template_string", "`a\\`b`"), "'a`b'");
        assert_eq!(normalize_string("string", "'it\\'s'"), "\"it's\"");
        assert_eq!(
            normalize_string("string", "'\\n\\t\\x41\\u0042\\u{1F600}\\q\\\\\\\"'"),
            "'\\n\\tAB😀q\\\\\"'"
        );
        assert_eq!(normalize_string("string", "'a\\\nb'"), "'ab'");
        assert_eq!(
            normalize_string("string", "'\\r\\0\\b\\f\\v'"),
            "'\\r\\x00\\x08\\x0c\\x0b'"
        );
        assert_eq!(
            normalize_string("string", "\"\\u{}\\u12\\x4\""),
            "'u{}u12x4'"
        );
        assert_eq!(normalize_string("string", "'x\\'"), "'x\\\\'");
        // Not a character: the raw text is kept.
        assert_eq!(normalize_string("string", "'\\u{110000}'"), "'\\u{110000}'");
        // Other node types and too-short texts pass through.
        assert_eq!(normalize_string("regex", "/a/"), "/a/");
        assert_eq!(normalize_string("string", "'"), "'");
    }
}
