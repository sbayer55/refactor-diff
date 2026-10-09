//! TypeScript / TSX / JavaScript analyzers on tree-sitter.
//!
//! One analyzer per grammar (TypeScript, TSX, JavaScript); they share keywords, builtins and the
//! pathspecs used to look for missed renames, so a renamed TS export is also searched for in JS.
//!
//! The walker ([`walker`]) produces tokens and annotations; [`structure`] produces statement
//! spans and import sites. Neither calls nor defs are extracted, and there is no AST verifier.

mod structure;
mod walker;

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;

use tree_sitter::{Language, Node};

use super::{FileAnalysis, LanguageAnalyzer};
use crate::text::split_lines;

pub const TS_SUFFIXES: &[&str] = &[".ts", ".mts", ".cts"];
pub const TSX_SUFFIXES: &[&str] = &[".tsx"];
pub const JS_SUFFIXES: &[&str] = &[".js", ".jsx", ".mjs", ".cjs"];
/// Git pathspecs for every file this language family analyzes (TS, then TSX, then JS).
pub const GLOBS: &[&str] = &[
    "*.ts", "*.mts", "*.cts", "*.tsx", "*.js", "*.jsx", "*.mjs", "*.cjs",
];

const KEYWORD_LIST: &str = "
    abstract any as asserts async await bigint boolean break case catch class const constructor
    continue debugger declare default delete do else enum export extends false finally for from
    function get if implements import in infer instanceof interface is keyof let module namespace
    never new null number object of override private protected public readonly require return
    satisfies set static string super switch symbol this throw true try type typeof undefined
    unique unknown var void while with yield
";

const BUILTIN_LIST: &str = "
    Array ArrayBuffer AsyncGenerator BigInt Boolean DataView Date Error EvalError Float32Array
    Float64Array Function Generator Infinity Int8Array Int16Array Int32Array Intl JSON Map Math
    NaN Number Object Promise Proxy RangeError ReferenceError Reflect RegExp Set String Symbol
    SyntaxError TypeError URIError Uint8Array Uint8ClampedArray Uint16Array Uint32Array WeakMap
    WeakRef WeakSet globalThis console decodeURI decodeURIComponent encodeURI encodeURIComponent
    eval isFinite isNaN parseFloat parseInt queueMicrotask setInterval setTimeout clearInterval
    clearTimeout structuredClone fetch URL URLSearchParams window document process module exports
    require __dirname __filename
    Awaited Capitalize ConstructorParameters Exclude Extract InstanceType Lowercase NonNullable
    Omit OmitThisParameter Parameters Partial Pick Readonly ReadonlyArray Record Required
    ReturnType ThisParameterType ThisType Uncapitalize Uppercase PromiseLike Iterable Iterator
    AsyncIterable IterableIterator
";

static KEYWORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| KEYWORD_LIST.split_whitespace().collect());
static BUILTINS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| BUILTIN_LIST.split_whitespace().collect());

/// Keywords shared by the three dialects (a rename to or from one is never a rename).
pub fn is_keyword(value: &str) -> bool {
    KEYWORDS.contains(value)
}

/// Names always available without a definition (renaming them away is never "missed").
pub fn is_builtin(value: &str) -> bool {
    BUILTINS.contains(value)
}

pub struct TypeScriptAnalyzer {
    name: &'static str,
    language: Language,
    suffixes: &'static [&'static str],
}

impl TypeScriptAnalyzer {
    pub fn typescript() -> Self {
        Self {
            name: "typescript",
            language: Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            suffixes: TS_SUFFIXES,
        }
    }

    pub fn tsx() -> Self {
        Self {
            name: "tsx",
            language: Language::new(tree_sitter_typescript::LANGUAGE_TSX),
            suffixes: TSX_SUFFIXES,
        }
    }

    pub fn javascript() -> Self {
        Self {
            name: "javascript",
            language: Language::new(tree_sitter_javascript::LANGUAGE),
            suffixes: JS_SUFFIXES,
        }
    }
}

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn name(&self) -> &'static str {
        self.name
    }

    fn globs(&self) -> &'static [&'static str] {
        GLOBS
    }

    fn handles(&self, path: &str) -> bool {
        self.suffixes.iter().any(|s| path.ends_with(s))
    }

    fn analyze(&self, text: &str) -> FileAnalysis {
        let lines = split_lines(text);
        let Some(tree) = super::parse(&self.language, text) else {
            return FileAnalysis::unparsed(lines, vec![]);
        };
        let root = tree.root_node();
        let (tokens, annotations) = walker::walk(root, text, &lines);
        FileAnalysis {
            parsed: !root.has_error(),
            statements: structure::statements(root, text),
            imports: structure::imports(root, text),
            tokens,
            annotations,
            lines,
            ..Default::default()
        }
    }

    fn is_keyword(&self, value: &str) -> bool {
        is_keyword(value)
    }

    fn is_builtin(&self, value: &str) -> bool {
        is_builtin(value)
    }

    fn import_keywords(&self) -> &'static [&'static str] {
        &["import"]
    }

    fn definition_keywords(&self) -> &'static [&'static str] {
        &[
            "function",
            "class",
            "interface",
            "type",
            "enum",
            "namespace",
            "const",
            "let",
            "var",
        ]
    }
}

/// The source text a node spans. Byte ranges tree-sitter reports always sit on character
/// boundaries for valid UTF-8 input; should one not, the bytes decode with replacement
/// characters, as Python's `decode(errors="replace")` does.
pub(super) fn node_text<'a>(source: &'a str, node: Node<'_>) -> Cow<'a, str> {
    slice_text(source, node.start_byte(), node.end_byte())
}

/// `source[start..end]`, decoded with replacement characters when the cut is not on a
/// character boundary.
pub(super) fn slice_text(source: &str, start: usize, end: usize) -> Cow<'_, str> {
    let end = end.min(source.len());
    let start = start.min(end);
    match source.get(start..end) {
        Some(text) => Cow::Borrowed(text),
        None => String::from_utf8_lossy(&source.as_bytes()[start..end]),
    }
}

/// The text of a node's named field, or `""` when the field is absent.
pub(super) fn field_text(source: &str, node: Node<'_>, field: &str) -> String {
    node.child_by_field_name(field)
        .map(|n| node_text(source, n).into_owned())
        .unwrap_or_default()
}

/// A node's children, collected so the walk can recurse without holding a cursor borrow.
pub(super) fn children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

/// A node's named children, collected.
pub(super) fn named_children<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}
