//! Statement spans (one per object member or array element, nested) and key paths.

use tree_sitter::Node;

use crate::lang::{NodeRef, StmtKind, StmtSpan, Syntax};
use crate::text::Pos;

fn named<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect()
}

/// The members of a container (pairs of an object, elements of an array).
fn container_members(node: Node<'_>) -> Vec<Node<'_>> {
    match node.kind() {
        "object" | "array" => named(node),
        _ => Vec::new(),
    }
}

/// The value a member holds: a pair's value, or the element itself.
fn member_value(member: Node<'_>) -> Option<Node<'_>> {
    if member.kind() == "pair" {
        member.child_by_field_name("value")
    } else {
        Some(member)
    }
}

/// Top-level spans: the root container's members (or the root value itself when it is a
/// scalar), each with its nested members as children.
pub(super) fn statements(root: Node<'_>) -> Vec<StmtSpan> {
    let Some(value) = named(root).into_iter().next() else {
        return Vec::new();
    };
    match value.kind() {
        "object" | "array" => container_members(value).into_iter().map(span).collect(),
        _ => vec![span(value)],
    }
}

fn span(member: Node<'_>) -> StmtSpan {
    let children = member_value(member)
        .map(|v| container_members(v).into_iter().map(span).collect())
        .unwrap_or_default();
    StmtSpan {
        start: member.start_position().row as u32 + 1,
        end: member.end_position().row as u32 + 1,
        kind: StmtKind::Stmt,
        qualname: String::new(),
        node: Some(NodeRef::of(&member)),
        children,
    }
}

/// An object member or array element and where it sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Key path with concrete indices: `items[2].programId`; `""` is the root value.
    pub path: String,
    /// The same with every index as `[]`: `items[].programId`.
    pub norm: String,
    /// The containing member's `path`.
    pub parent: String,
    /// The key of a pair; `None` for array elements and the root.
    pub key: Option<String>,
    pub start: Pos,
    pub end: Pos,
    /// 1-based inclusive lines.
    pub first_line: u32,
    pub last_line: u32,
}

/// Every member of the document, the root first, then depth-first in source order.
pub fn members(syntax: &Syntax, lines: &[String]) -> Vec<Member> {
    let mut out = Vec::new();
    let root = syntax.tree.root_node();
    let Some(value) = named(root).into_iter().next() else {
        return out;
    };
    out.push(member(
        lines,
        value,
        String::new(),
        String::new(),
        String::new(),
        None,
    ));
    collect(syntax, lines, value, "", "", &mut out);
    out
}

fn member(
    lines: &[String],
    node: Node<'_>,
    path: String,
    norm: String,
    parent: String,
    key: Option<String>,
) -> Member {
    let (start, end) = super::node_span(lines, node);
    Member {
        path,
        norm,
        parent,
        key,
        start,
        end,
        first_line: node.start_position().row as u32 + 1,
        last_line: node.end_position().row as u32 + 1,
    }
}

fn collect(
    syntax: &Syntax,
    lines: &[String],
    container: Node<'_>,
    path: &str,
    norm: &str,
    out: &mut Vec<Member>,
) {
    for (i, child) in container_members(container).into_iter().enumerate() {
        let (seg, nseg, key) = if child.kind() == "pair" {
            let Some(key_node) = child.child_by_field_name("key") else {
                continue;
            };
            let raw = syntax.text_of(&key_node);
            let key = super::string_value(raw).unwrap_or_else(|| raw.to_string());
            let dot = if path.is_empty() { "" } else { "." };
            (format!("{dot}{key}"), format!("{dot}{key}"), Some(key))
        } else {
            (format!("[{i}]"), "[]".to_string(), None)
        };
        let (cpath, cnorm) = (format!("{path}{seg}"), format!("{norm}{nseg}"));
        out.push(member(
            lines,
            child,
            cpath.clone(),
            cnorm.clone(),
            path.to_string(),
            key,
        ));
        if let Some(value) = member_value(child) {
            collect(syntax, lines, value, &cpath, &cnorm, out);
        }
    }
}
