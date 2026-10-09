//! Python analyzer on tree-sitter-python.
//!
//! Reproduces what the original implementation derived from CPython's `tokenize` and `ast`
//! modules: the token stream (including the synthetic `<NEWLINE>` / `<INDENT>` / `<DEDENT>`
//! tokens `tokenize` emits), annotations, docstrings, statement spans, call/def/import sites,
//! and a canonical tree dump for "verified by AST".

mod structure;
mod tokens;
mod verify;

use std::collections::HashMap;

use tree_sitter::{Node, Point};

use super::{AstVerifier, FileAnalysis, LanguageAnalyzer, NodeRef, Syntax};

pub struct PythonAnalyzer;

impl PythonAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PythonAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn name(&self) -> &'static str {
        "python"
    }

    fn globs(&self) -> &'static [&'static str] {
        &["*.py", "*.pyi"]
    }

    fn handles(&self, path: &str) -> bool {
        path.ends_with(".py") || path.ends_with(".pyi")
    }

    fn analyze(&self, text: &str) -> FileAnalysis {
        let lines = crate::split_lines(text);
        let Some(tree) = super::parse(&language(), text) else {
            return FileAnalysis::unparsed(lines, vec![]);
        };
        let scan = tokens::tokenize(&tree, text, &lines);
        // CPython rejects a dedent to a column that is not on the indentation stack; tree-sitter
        // happily parses it, so the token scan reports it separately.
        if tree.root_node().has_error() || scan.indent_error {
            return FileAnalysis::unparsed(lines, scan.tokens);
        }
        let syntax = Syntax {
            tree,
            source: text.to_string(),
        };
        let st = structure::extract(&syntax, &lines);
        FileAnalysis {
            lines,
            tokens: scan.tokens,
            annotations: st.annotations,
            docstrings: st.docstrings,
            parsed: true,
            syntax: Some(syntax),
            statements: st.statements,
            calls: st.calls,
            defs: st.defs,
            imports: st.imports,
        }
    }

    fn is_keyword(&self, value: &str) -> bool {
        KEYWORDS.binary_search(&value).is_ok() || SOFT_KEYWORDS.binary_search(&value).is_ok()
    }

    fn is_builtin(&self, value: &str) -> bool {
        BUILTINS.binary_search(&value).is_ok()
    }

    fn import_keywords(&self) -> &'static [&'static str] {
        &["import", "from"]
    }

    fn definition_keywords(&self) -> &'static [&'static str] {
        &["def", "class"]
    }

    fn verifier(&self) -> Option<&dyn AstVerifier> {
        Some(self)
    }
}

impl AstVerifier for PythonAnalyzer {
    fn parse_block(&self, text: &str) -> Option<Syntax> {
        verify::parse_block(text)
    }

    fn normalized_dump(
        &self,
        syntax: &Syntax,
        node: Option<NodeRef>,
        renames: &HashMap<String, String>,
        strip_annotations: bool,
    ) -> String {
        verify::normalized_dump(syntax, node, renames, strip_annotations)
    }
}

pub(super) fn language() -> tree_sitter::Language {
    tree_sitter_python::LANGUAGE.into()
}

/// `keyword.kwlist` (CPython 3.14), sorted.
const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// `keyword.softkwlist` (CPython 3.14), sorted.
const SOFT_KEYWORDS: &[&str] = &["_", "case", "match", "type"];

/// `sorted(dir(builtins))` (CPython 3.14).
const BUILTINS: &[&str] = &[
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BaseException",
    "BaseExceptionGroup",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "BytesWarning",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "DeprecationWarning",
    "EOFError",
    "Ellipsis",
    "EncodingWarning",
    "EnvironmentError",
    "Exception",
    "ExceptionGroup",
    "False",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "FutureWarning",
    "GeneratorExit",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "None",
    "NotADirectoryError",
    "NotImplemented",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PendingDeprecationWarning",
    "PermissionError",
    "ProcessLookupError",
    "PythonFinalizationError",
    "RecursionError",
    "ReferenceError",
    "ResourceWarning",
    "RuntimeError",
    "RuntimeWarning",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "True",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "UserWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
    "_IncompleteInputError",
    "__build_class__",
    "__debug__",
    "__doc__",
    "__import__",
    "__loader__",
    "__name__",
    "__package__",
    "__spec__",
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "copyright",
    "credits",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "exit",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "license",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "quit",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
];

// --- CST helpers shared by the submodules --------------------------------------------------

/// Source text of a node.
pub(super) fn text<'a>(src: &'a str, node: Node<'_>) -> &'a str {
    &src[node.start_byte()..node.end_byte()]
}

/// The lowercase prefix letters of a `string` node (`rb"..."` → `"rb"`).
fn string_prefix(src: &str, node: Node<'_>) -> String {
    let start = match node.child(0) {
        Some(c) if c.kind() == "string_start" => text(src, c),
        _ => text(src, node),
    };
    start
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// An f-string or t-string (`string` node whose prefix has `f` or `t`).
pub(super) fn is_format_string(src: &str, node: Node<'_>) -> bool {
    let p = string_prefix(src, node);
    p.contains('f') || p.contains('t')
}

/// A plain `str` literal: no f/t prefix and not bytes.
fn is_plain_str(src: &str, node: Node<'_>) -> bool {
    node.kind() == "string" && {
        let p = string_prefix(src, node);
        !p.contains('f') && !p.contains('t') && !p.contains('b')
    }
}

/// An `expression_statement` whose only content is a `str` constant (possibly
/// parenthesized or implicitly concatenated): `ast.Expr(ast.Constant(str))`.
pub(super) fn is_docstring_stmt(src: &str, node: Node<'_>) -> bool {
    if node.kind() != "expression_statement" {
        return false;
    }
    let mut cursor = node.walk();
    let mut kids = node.named_children(&mut cursor).filter(|c| !is_extra(*c));
    let (Some(value), None) = (kids.next(), kids.next()) else {
        return false;
    };
    let value = unwrap_parens(value);
    match value.kind() {
        "string" => is_plain_str(src, value),
        "concatenated_string" => {
            let mut c = value.walk();
            value
                .named_children(&mut c)
                .filter(|c| !is_extra(*c))
                .all(|c| is_plain_str(src, c))
        }
        _ => false,
    }
}

/// Comments and backslash continuations: present in the CST, absent from the AST.
pub(super) fn is_extra(node: Node<'_>) -> bool {
    matches!(node.kind(), "comment" | "line_continuation")
}

/// Strip grouping parentheses: `(x)` → `x`, and a one-element `tuple_pattern` without a
/// comma (tree-sitter's shape for a parenthesized assignment target).
pub(super) fn unwrap_parens(mut node: Node<'_>) -> Node<'_> {
    loop {
        let mut cursor = node.walk();
        match node.kind() {
            "parenthesized_expression" => {}
            "tuple_pattern" if !node.children(&mut cursor).any(|c| c.kind() == ",") => {}
            _ => return node,
        }
        let mut kids = node.named_children(&mut cursor).filter(|c| !is_extra(*c));
        match (kids.next(), kids.next()) {
            (Some(inner), None) => node = inner,
            _ => return node,
        }
    }
}

/// The end of a node ignoring trailing comments (CPython's `end_lineno` / `end_col_offset`:
/// tree-sitter attaches a comment line that follows a block to that block).
pub(super) fn end_point(node: Node<'_>) -> Point {
    let mut n = node;
    loop {
        let count = n.child_count();
        if count == 0 {
            return n.end_position();
        }
        let last = (0..count)
            .rev()
            .filter_map(|i| n.child(i))
            .find(|c| !is_extra(*c));
        match last {
            Some(c) => n = c,
            None => return n.end_position(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_for_binary_search() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(SOFT_KEYWORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(BUILTINS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn keywords_and_builtins() {
        let py = PythonAnalyzer::new();
        assert!(py.is_keyword("def"));
        assert!(py.is_keyword("None"));
        assert!(py.is_keyword("match"));
        assert!(py.is_keyword("_"));
        assert!(!py.is_keyword("print"));
        assert!(py.is_builtin("print"));
        assert!(py.is_builtin("__name__"));
        assert!(!py.is_builtin("def"));
        assert!(py.handles("a.py") && py.handles("a.pyi") && !py.handles("a.ts"));
        assert!(py.verifier().is_some());
    }

    #[test]
    fn analyze_is_total() {
        let py = PythonAnalyzer::new();
        let an = py.analyze("");
        assert!(an.parsed && an.tokens.is_empty() && an.syntax.is_some());
        let an = py.analyze("def f(:\n    pass\n");
        assert!(!an.parsed && an.syntax.is_none() && !an.tokens.is_empty());
        assert!(an.statements.is_empty() && an.defs.is_empty());
    }
}
