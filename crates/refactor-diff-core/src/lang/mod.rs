//! Language analyzers and the language-neutral types they produce.

pub mod pyrepr;
pub mod python;
pub mod typescript;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::LazyLock;

use crate::text::Pos;

/// Token kinds. Structural tokens carry layout meaning (Python indentation, logical line ends)
/// but have no visible text worth highlighting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    Name,
    Op,
    String,
    Number,
    Comment,
    Structural,
    Other,
}

impl TokenKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            TokenKind::Name => "name",
            TokenKind::Op => "op",
            TokenKind::String => "string",
            TokenKind::Number => "number",
            TokenKind::Comment => "comment",
            TokenKind::Structural => "structural",
            TokenKind::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Token {
    pub kind: TokenKind,
    /// Normalized value used for comparison.
    pub value: String,
    /// Original source text.
    pub text: String,
    pub start: Pos,
    pub end: Pos,
}

impl Token {
    pub fn new(
        kind: TokenKind,
        value: impl Into<String>,
        text: impl Into<String>,
        start: Pos,
        end: Pos,
    ) -> Self {
        Self {
            kind,
            value: value.into(),
            text: text.into(),
            start,
            end,
        }
    }

    /// A zero-width structural token (`<INDENT>`, `<DEDENT>`, `<NEWLINE>`).
    pub fn structural(value: &str, at: Pos) -> Self {
        Self::new(TokenKind::Structural, value, "", at, at)
    }

    pub fn is(&self, kind: TokenKind, value: &str) -> bool {
        self.kind == kind && self.value == value
    }
}

/// A type annotation span and what it annotates (e.g. "param user_id").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    pub start: Pos,
    pub end: Pos,
    pub text: String,
    pub target: String,
}

impl Annotation {
    pub fn contains(&self, tok: &Token) -> bool {
        self.start <= tok.start && tok.end <= self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StmtKind {
    Def,
    Class,
    Stmt,
}

impl StmtKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            StmtKind::Def => "def",
            StmtKind::Class => "class",
            StmtKind::Stmt => "stmt",
        }
    }
}

/// A node of [`Syntax`], re-located on demand (byte range plus node kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind_id: u16,
}

impl NodeRef {
    pub fn of(node: &tree_sitter::Node<'_>) -> Self {
        Self {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            kind_id: node.kind_id(),
        }
    }
}

/// A statement's line span (1-based, inclusive; decorators included). Top-level statements
/// recurse into class bodies to method level via `children`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StmtSpan {
    pub start: u32,
    pub end: u32,
    pub kind: StmtKind,
    /// `Cls.method` for methods, the def/class name, or `""` for other statements.
    pub qualname: String,
    pub node: Option<NodeRef>,
    pub children: Vec<StmtSpan>,
}

impl StmtSpan {
    pub fn contains(&self, first: u32, last: u32) -> bool {
        self.start <= first && last <= self.end
    }

    /// This span and every descendant, depth-first.
    pub fn flatten(&self) -> Vec<&StmtSpan> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.flatten());
        }
        out
    }
}

/// How a call argument is passed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArgKeyword {
    Positional,
    Star,
    StarStar,
    Named(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Arg {
    pub start: Pos,
    pub end: Pos,
    pub keyword: ArgKeyword,
}

/// A call expression: `name` is the callee text, `args_*` the span from the opening
/// parenthesis through the closing one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallSite {
    pub name: String,
    pub start: Pos,
    pub end: Pos,
    pub args_start: Pos,
    pub args_end: Pos,
    pub args: Vec<Arg>,
}

impl CallSite {
    pub fn short_name(&self) -> &str {
        self.name.rsplit('.').next().unwrap_or(&self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamKind {
    PosOnly,
    Pos,
    VarArg,
    KwOnly,
    KwArg,
}

impl ParamKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ParamKind::PosOnly => "posonly",
            ParamKind::Pos => "pos",
            ParamKind::VarArg => "vararg",
            ParamKind::KwOnly => "kwonly",
            ParamKind::KwArg => "kwarg",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub has_default: bool,
    pub start: Pos,
    /// Span of the whole parameter including annotation and default.
    pub end: Pos,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DefSite {
    pub name: String,
    pub start: Pos,
    pub end: Pos,
    /// The parenthesised parameter list, parens included.
    pub params_start: Pos,
    pub params_end: Pos,
    pub params: Vec<Param>,
}

impl DefSite {
    pub fn short_name(&self) -> &str {
        &self.name
    }
}

/// One name an import statement binds. `name` is `None` for `import a.b`.
#[derive(Clone, Debug)]
pub struct Binding {
    /// Dotted module, prefixed with `.` per relative level.
    pub module: String,
    pub name: Option<String>,
    /// The name bound in the importing module.
    pub alias: String,
    pub level: u32,
    /// Display form, when not Python's syntax. Excluded from equality.
    pub text: String,
}

impl PartialEq for Binding {
    fn eq(&self, other: &Self) -> bool {
        self.module == other.module
            && self.name == other.name
            && self.alias == other.alias
            && self.level == other.level
    }
}

impl Eq for Binding {}

impl Hash for Binding {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.module.hash(state);
        self.name.hash(state);
        self.alias.hash(state);
        self.level.hash(state);
    }
}

impl Binding {
    pub fn new(module: impl Into<String>, name: Option<String>, alias: impl Into<String>) -> Self {
        Self {
            module: module.into(),
            name,
            alias: alias.into(),
            level: 0,
            text: String::new(),
        }
    }

    /// `module.name`, or just the module for `import a.b`.
    pub fn dotted(&self) -> String {
        match &self.name {
            Some(n) => format!("{}.{}", self.module, n),
            None => self.module.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSite {
    /// 1-based inclusive lines.
    pub start: u32,
    pub end: u32,
    pub bindings: Vec<Binding>,
}

/// A parsed tree together with the source it was parsed from.
#[derive(Clone)]
pub struct Syntax {
    pub tree: tree_sitter::Tree,
    pub source: String,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("root", &self.tree.root_node().kind())
            .finish()
    }
}

impl Syntax {
    /// Re-locate a node: the smallest node spanning the bytes, climbing to the recorded kind
    /// when several nodes share the same extent.
    pub fn node(&self, r: NodeRef) -> Option<tree_sitter::Node<'_>> {
        let root = self.tree.root_node();
        let mut node = root.descendant_for_byte_range(r.start_byte, r.end_byte)?;
        while node.kind_id() != r.kind_id {
            let Some(parent) = node.parent() else {
                return Some(node);
            };
            if parent.start_byte() != r.start_byte || parent.end_byte() != r.end_byte {
                return Some(node);
            }
            node = parent;
        }
        Some(node)
    }

    pub fn text_of(&self, node: &tree_sitter::Node<'_>) -> &str {
        &self.source[node.start_byte()..node.end_byte()]
    }
}

#[derive(Clone, Debug, Default)]
pub struct FileAnalysis {
    pub lines: Vec<String>,
    pub tokens: Vec<Token>,
    pub annotations: Vec<Annotation>,
    /// `(start, end)` spans.
    pub docstrings: Vec<(Pos, Pos)>,
    /// `false` when the file did not parse cleanly.
    pub parsed: bool,
    /// The tree, kept only for languages that verify by AST; `None` when the file didn't parse.
    pub syntax: Option<Syntax>,
    /// Structure from the parser; empty when the file didn't parse.
    pub statements: Vec<StmtSpan>,
    pub calls: Vec<CallSite>,
    pub defs: Vec<DefSite>,
    pub imports: Vec<ImportSite>,
}

impl FileAnalysis {
    /// Lines and tokens only, as for a file that did not parse.
    pub fn unparsed(lines: Vec<String>, tokens: Vec<Token>) -> Self {
        Self {
            lines,
            tokens,
            parsed: false,
            ..Default::default()
        }
    }

    /// Statement spans at every depth, depth-first.
    pub fn flat_statements(&self) -> Vec<&StmtSpan> {
        self.statements.iter().flat_map(|s| s.flatten()).collect()
    }
}

/// What the engine needs from a language.
pub trait LanguageAnalyzer: Send + Sync {
    fn name(&self) -> &'static str;
    /// Git pathspecs for this language's files.
    fn globs(&self) -> &'static [&'static str];
    fn handles(&self, path: &str) -> bool;
    /// Tokenize and parse. Never fails: parse errors set `parsed = false`.
    fn analyze(&self, text: &str) -> FileAnalysis;
    /// Blocks a rename when either side is a keyword.
    fn is_keyword(&self, value: &str) -> bool;
    /// Skips the leftover check for builtins.
    fn is_builtin(&self, value: &str) -> bool;
    /// Tokens that start an import statement.
    fn import_keywords(&self) -> &'static [&'static str];
    /// Tokens that precede a defined name.
    fn definition_keywords(&self) -> &'static [&'static str];
    /// AST verification (see `verify.rs`); `None` when the language opts out.
    fn verifier(&self) -> Option<&dyn AstVerifier> {
        None
    }
}

/// Canonical program comparison for "verified by AST".
pub trait AstVerifier: Send + Sync {
    /// Parse a standalone (possibly indented) block; `None` when it has syntax errors.
    fn parse_block(&self, text: &str) -> Option<Syntax>;
    /// A canonical dump of `node` (`None` = the root) with docstrings removed, identifiers
    /// mapped through `renames` and, optionally, type annotations dropped. Equal dumps mean
    /// the code is the same program.
    fn normalized_dump(
        &self,
        syntax: &Syntax,
        node: Option<NodeRef>,
        renames: &HashMap<String, String>,
        strip_annotations: bool,
    ) -> String;
}

static ANALYZERS: LazyLock<Vec<Box<dyn LanguageAnalyzer>>> = LazyLock::new(|| {
    vec![
        Box::new(python::PythonAnalyzer::new()),
        Box::new(typescript::TypeScriptAnalyzer::typescript()),
        Box::new(typescript::TypeScriptAnalyzer::tsx()),
        Box::new(typescript::TypeScriptAnalyzer::javascript()),
    ]
});

/// The analyzer for a path, or `None` when the file is listed but not analyzed.
pub fn analyzer_for(path: &str) -> Option<&'static dyn LanguageAnalyzer> {
    ANALYZERS
        .iter()
        .map(|a| a.as_ref())
        .find(|a| a.handles(path))
}

/// Every registered analyzer, in dispatch order.
pub fn analyzers() -> impl Iterator<Item = &'static dyn LanguageAnalyzer> {
    ANALYZERS.iter().map(|a| a.as_ref())
}

/// The file suffixes any analyzer handles (used to decide what a snapshot needs).
pub const SNAPSHOT_SUFFIXES: &[&str] = &[
    ".py", ".pyi", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs",
];

/// Parse `text` with `language`, returning the tree (never `None` without a timeout).
#[allow(dead_code)] // used by the analyzers
pub(crate) fn parse(language: &tree_sitter::Language, text: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(language)
        .expect("grammar ABI matches the tree-sitter crate");
    parser.parse(text.as_bytes(), None)
}

/// A tree-sitter point as a token position (code-point column).
#[allow(dead_code)] // used by the analyzers
pub(crate) fn pos_of(lines: &[String], point: tree_sitter::Point) -> Pos {
    let line = point.row as u32 + 1;
    Pos::new(line, crate::text::char_col(lines, line, point.column))
}

/// `text` with every run of whitespace collapsed to one space.
#[allow(dead_code)] // used by the analyzers
pub(crate) fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
