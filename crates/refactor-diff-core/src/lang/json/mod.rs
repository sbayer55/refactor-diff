//! JSON analyzer on tree-sitter-json.
//!
//! The walker ([`walker`]) produces tokens: strings (keys included) compare by value like
//! TypeScript strings, separating commas fold into the `<NEWLINE>` that ends each member, so
//! dropping the last member's comma reads as formatting. [`structure`] produces one statement
//! span per object member or array element, nested. There are no imports, calls or defs.
//!
//! The parsed tree is kept in [`FileAnalysis::syntax`] so [`crate::json_links`] can recover the
//! key path of each member with [`members`].

mod structure;
mod walker;

use tree_sitter::{Language, Node};

use super::{FileAnalysis, LanguageAnalyzer, Syntax, Token, TokenKind, pos_of};
use crate::text::{Pos, char_len, split_lines};

pub use structure::{Member, members};

/// Files larger than this are tokenized line by line instead of parsed.
pub const MAX_PARSE_BYTES: usize = 2 * 1024 * 1024;

pub struct JsonAnalyzer {
    language: Language,
}

impl JsonAnalyzer {
    pub fn new() -> Self {
        Self {
            language: Language::new(tree_sitter_json::LANGUAGE),
        }
    }
}

impl Default for JsonAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// `*.json`, except lockfiles (generated, huge and never reviewed by hand).
pub fn handles(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".json") && name != "package-lock.json" && !name.ends_with(".lock.json")
}

impl LanguageAnalyzer for JsonAnalyzer {
    fn name(&self) -> &'static str {
        "json"
    }

    fn globs(&self) -> &'static [&'static str] {
        &["*.json"]
    }

    fn handles(&self, path: &str) -> bool {
        handles(path)
    }

    fn analyze(&self, text: &str) -> FileAnalysis {
        let lines = split_lines(text);
        if text.len() > MAX_PARSE_BYTES {
            let tokens = line_tokens(&lines);
            return FileAnalysis::unparsed(lines, tokens);
        }
        let Some(tree) = super::parse(&self.language, text) else {
            let tokens = line_tokens(&lines);
            return FileAnalysis::unparsed(lines, tokens);
        };
        let root = tree.root_node();
        let parsed = !root.has_error();
        let tokens = walker::walk(root, text, &lines);
        let statements = structure::statements(root);
        FileAnalysis {
            parsed,
            statements,
            tokens,
            syntax: parsed.then(|| Syntax {
                tree,
                source: text.to_string(),
            }),
            lines,
            ..Default::default()
        }
    }

    fn is_keyword(&self, value: &str) -> bool {
        matches!(value, "true" | "false" | "null")
    }

    fn is_builtin(&self, _value: &str) -> bool {
        false
    }

    fn import_keywords(&self) -> &'static [&'static str] {
        &[]
    }

    fn definition_keywords(&self) -> &'static [&'static str] {
        &[]
    }
}

/// One token per non-blank line (its trimmed text), for files too large to parse: changes
/// still classify, just without token-level detail.
fn line_tokens(lines: &[String]) -> Vec<Token> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let n = i as u32 + 1;
            let lead = char_len(l) - char_len(l.trim_start());
            let start = Pos::new(n, lead);
            let end = Pos::new(n, char_len(l.trim_end()));
            Token::new(TokenKind::Other, l.trim(), l.trim(), start, end)
        })
        .collect()
}

/// A node's span as token positions.
pub(crate) fn node_span(lines: &[String], node: Node<'_>) -> (Pos, Pos) {
    (
        pos_of(lines, node.start_position()),
        pos_of(lines, node.end_position()),
    )
}

/// The value of a JSON string node's text (quotes included), escapes resolved. `None` when
/// an escape is invalid.
pub fn string_value(text: &str) -> Option<String> {
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    walker::unescape(body)
}
