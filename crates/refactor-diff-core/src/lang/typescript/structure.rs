//! Statement spans and import sites from the parsed tree.

use tree_sitter::Node;

use super::{field_text, named_children, node_text};
use crate::lang::{Binding, ImportSite, NodeRef, StmtKind, StmtSpan};

const FUNCTION_DECLS: &[&str] = &["function_declaration", "generator_function_declaration"];
const FUNCTION_VALUES: &[&str] = &[
    "arrow_function",
    "function_expression",
    "function",
    "generator_function",
];
const CLASS_DECLS: &[&str] = &["class_declaration", "abstract_class_declaration"];
const TYPE_DECLS: &[&str] = &[
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
];

fn line(node: Node<'_>, start: bool) -> u32 {
    let point = if start {
        node.start_position()
    } else {
        node.end_position()
    };
    point.row as u32 + 1
}

/// Top-level statements; class bodies recurse one level so that methods are spans of their
/// own. A statement's span includes its `export` and decorators.
pub(super) fn statements(root: Node<'_>, source: &str) -> Vec<StmtSpan> {
    named_children(root)
        .into_iter()
        .filter(|c| c.kind() != "comment")
        .map(|c| span(source, c, c, "", None))
        .collect()
}

fn span(
    source: &str,
    outer: Node<'_>,
    node: Node<'_>,
    prefix: &str,
    start: Option<u32>,
) -> StmtSpan {
    let first = start.unwrap_or_else(|| line(outer, true));
    let last = line(outer, false);
    let make = |kind: StmtKind, qualname: String, children: Vec<StmtSpan>| StmtSpan {
        start: first,
        end: last,
        kind,
        qualname,
        node: Some(NodeRef::of(&outer)),
        children,
    };
    if node.kind() == "export_statement" {
        if let Some(inner) = node.child_by_field_name("declaration") {
            return span(source, outer, inner, prefix, Some(first));
        }
        return make(StmtKind::Stmt, String::new(), Vec::new());
    }
    let name = field_text(source, node, "name");
    let kind = node.kind();
    if FUNCTION_DECLS.contains(&kind) && !name.is_empty() {
        return make(StmtKind::Def, format!("{prefix}{name}"), Vec::new());
    }
    if CLASS_DECLS.contains(&kind) && !name.is_empty() {
        let qual = format!("{prefix}{name}");
        let children = node
            .child_by_field_name("body")
            .map(|body| members(source, body, &format!("{qual}.")))
            .unwrap_or_default();
        return make(StmtKind::Class, qual, children);
    }
    if TYPE_DECLS.contains(&kind) && !name.is_empty() {
        return make(StmtKind::Class, format!("{prefix}{name}"), Vec::new());
    }
    if matches!(kind, "lexical_declaration" | "variable_declaration") {
        let decls: Vec<Node<'_>> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        if let [decl] = decls.as_slice() {
            let value = decl.child_by_field_name("value");
            let held = decl.child_by_field_name("name");
            if value.is_some_and(|v| FUNCTION_VALUES.contains(&v.kind()))
                && held.is_some_and(|h| h.kind() == "identifier")
            {
                let held = field_text(source, *decl, "name");
                return make(StmtKind::Def, format!("{prefix}{held}"), Vec::new());
            }
        }
    }
    make(StmtKind::Stmt, String::new(), Vec::new())
}

/// Class members; a member's decorators (siblings before it) belong to its span.
fn members(source: &str, body: Node<'_>, prefix: &str) -> Vec<StmtSpan> {
    let mut spans = Vec::new();
    let mut decorated: Option<u32> = None;
    for c in named_children(body) {
        if c.kind() == "comment" {
            continue;
        }
        if c.kind() == "decorator" {
            if decorated.is_none() {
                decorated = Some(line(c, true));
            }
            continue;
        }
        let start = decorated.take().unwrap_or_else(|| line(c, true));
        let name = field_text(source, c, "name");
        let (kind, qualname) = if c.kind() == "method_definition" && !name.is_empty() {
            (StmtKind::Def, format!("{prefix}{name}"))
        } else {
            (StmtKind::Stmt, String::new())
        };
        spans.push(StmtSpan {
            start,
            end: line(c, false),
            kind,
            qualname,
            node: Some(NodeRef::of(&c)),
            children: Vec::new(),
        });
    }
    spans
}

/// Top-level `import` statements and the names they bind.
pub(super) fn imports(root: Node<'_>, source: &str) -> Vec<ImportSite> {
    let mut sites = Vec::new();
    for node in named_children(root) {
        if node.kind() != "import_statement" {
            continue;
        }
        let Some(module_node) = node.child_by_field_name("source") else {
            continue;
        };
        let module: String = named_children(module_node)
            .into_iter()
            .map(|c| node_text(source, c))
            .collect();
        let quoted = format!("\"{module}\"");
        let binding = |name: Option<String>, alias: String, text: String| Binding {
            module: module.clone(),
            name,
            alias,
            level: 0,
            text,
        };
        let mut bindings = Vec::new();
        for clause in named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "import_clause")
        {
            for part in named_children(clause) {
                match part.kind() {
                    "identifier" => {
                        let alias = node_text(source, part).into_owned();
                        let text = format!("import {alias} from {quoted}");
                        bindings.push(binding(Some("default".to_string()), alias, text));
                    }
                    "namespace_import" => {
                        let alias = named_children(part)
                            .into_iter()
                            .rfind(|c| c.kind() == "identifier")
                            .map(|c| node_text(source, c).into_owned())
                            .unwrap_or_default();
                        let text = format!("import * as {alias} from {quoted}");
                        bindings.push(binding(None, alias, text));
                    }
                    "named_imports" => {
                        for spec in named_children(part)
                            .into_iter()
                            .filter(|s| s.kind() == "import_specifier")
                        {
                            let name = field_text(source, spec, "name");
                            let alias = {
                                let alias = field_text(source, spec, "alias");
                                if alias.is_empty() {
                                    name.clone()
                                } else {
                                    alias
                                }
                            };
                            let shown = if alias == name {
                                name.clone()
                            } else {
                                format!("{name} as {alias}")
                            };
                            let text = format!("import {{ {shown} }} from {quoted}");
                            bindings.push(binding(Some(name), alias, text));
                        }
                    }
                    _ => {}
                }
            }
        }
        if bindings.is_empty() {
            bindings.push(binding(None, String::new(), format!("import {quoted}")));
        }
        sites.push(ImportSite {
            start: line(node, true),
            end: line(node, false),
            bindings,
        });
    }
    sites
}
