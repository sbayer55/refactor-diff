//! Structure of a cleanly parsed file: annotations, docstrings, statement spans, call sites,
//! def sites and import sites, in the order CPython's `ast.walk` (breadth-first) produced them.

use tree_sitter::Node;

use super::super::{
    Annotation, Arg, ArgKeyword, Binding, CallSite, DefSite, ImportSite, NodeRef, Param, ParamKind,
    StmtKind, StmtSpan, Syntax, collapse_ws, pos_of,
};
use super::{end_point, is_docstring_stmt, is_extra, text, unwrap_parens};
use crate::text::Pos;

pub(super) struct Structure {
    pub annotations: Vec<Annotation>,
    pub docstrings: Vec<(Pos, Pos)>,
    pub statements: Vec<StmtSpan>,
    pub calls: Vec<CallSite>,
    pub defs: Vec<DefSite>,
    pub imports: Vec<ImportSite>,
}

pub(super) fn extract(syntax: &Syntax, lines: &[String]) -> Structure {
    let root = syntax.tree.root_node();
    let mut cx = Collector {
        src: &syntax.source,
        lines,
        annotations: Vec::new(),
        docstrings: Vec::new(),
        calls: Vec::new(),
        defs: Vec::new(),
        imports: Vec::new(),
    };
    cx.visit(root, 0);
    // `ast.walk` is breadth-first: everything at AST depth 1 first, then depth 2, ... and
    // within one depth in document order.
    cx.annotations
        .sort_by_key(|(depth, at, seq, _)| (*depth, *at, *seq));
    cx.docstrings.sort_by_key(|(depth, at, _)| (*depth, *at));
    cx.calls.sort_by_key(|c| (c.start, c.end));
    cx.defs.sort_by_key(|d| (d.start, d.end));
    cx.imports.sort_by_key(|s| s.start);
    Structure {
        annotations: cx.annotations.into_iter().map(|(_, _, _, a)| a).collect(),
        docstrings: cx.docstrings.into_iter().map(|(_, _, d)| d).collect(),
        statements: statements(&syntax.source, root, ""),
        calls: cx.calls,
        defs: cx.defs,
        imports: cx.imports,
    }
}

struct Collector<'a> {
    src: &'a str,
    lines: &'a [String],
    /// `(ast depth, statement start byte, sequence within the statement, annotation)`.
    annotations: Vec<(u32, usize, usize, Annotation)>,
    docstrings: Vec<(u32, usize, (Pos, Pos))>,
    calls: Vec<CallSite>,
    defs: Vec<DefSite>,
    imports: Vec<ImportSite>,
}

impl Collector<'_> {
    fn start(&self, node: Node<'_>) -> Pos {
        pos_of(self.lines, node.start_position())
    }

    fn end(&self, node: Node<'_>) -> Pos {
        pos_of(self.lines, end_point(node))
    }

    /// `depth` is the AST depth of `node` when it is a statement; for a block or clause it is
    /// the depth its statements get.
    fn visit(&mut self, node: Node<'_>, depth: u32) {
        match node.kind() {
            "function_definition" => self.def(node, depth),
            "expression_statement" if is_docstring_stmt(self.src, node) => {
                self.docstrings.push((
                    depth,
                    node.start_byte(),
                    (self.start(node), self.end(node)),
                ));
            }
            "assignment" => {
                if let Some(ty) = node.child_by_field_name("type") {
                    let target = node
                        .child_by_field_name("left")
                        .map(unwrap_parens)
                        .map(|t| collapse_ws(text(self.src, t)))
                        .unwrap_or_default();
                    self.annotation(depth, node, 0, ty, format!("variable {target}"));
                }
            }
            "call" => self.call(node),
            "import_statement" | "import_from_statement" | "future_import_statement" => {
                self.import(node);
            }
            _ => {}
        }
        let mut elifs = 0u32;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let d = child_depth(node, child, depth, &mut elifs);
            self.visit(child, d);
        }
    }

    fn annotation(&mut self, depth: u32, stmt: Node<'_>, seq: usize, ty: Node<'_>, target: String) {
        self.annotations.push((
            depth,
            stmt.start_byte(),
            seq,
            Annotation {
                start: self.start(ty),
                end: pos_of(self.lines, ty.end_position()),
                text: collapse_ws(text(self.src, ty)),
                target,
            },
        ));
    }

    fn def(&mut self, node: Node<'_>, depth: u32) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let Some(params_node) = node.child_by_field_name("parameters") else {
            return;
        };
        let name = text(self.src, name_node);
        let params = parameters(self.src, params_node);
        // CPython lists annotations as posonlyargs, args, kwonlyargs, vararg, kwarg, returns.
        let mut seq = 0;
        for kind in [
            ParamKind::PosOnly,
            ParamKind::Pos,
            ParamKind::KwOnly,
            ParamKind::VarArg,
            ParamKind::KwArg,
        ] {
            for (param, ty) in params.iter().filter(|(p, _)| p.kind == kind) {
                if let Some(ty) = ty {
                    self.annotation(depth, node, seq, *ty, format!("param {}", param.name));
                    seq += 1;
                }
            }
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.annotation(depth, node, seq, ret, format!("return of {name}"));
        }
        let mut params: Vec<Param> = params
            .into_iter()
            .map(|(mut p, _)| {
                p.start = pos_of(self.lines, p_start(p.start));
                p.end = pos_of(self.lines, p_start(p.end));
                p
            })
            .collect();
        params.sort_by_key(|p| p.start);
        self.defs.push(DefSite {
            name: name.to_string(),
            start: self.start(node),
            end: self.end(node),
            params_start: self.start(params_node),
            params_end: pos_of(self.lines, params_node.end_position()),
            params,
        });
    }

    fn call(&mut self, node: Node<'_>) {
        let Some(function) = node.child_by_field_name("function") else {
            return;
        };
        if !matches!(function.kind(), "identifier" | "attribute") {
            return;
        }
        let Some(arguments) = node.child_by_field_name("arguments") else {
            return;
        };
        let mut args: Vec<Arg> = Vec::new();
        if arguments.kind() == "generator_expression" {
            args.push(Arg {
                start: self.start(arguments),
                end: pos_of(self.lines, arguments.end_position()),
                keyword: ArgKeyword::Positional,
            });
        } else {
            let mut cursor = arguments.walk();
            for a in arguments.named_children(&mut cursor) {
                if is_extra(a) {
                    continue;
                }
                let keyword = match a.kind() {
                    "keyword_argument" => ArgKeyword::Named(
                        a.child_by_field_name("name")
                            .map(|n| text(self.src, n).to_string())
                            .unwrap_or_default(),
                    ),
                    "list_splat" => ArgKeyword::Star,
                    "dictionary_splat" => ArgKeyword::StarStar,
                    _ => ArgKeyword::Positional,
                };
                let span = if keyword == ArgKeyword::Positional {
                    unwrap_parens(a)
                } else {
                    a
                };
                args.push(Arg {
                    start: self.start(span),
                    end: pos_of(self.lines, span.end_position()),
                    keyword,
                });
            }
        }
        args.sort_by_key(|a| (a.start, a.end));
        let end = pos_of(self.lines, node.end_position());
        self.calls.push(CallSite {
            name: text(self.src, function)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect(),
            start: self.start(node),
            end,
            args_start: pos_of(self.lines, function.end_position()),
            args_end: end,
            args,
        });
    }

    fn import(&mut self, node: Node<'_>) {
        let mut bindings = Vec::new();
        let mut cursor = node.walk();
        match node.kind() {
            "import_statement" => {
                for name in node.children_by_field_name("name", &mut cursor) {
                    let (module, alias) = self.module_and_alias(name);
                    let alias = alias.unwrap_or_else(|| {
                        module.split('.').next().unwrap_or_default().to_string()
                    });
                    bindings.push(Binding::new(module, None, alias));
                }
            }
            "import_from_statement" | "future_import_statement" => {
                let module: String = match node.child_by_field_name("module_name") {
                    Some(m) => text(self.src, m)
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect(),
                    None => "__future__".to_string(),
                };
                let level = module.chars().take_while(|c| *c == '.').count() as u32;
                let mut names: Vec<Node<'_>> =
                    node.children_by_field_name("name", &mut cursor).collect();
                let mut c2 = node.walk();
                names.extend(
                    node.named_children(&mut c2)
                        .filter(|c| c.kind() == "wildcard_import"),
                );
                names.sort_by_key(|n| n.start_byte());
                for name in names {
                    let (imported, alias) = if name.kind() == "wildcard_import" {
                        ("*".to_string(), None)
                    } else {
                        self.module_and_alias(name)
                    };
                    let alias = alias.unwrap_or_else(|| imported.clone());
                    let mut b = Binding::new(module.clone(), Some(imported), alias);
                    b.level = level;
                    bindings.push(b);
                }
            }
            _ => return,
        }
        self.imports.push(ImportSite {
            start: node.start_position().row as u32 + 1,
            end: end_point(node).row as u32 + 1,
            bindings,
        });
    }

    /// `(dotted name, alias)` of a `dotted_name` or `aliased_import` node.
    fn module_and_alias(&self, node: Node<'_>) -> (String, Option<String>) {
        let dotted = |n: Node<'_>| -> String {
            text(self.src, n)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect()
        };
        if node.kind() == "aliased_import" {
            let name = node
                .child_by_field_name("name")
                .map(dotted)
                .unwrap_or_default();
            let alias = node
                .child_by_field_name("alias")
                .map(|a| text(self.src, a).to_string());
            (name, alias)
        } else {
            (dotted(node), None)
        }
    }
}

/// `Pos` values built by [`parameters`] carry byte columns; convert through `pos_of`.
fn p_start(p: Pos) -> tree_sitter::Point {
    tree_sitter::Point::new(p.line as usize - 1, p.col as usize)
}

/// The AST depth of `child` given its parent's depth. `block` and clause nodes pass through the
/// depth their statements get; `elif` chains nest one `If` per clause in the AST, an
/// `except` handler and a `match` case are nodes of their own above their bodies.
fn child_depth(parent: Node<'_>, child: Node<'_>, d: u32, elifs: &mut u32) -> u32 {
    match parent.kind() {
        "module" => 1,
        "if_statement" => match child.kind() {
            "block" => d + 1,
            "elif_clause" => {
                *elifs += 1;
                d + 1 + *elifs
            }
            "else_clause" => d + 1 + *elifs,
            _ => d,
        },
        "for_statement"
        | "while_statement"
        | "with_statement"
        | "function_definition"
        | "class_definition" => match child.kind() {
            "block" | "else_clause" => d + 1,
            _ => d,
        },
        "try_statement" => match child.kind() {
            "block" | "else_clause" | "finally_clause" => d + 1,
            "except_clause" | "except_group_clause" => d + 2,
            _ => d,
        },
        "match_statement" => match child.kind() {
            "block" => d + 1,
            _ => d,
        },
        "case_clause" => match child.kind() {
            "block" => d + 1,
            _ => d,
        },
        _ => d,
    }
}

/// The parameters of a def with their kinds and annotation nodes. Spans are returned with
/// *byte* columns (converted by the caller); a `*args` / `**kwargs` span starts at the name,
/// as CPython's `ast.arg` does.
fn parameters<'t>(src: &str, params: Node<'t>) -> Vec<(Param, Option<Node<'t>>)> {
    let byte_pos = |p: tree_sitter::Point| Pos::new(p.row as u32 + 1, p.column as u32);
    let mut out: Vec<(Param, Option<Node<'t>>)> = Vec::new();
    let mut state = ParamKind::Pos;
    let mut cursor = params.walk();
    for node in params.named_children(&mut cursor) {
        if is_extra(node) {
            continue;
        }
        let ty = node.child_by_field_name("type");
        let mut has_default = false;
        let pattern = match node.kind() {
            "identifier" | "list_splat_pattern" | "dictionary_splat_pattern" => node,
            "typed_parameter" => {
                let mut c = node.walk();
                match node.named_children(&mut c).find(|c| !is_extra(*c)) {
                    Some(p) => p,
                    None => continue,
                }
            }
            "default_parameter" | "typed_default_parameter" => {
                has_default = true;
                match node.child_by_field_name("name") {
                    Some(p) => p,
                    None => continue,
                }
            }
            "keyword_separator" => {
                state = ParamKind::KwOnly;
                continue;
            }
            "positional_separator" => {
                for (p, _) in out.iter_mut() {
                    if p.kind == ParamKind::Pos {
                        p.kind = ParamKind::PosOnly;
                    }
                }
                continue;
            }
            _ => continue,
        };
        let (kind, name_node) = match pattern.kind() {
            "list_splat_pattern" => (ParamKind::VarArg, splat_name(pattern)),
            "dictionary_splat_pattern" => (ParamKind::KwArg, splat_name(pattern)),
            _ => (state, pattern),
        };
        if kind == ParamKind::VarArg {
            state = ParamKind::KwOnly;
        }
        out.push((
            Param {
                name: text(src, name_node).to_string(),
                kind,
                has_default,
                start: byte_pos(name_node.start_position()),
                end: byte_pos(node.end_position()),
            },
            ty,
        ));
    }
    out
}

/// The name inside `*args` / `**kwargs` (the whole pattern when it has no identifier).
fn splat_name(pattern: Node<'_>) -> Node<'_> {
    let mut c = pattern.walk();
    pattern
        .named_children(&mut c)
        .find(|c| !is_extra(*c))
        .unwrap_or(pattern)
}

/// Spans of the statements in a module or class body; class bodies recurse so that methods
/// are spans of their own.
fn statements(src: &str, body: Node<'_>, prefix: &str) -> Vec<StmtSpan> {
    let mut spans = Vec::new();
    let mut cursor = body.walk();
    for node in body.named_children(&mut cursor) {
        if is_extra(node) || node.is_error() {
            continue;
        }
        let start = node.start_position().row as u32 + 1;
        let inner = if node.kind() == "decorated_definition" {
            node.child_by_field_name("definition").unwrap_or(node)
        } else {
            node
        };
        let end = end_point(inner).row as u32 + 1;
        let name = || {
            inner
                .child_by_field_name("name")
                .map(|n| text(src, n))
                .unwrap_or_default()
        };
        let (kind, qualname, children) = match inner.kind() {
            "function_definition" => (StmtKind::Def, format!("{prefix}{}", name()), vec![]),
            "class_definition" => {
                let qual = format!("{prefix}{}", name());
                let children = inner
                    .child_by_field_name("body")
                    .map(|b| statements(src, b, &format!("{qual}.")))
                    .unwrap_or_default();
                (StmtKind::Class, qual, children)
            }
            _ => (StmtKind::Stmt, String::new(), vec![]),
        };
        spans.push(StmtSpan {
            start,
            end,
            kind,
            qualname,
            node: Some(NodeRef::of(&node)),
            children,
        });
    }
    spans
}
