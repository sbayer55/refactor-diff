//! Code issues: problems in the code itself (syntax errors, unreachable code, unused names),
//! found per file and per side with tree-sitter.
//!
//! This is separate from `warnings.rs`, which checks whether a *refactor* was applied
//! consistently. Code issues never feed the report, its warnings or its explained counts; the
//! server serves them on their own endpoint for the viewer's squiggly underlines.
//!
//! The checks are deliberately conservative: a name counts as used when an identifier with
//! that name is read anywhere inside the function (nested functions included), and only
//! statements directly after a `return`/`raise`/`throw`/`break`/`continue` in the same block
//! are unreachable. No flow analysis, no cross-file analysis.

use std::collections::HashSet;

use serde::Serialize;
use tree_sitter::{Language, Node};

use crate::lang::{analyzer_for, parse, pos_of};
use crate::text::{Pos, split_lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// One issue: `start..end` in 1-based lines and code-point columns, like tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CodeIssue {
    pub severity: Severity,
    /// `syntax-error`, `unreachable`, `unused-variable`, `unused-parameter` or `unused-import`.
    pub code: &'static str,
    pub message: String,
    pub start: Pos,
    pub end: Pos,
}

pub const SYNTAX_ERROR: &str = "syntax-error";
pub const UNREACHABLE: &str = "unreachable";
pub const UNUSED_VARIABLE: &str = "unused-variable";
pub const UNUSED_PARAMETER: &str = "unused-parameter";
pub const UNUSED_IMPORT: &str = "unused-import";

/// The severity of each issue code (the one place that decides colors).
pub fn severity_of(code: &str) -> Severity {
    match code {
        SYNTAX_ERROR => Severity::Error,
        UNREACHABLE | UNUSED_VARIABLE => Severity::Warning,
        _ => Severity::Info,
    }
}

/// Code issues in one file, sorted by position. Empty for languages without an analyzer.
///
/// A file with syntax errors reports only those: scope results from a broken tree aren't
/// reliable.
pub fn find_issues(path: &str, text: &str) -> Vec<CodeIssue> {
    let Some(lang) = Lang::for_path(path) else {
        return vec![];
    };
    let Some(tree) = parse(&lang.grammar(), text) else {
        return vec![];
    };
    let root = tree.root_node();
    let mut cx = Cx {
        lang,
        path,
        src: text.as_bytes(),
        lines: split_lines(text),
        out: vec![],
    };
    if root.has_error() {
        cx.syntax_errors(root);
    } else {
        cx.unreachable(root);
        let positions = cx.binding_positions(root);
        cx.unused_names(root, &positions);
        cx.unused_imports(root, &positions);
    }
    let mut out = cx.out;
    out.sort_by(|a, b| (a.start, a.end, a.code).cmp(&(b.start, b.end, b.code)));
    out.dedup();
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Python,
    TypeScript,
    Tsx,
    JavaScript,
}

impl Lang {
    fn for_path(path: &str) -> Option<Self> {
        Some(match analyzer_for(path)?.name() {
            "python" => Lang::Python,
            "typescript" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "javascript" => Lang::JavaScript,
            _ => return None,
        })
    }

    fn grammar(self) -> Language {
        match self {
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }

    fn is_python(self) -> bool {
        self == Lang::Python
    }

    /// Nodes that hold a sequence of statements.
    fn statement_lists(self) -> &'static [&'static str] {
        if self.is_python() {
            &["module", "block"]
        } else {
            &[
                "program",
                "statement_block",
                "switch_case",
                "switch_default",
            ]
        }
    }

    /// Statements after which the rest of their block never runs.
    fn terminators(self) -> &'static [&'static str] {
        if self.is_python() {
            &[
                "return_statement",
                "raise_statement",
                "break_statement",
                "continue_statement",
            ]
        } else {
            &[
                "return_statement",
                "throw_statement",
                "break_statement",
                "continue_statement",
            ]
        }
    }

    /// Statements that are fine after a terminator: hoisted or type-only declarations.
    fn reachable_anyway(self) -> &'static [&'static str] {
        if self.is_python() {
            &[]
        } else {
            &[
                "function_declaration",
                "generator_function_declaration",
                "interface_declaration",
                "type_alias_declaration",
                "ambient_declaration",
                "empty_statement",
            ]
        }
    }

    /// Function-like nodes whose locals and parameters are checked.
    fn functions(self) -> &'static [&'static str] {
        if self.is_python() {
            &["function_definition", "lambda"]
        } else {
            &[
                "function_declaration",
                "function_expression",
                "function",
                "generator_function_declaration",
                "generator_function",
                "arrow_function",
                "method_definition",
            ]
        }
    }

    /// Nodes that open a new scope (a local walk stops at them).
    fn scopes(self) -> &'static [&'static str] {
        if self.is_python() {
            &["function_definition", "lambda", "class_definition"]
        } else {
            &[
                "function_declaration",
                "function_expression",
                "function",
                "generator_function_declaration",
                "generator_function",
                "arrow_function",
                "method_definition",
                "class_declaration",
                "class",
                "abstract_class_declaration",
            ]
        }
    }

    /// Node kinds that can read a name.
    fn name_kinds(self) -> &'static [&'static str] {
        if self.is_python() {
            &["identifier"]
        } else {
            &[
                "identifier",
                "type_identifier",
                "shorthand_property_identifier",
            ]
        }
    }

    fn imports(self) -> &'static [&'static str] {
        if self.is_python() {
            &[
                "import_statement",
                "import_from_statement",
                "future_import_statement",
            ]
        } else {
            &["import_statement"]
        }
    }

    /// Calls that can read any local by name.
    fn dynamic_scope_calls(self) -> &'static [&'static str] {
        if self.is_python() {
            &["locals", "vars", "eval", "exec"]
        } else {
            &["eval"]
        }
    }
}

struct Cx<'s> {
    lang: Lang,
    path: &'s str,
    src: &'s [u8],
    lines: Vec<String>,
    out: Vec<CodeIssue>,
}

impl<'s> Cx<'s> {
    fn text(&self, node: Node<'_>) -> &'s str {
        node.utf8_text(self.src).unwrap_or("")
    }

    fn push(&mut self, code: &'static str, message: String, start: Node<'_>, end: Node<'_>) {
        let mut start_pos = pos_of(&self.lines, start.start_position());
        let mut end_pos = pos_of(&self.lines, end.end_position());
        if start_pos == end_pos {
            // A zero-width node (a missing token): widen to one character so it shows.
            let len = self
                .lines
                .get(start_pos.line as usize - 1)
                .map_or(0, |l| l.chars().count() as u32);
            if start_pos.col < len {
                end_pos.col += 1;
            } else if start_pos.col > 0 {
                start_pos.col -= 1;
            }
        }
        self.out.push(CodeIssue {
            severity: severity_of(code),
            code,
            message,
            start: start_pos,
            end: end_pos,
        });
    }

    // ---------- syntax errors ----------

    fn syntax_errors(&mut self, root: Node<'_>) {
        let mut found = vec![];
        preorder(root, |n| {
            if n.is_error() || n.is_missing() {
                found.push(n);
                false
            } else {
                n.has_error()
            }
        });
        for n in found {
            let message = if n.is_error() {
                "Syntax error".to_string()
            } else if n.is_named() {
                format!("Syntax error: missing {}", n.kind().replace('_', " "))
            } else {
                format!("Syntax error: missing `{}`", n.kind())
            };
            self.push(SYNTAX_ERROR, message, n, n);
        }
    }

    // ---------- unreachable code ----------

    fn unreachable(&mut self, root: Node<'_>) {
        let mut lists = vec![];
        preorder(root, |n| {
            if self.lang.statement_lists().contains(&n.kind()) {
                lists.push(n);
            }
            true
        });
        for list in lists {
            self.dead_statements(list);
        }
    }

    fn dead_statements(&mut self, list: Node<'_>) {
        let mut cursor = list.walk();
        let stmts: Vec<Node<'_>> = if matches!(list.kind(), "switch_case" | "switch_default") {
            list.children_by_field_name("body", &mut cursor).collect()
        } else {
            list.named_children(&mut cursor).collect()
        };
        let stmts: Vec<Node<'_>> = stmts
            .into_iter()
            .filter(|s| s.kind() != "comment")
            .collect();
        let Some(stop) = stmts
            .iter()
            .position(|s| self.lang.terminators().contains(&s.kind()))
        else {
            return;
        };
        let dead: Vec<Node<'_>> = stmts[stop + 1..]
            .iter()
            .copied()
            .filter(|s| !self.lang.reachable_anyway().contains(&s.kind()))
            .collect();
        if let (Some(&first), Some(&last)) = (dead.first(), dead.last()) {
            self.push(UNREACHABLE, "Unreachable code".into(), first, last);
        }
    }

    // ---------- names: where they are bound ----------

    /// Ids of every name node that binds or declares rather than reads (assignment targets,
    /// parameters, definition names, import names, ...).
    fn binding_positions(&self, root: Node<'_>) -> HashSet<usize> {
        let mut ids = HashSet::new();
        let imports = self.lang.imports();
        preorder(root, |n| {
            if imports.contains(&n.kind()) {
                preorder(n, |m| {
                    ids.insert(m.id());
                    true
                });
                return false;
            }
            for b in self.bind_sites(n) {
                ids.insert(b.id());
            }
            if self.lang.functions().contains(&n.kind()) {
                for param in self.params(n) {
                    ids.extend(param.names.iter().map(|p| p.id()));
                }
            }
            if let Some(name) = n.child_by_field_name("name")
                && matches!(
                    n.kind(),
                    "function_definition"
                        | "class_definition"
                        | "function_declaration"
                        | "generator_function_declaration"
                        | "function_expression"
                        | "class_declaration"
                        | "abstract_class_declaration"
                        | "class"
                        | "interface_declaration"
                        | "type_alias_declaration"
                        | "enum_declaration"
                        | "type_parameter"
                )
            {
                ids.insert(name.id());
            }
            if n.kind() == "catch_clause"
                && let Some(param) = n.child_by_field_name("parameter")
            {
                ids.extend(self.ts_pattern_names(param).iter().map(|p| p.id()));
            }
            true
        });
        ids
    }

    /// Names that `node` itself binds as a local (not descending into its children's own
    /// bindings).
    fn bind_sites<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        if self.lang.is_python() {
            match node.kind() {
                // `x: int` without a value only declares.
                "assignment" if node.child_by_field_name("right").is_some() => node
                    .child_by_field_name("left")
                    .map(py_targets)
                    .unwrap_or_default(),
                "for_statement" => node
                    .child_by_field_name("left")
                    .map(py_targets)
                    .unwrap_or_default(),
                "as_pattern" => node
                    .child_by_field_name("alias")
                    .map(py_targets)
                    .unwrap_or_default(),
                "except_clause" => node
                    .child_by_field_name("alias")
                    .map(py_targets)
                    .unwrap_or_default(),
                "named_expression" => node.child_by_field_name("name").into_iter().collect(),
                _ => vec![],
            }
        } else {
            match node.kind() {
                "variable_declarator" => node
                    .child_by_field_name("name")
                    .map(|n| self.ts_pattern_names(n))
                    .unwrap_or_default(),
                "for_in_statement" if node.child_by_field_name("kind").is_some() => node
                    .child_by_field_name("left")
                    .map(|n| self.ts_pattern_names(n))
                    .unwrap_or_default(),
                _ => vec![],
            }
        }
    }

    /// Binding names in a TS/JS pattern. Object patterns with a rest element bind nothing
    /// reportable: picking names out to omit them from the rest is the point.
    fn ts_pattern_names<'t>(&self, node: Node<'t>) -> Vec<Node<'t>> {
        let mut out = vec![];
        ts_pattern(node, &mut out);
        out
    }

    fn params<'t>(&self, func: Node<'t>) -> Vec<Param<'t>> {
        let mut out = vec![];
        if self.lang.is_python() {
            let list = func.child_by_field_name("parameters").or_else(|| {
                let mut c = func.walk();
                func.named_children(&mut c)
                    .find(|n| n.kind() == "lambda_parameters")
            });
            let Some(list) = list else { return out };
            let mut cursor = list.walk();
            for p in list.named_children(&mut cursor) {
                let name = match p.kind() {
                    "identifier" => Some(p),
                    "typed_parameter" => {
                        let mut c = p.walk();
                        p.named_children(&mut c)
                            .next()
                            .filter(|n| n.kind() == "identifier")
                    }
                    "default_parameter" | "typed_default_parameter" => p
                        .child_by_field_name("name")
                        .filter(|n| n.kind() == "identifier"),
                    // `*args`/`**kwargs` and separators are never reported.
                    _ => None,
                };
                if let Some(name) = name {
                    out.push(Param {
                        names: vec![name],
                        fixed: false,
                    });
                }
            }
        } else {
            if let Some(p) = func.child_by_field_name("parameter") {
                out.push(Param {
                    names: vec![p],
                    fixed: false,
                });
                return out;
            }
            let Some(list) = func.child_by_field_name("parameters") else {
                return out;
            };
            let mut cursor = list.walk();
            for p in list.named_children(&mut cursor) {
                match p.kind() {
                    "comment" | "decorator" => {}
                    "required_parameter" | "optional_parameter" => {
                        let mut c = p.walk();
                        // `constructor(private x)` declares a field: always "used".
                        let fixed = p.children(&mut c).any(|ch| {
                            matches!(
                                ch.kind(),
                                "accessibility_modifier" | "readonly" | "override_modifier"
                            )
                        });
                        let names = p
                            .child_by_field_name("pattern")
                            .map(|n| self.ts_pattern_names(n))
                            .unwrap_or_default();
                        out.push(Param { names, fixed });
                    }
                    _ => out.push(Param {
                        names: self.ts_pattern_names(p),
                        fixed: false,
                    }),
                }
            }
        }
        out
    }

    // ---------- names: where they are read ----------

    /// Whether `node` reads the name it spells.
    fn is_read(&self, node: Node<'_>, positions: &HashSet<usize>) -> bool {
        if !self.lang.name_kinds().contains(&node.kind()) || positions.contains(&node.id()) {
            return false;
        }
        let Some(parent) = node.parent() else {
            return true;
        };
        if !self.lang.is_python() {
            return true;
        }
        // `obj.name` and `f(name=...)` don't read a variable called `name`.
        match parent.kind() {
            "attribute" => parent.child_by_field_name("attribute") != Some(node),
            "keyword_argument" => parent.child_by_field_name("name") != Some(node),
            _ => true,
        }
    }

    fn reads_in(&self, node: Node<'_>, positions: &HashSet<usize>) -> HashSet<&'s str> {
        let mut reads = HashSet::new();
        preorder(node, |n| {
            if self.is_read(n, positions) {
                reads.insert(self.text(n));
            }
            true
        });
        reads
    }

    // ---------- unused locals and parameters ----------

    fn unused_names(&mut self, root: Node<'_>, positions: &HashSet<usize>) {
        let mut funcs = vec![];
        preorder(root, |n| {
            if self.lang.functions().contains(&n.kind()) {
                funcs.push(n);
            }
            true
        });
        for f in funcs {
            self.check_function(f, positions);
        }
    }

    fn check_function(&mut self, func: Node<'_>, positions: &HashSet<usize>) {
        let reads = self.reads_in(func, positions);
        if self
            .lang
            .dynamic_scope_calls()
            .iter()
            .any(|c| reads.contains(c))
        {
            return;
        }
        let params = self.params(func);
        let param_names: HashSet<&str> = params
            .iter()
            .flat_map(|p| p.names.iter().map(|n| self.text(*n)))
            .collect();

        // Locals: bindings in this function's own scope, first site per name.
        let mut outer: HashSet<&str> = HashSet::new();
        let mut locals: Vec<Node<'_>> = vec![];
        if let Some(body) = func.child_by_field_name("body") {
            let scopes = self.lang.scopes();
            preorder(body, |n| {
                if scopes.contains(&n.kind()) {
                    return false;
                }
                if matches!(n.kind(), "global_statement" | "nonlocal_statement") {
                    let mut c = n.walk();
                    outer.extend(n.named_children(&mut c).map(|m| self.text(m)));
                }
                locals.extend(self.bind_sites(n));
                true
            });
        }
        let mut seen = HashSet::new();
        for local in locals {
            let name = self.text(local);
            if !seen.insert(name)
                || reads.contains(name)
                || outer.contains(name)
                || param_names.contains(name)
                || self.ignored(name)
            {
                continue;
            }
            self.push(
                UNUSED_VARIABLE,
                format!("`{name}` is assigned but never used"),
                local,
                local,
            );
        }

        if self.skip_params(func) {
            return;
        }
        let used: Vec<bool> = params
            .iter()
            .map(|p| {
                p.fixed
                    || p.names.is_empty()
                    || p.names.iter().any(|n| reads.contains(self.text(*n)))
            })
            .collect();
        // TS/JS convention: parameters before the last used one are placeholders for position.
        let from = if self.lang.is_python() {
            0
        } else {
            used.iter().rposition(|u| *u).map_or(0, |i| i + 1)
        };
        for (param, used) in params.iter().zip(&used).skip(from) {
            if *used {
                continue;
            }
            for name_node in &param.names {
                let name = self.text(*name_node);
                if reads.contains(name) || self.ignored(name) {
                    continue;
                }
                self.push(
                    UNUSED_PARAMETER,
                    format!("Parameter `{name}` is never used"),
                    *name_node,
                    *name_node,
                );
            }
        }
    }

    fn ignored(&self, name: &str) -> bool {
        name.is_empty() || name.starts_with('_') || name == "self" || name == "cls"
    }

    /// Functions whose parameter list is dictated from outside: stubs, overloads, abstract
    /// methods, overrides, TS/JS class methods and Python protocol (dunder) methods.
    fn skip_params(&self, func: Node<'_>) -> bool {
        if !self.lang.is_python() {
            // Class methods usually implement an interface or override a base method.
            return func.kind() == "method_definition";
        }
        if self.path.ends_with(".pyi") {
            return true;
        }
        if let Some(name) = func.child_by_field_name("name") {
            let name = self.text(name);
            if name.starts_with("__") && name.ends_with("__") {
                return true;
            }
        }
        if let Some(parent) = func.parent()
            && parent.kind() == "decorated_definition"
        {
            let mut c = parent.walk();
            let decorated = parent.named_children(&mut c).any(|d| {
                d.kind() == "decorator" && {
                    let t = self.text(d);
                    ["overload", "abstractmethod", "override", "abstractproperty"]
                        .iter()
                        .any(|k| t.contains(k))
                }
            });
            if decorated {
                return true;
            }
        }
        let Some(body) = func.child_by_field_name("body") else {
            return false;
        };
        if body.kind() != "block" {
            return false;
        }
        let mut c = body.walk();
        body.named_children(&mut c)
            .filter(|s| s.kind() != "comment")
            .all(|s| match s.kind() {
                "pass_statement" | "raise_statement" => true,
                "expression_statement" => {
                    let mut c2 = s.walk();
                    s.named_children(&mut c2)
                        .all(|e| matches!(e.kind(), "ellipsis" | "string"))
                }
                _ => false,
            })
    }

    // ---------- unused imports ----------

    fn unused_imports(&mut self, root: Node<'_>, positions: &HashSet<usize>) {
        if self.lang.is_python()
            && (self.path.ends_with(".pyi")
                || self.path == "__init__.py"
                || self.path.ends_with("/__init__.py"))
        {
            return;
        }
        let mut bound: Vec<Node<'_>> = vec![];
        let mut cursor = root.walk();
        for stmt in root.named_children(&mut cursor) {
            match stmt.kind() {
                "import_statement" if self.lang.is_python() => {
                    self.py_import_names(stmt, &mut bound)
                }
                "import_from_statement" => {
                    let module = stmt
                        .child_by_field_name("module_name")
                        .map(|m| self.text(m));
                    if module != Some("__future__") {
                        self.py_import_names(stmt, &mut bound);
                    }
                }
                "import_statement" => self.ts_import_names(stmt, &mut bound),
                _ => {}
            }
        }
        if bound.is_empty() {
            return;
        }

        let mut used: HashSet<&str> = self.reads_in(root, positions);
        let mut has_jsx = false;
        let python = self.lang.is_python();
        let imports = self.lang.imports();
        preorder(root, |n| {
            let kind = n.kind();
            if imports.contains(&kind) {
                return false;
            }
            // Names mentioned in text: Python's `__all__`, string annotations and type
            // comments; JSDoc types in TS/JS comments. Docstrings don't count.
            let mentions = if python {
                (kind == "string"
                    && n.parent()
                        .is_some_and(|p| p.kind() != "expression_statement"))
                    || (kind == "comment"
                        && self
                            .text(n)
                            .trim_start_matches(['#', ' '])
                            .starts_with("type:"))
            } else {
                kind == "comment"
            };
            if mentions {
                used.extend(words(self.text(n)));
                return false;
            }
            if matches!(
                kind,
                "jsx_element" | "jsx_self_closing_element" | "jsx_fragment"
            ) {
                has_jsx = true;
            }
            true
        });
        if has_jsx {
            // The classic JSX runtime reads `React` implicitly.
            used.insert("React");
        }
        for b in bound {
            let name = self.text(b);
            if used.contains(name) || name.is_empty() {
                continue;
            }
            self.push(
                UNUSED_IMPORT,
                format!("`{name}` is imported but never used"),
                b,
                b,
            );
        }
    }

    fn py_import_names<'t>(&self, stmt: Node<'t>, out: &mut Vec<Node<'t>>) {
        let mut cursor = stmt.walk();
        for n in stmt.children_by_field_name("name", &mut cursor) {
            match n.kind() {
                // `import a.b` binds `a`; `from m import a` binds `a`.
                "dotted_name" => {
                    let mut c = n.walk();
                    if let Some(first) = n.named_children(&mut c).next() {
                        out.push(first);
                    }
                }
                "aliased_import" => {
                    let alias = n.child_by_field_name("alias");
                    let name = n.child_by_field_name("name");
                    // `import x as x` is the explicit re-export idiom.
                    if let (Some(alias), Some(name)) = (alias, name)
                        && self.text(alias) != self.text(name)
                    {
                        out.push(alias);
                    }
                }
                _ => {}
            }
        }
    }

    fn ts_import_names<'t>(&self, stmt: Node<'t>, out: &mut Vec<Node<'t>>) {
        let mut cursor = stmt.walk();
        let Some(clause) = stmt
            .named_children(&mut cursor)
            .find(|n| n.kind() == "import_clause")
        else {
            return; // `import "x"` runs for its side effects
        };
        let mut c = clause.walk();
        for part in clause.named_children(&mut c) {
            match part.kind() {
                "identifier" => out.push(part),
                "namespace_import" => {
                    let mut c2 = part.walk();
                    out.extend(
                        part.named_children(&mut c2)
                            .filter(|n| n.kind() == "identifier"),
                    );
                }
                "named_imports" => {
                    let mut c2 = part.walk();
                    for spec in part.named_children(&mut c2) {
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let name = spec
                            .child_by_field_name("alias")
                            .or_else(|| spec.child_by_field_name("name"));
                        if let Some(name) = name.filter(|n| n.kind() == "identifier") {
                            out.push(name);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

struct Param<'t> {
    names: Vec<Node<'t>>,
    /// Never reported (a TS parameter property).
    fixed: bool,
}

/// Visit `node` and its descendants in source order; `f` returns whether to descend.
fn preorder<'t>(node: Node<'t>, mut f: impl FnMut(Node<'t>) -> bool) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if f(n) {
            let mut cursor = n.walk();
            let kids: Vec<Node<'t>> = n.children(&mut cursor).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
}

/// Identifiers bound by a Python assignment target (`a`, `a, b`, `[a, *rest]`, ...).
fn py_targets<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut out = vec![];
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" => out.push(n),
            "pattern_list"
            | "tuple_pattern"
            | "list_pattern"
            | "tuple"
            | "list"
            | "expression_list"
            | "parenthesized_expression"
            | "list_splat_pattern"
            | "list_splat"
            | "as_pattern_target" => {
                let mut c = n.walk();
                let kids: Vec<Node<'_>> = n.named_children(&mut c).collect();
                stack.extend(kids.into_iter().rev());
            }
            // Attributes and subscripts store into an object; nothing local is bound.
            _ => {}
        }
    }
    out
}

fn ts_pattern<'t>(node: Node<'t>, out: &mut Vec<Node<'t>>) {
    match node.kind() {
        "identifier" | "shorthand_property_identifier_pattern" => out.push(node),
        "object_pattern" => {
            let mut c = node.walk();
            let kids: Vec<Node<'_>> = node.named_children(&mut c).collect();
            if kids.iter().any(|k| k.kind() == "rest_pattern") {
                return;
            }
            for k in kids {
                match k.kind() {
                    "pair_pattern" => {
                        if let Some(v) = k.child_by_field_name("value") {
                            ts_pattern(v, out);
                        }
                    }
                    "object_assignment_pattern" => {
                        if let Some(l) = k.child_by_field_name("left") {
                            ts_pattern(l, out);
                        }
                    }
                    _ => ts_pattern(k, out),
                }
            }
        }
        "assignment_pattern" => {
            if let Some(l) = node.child_by_field_name("left") {
                ts_pattern(l, out);
            }
        }
        "array_pattern" | "rest_pattern" => {
            let mut c = node.walk();
            let kids: Vec<Node<'_>> = node.named_children(&mut c).collect();
            for k in kids {
                ts_pattern(k, out);
            }
        }
        _ => {}
    }
}

/// Identifier-like words in free text.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|w| !w.is_empty())
}
