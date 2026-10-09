//! AST verification: parse a standalone block and dump a tree canonically.
//!
//! The dump is an S-expression over the CST that is invariant under formatting (quote style,
//! parentheses, trailing commas, comments, line breaks, numeric spelling), maps identifiers
//! through a rename table, drops docstrings and, on request, type annotations: the same
//! normalization the Python implementation applied to `ast` trees before `ast.dump`.

use std::collections::HashMap;

use tree_sitter::Node;

use super::super::pyrepr::{PyLiteral, eval_python_string, py_repr};
use super::super::{NodeRef, Syntax};
use super::{is_docstring_stmt, is_extra, is_format_string, text, tokens, unwrap_parens};

/// Parse a (possibly indented) block after `textwrap.dedent`; `None` on syntax errors.
pub(super) fn parse_block(text: &str) -> Option<Syntax> {
    let source = dedent(text);
    let tree = super::super::parse(&super::language(), &source)?;
    if tree.root_node().has_error() {
        return None;
    }
    let lines = crate::split_lines(&source);
    if tokens::tokenize(&tree, &source, &lines).indent_error {
        return None;
    }
    Some(Syntax { tree, source })
}

/// `textwrap.dedent`: remove the whitespace prefix common to every non-blank line; lines
/// made only of spaces and tabs become empty.
fn dedent(text: &str) -> String {
    let blank = |line: &str| line.chars().all(|c| c == ' ' || c == '\t');
    let mut margin: Option<String> = None;
    for line in text.split('\n') {
        if blank(line) {
            continue;
        }
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        margin = Some(match margin {
            None => indent,
            Some(m) => m
                .chars()
                .zip(indent.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    let margin = margin.unwrap_or_default();
    text.split('\n')
        .map(|line| {
            if blank(line) {
                ""
            } else {
                line.strip_prefix(margin.as_str()).unwrap_or(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn normalized_dump(
    syntax: &Syntax,
    node: Option<NodeRef>,
    renames: &HashMap<String, String>,
    strip_annotations: bool,
) -> String {
    let root = syntax.tree.root_node();
    let node = node.and_then(|r| syntax.node(r)).unwrap_or(root);
    let dumper = Dumper {
        src: &syntax.source,
        renames,
        strip: strip_annotations,
    };
    dumper.dump(node, true).unwrap_or_default()
}

/// Punctuation that carries no meaning beyond the node structure already dumped.
const PUNCTUATION: &[&str] = &["(", ")", "[", "]", "{", "}", ",", ";"];

struct Dumper<'a> {
    src: &'a str,
    renames: &'a HashMap<String, String>,
    strip: bool,
}

impl Dumper<'_> {
    /// `None` for nodes that leave no trace in the dump.
    fn dump(&self, node: Node<'_>, rename: bool) -> Option<String> {
        if node.is_missing() || is_extra(node) {
            return None;
        }
        match node.kind() {
            "parenthesized_expression" | "tuple_pattern" => {
                let inner = unwrap_parens(node);
                if inner != node {
                    return self.dump(inner, rename);
                }
            }
            "string" => return Some(self.string(node, rename)),
            "concatenated_string" => return Some(self.concatenated(node, rename)),
            "format_specifier" => return Some(self.format_spec(node, rename)),
            "module" => return Some(self.body(node, "module", true, rename)),
            "block" => return Some(self.body(node, "block", false, rename)),
            "function_definition" | "class_definition" => {
                return Some(self.definition(node, rename));
            }
            "expression_statement" if self.strip && is_bare_annotation(node) => return None,
            "assignment" if self.strip && node.child_by_field_name("type").is_some() => {
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                return Some(format!(
                    "(assignment {} = {})",
                    self.dump(left, rename)?,
                    self.dump(right, rename)?
                ));
            }
            "typed_parameter" if self.strip => {
                let mut c = node.walk();
                let pattern = node.named_children(&mut c).find(|c| !is_extra(*c))?;
                return self.dump(pattern, rename);
            }
            "typed_default_parameter" if self.strip => {
                let name = node.child_by_field_name("name")?;
                let value = node.child_by_field_name("value")?;
                return Some(format!(
                    "(default_parameter {} = {})",
                    self.dump(name, rename)?,
                    self.dump(value, rename)?
                ));
            }
            "identifier" => {
                let t = text(self.src, node);
                let name = if rename {
                    self.renames.get(t).map(String::as_str).unwrap_or(t)
                } else {
                    t
                };
                return Some(format!("(identifier {name})"));
            }
            "integer" | "float" => {
                // `1j` lexes as an integer and `1.0j` as a float; both are `complex(0, 1)`.
                let canon = canon_number(text(self.src, node));
                let kind = if canon.ends_with('j') {
                    "imaginary"
                } else {
                    node.kind()
                };
                return Some(format!("({kind} {canon})"));
            }
            _ => {}
        }
        if node.child_count() == 0 {
            if node.is_named() {
                return Some(format!("({} {})", node.kind(), text(self.src, node)));
            }
            let kind = node.kind();
            return (!PUNCTUATION.contains(&kind)).then(|| kind.to_string());
        }
        let module_name = (node.kind() == "import_from_statement")
            .then(|| node.child_by_field_name("module_name"))
            .flatten();
        let mut cursor = node.walk();
        let parts: Vec<String> = node
            .children(&mut cursor)
            .filter_map(|c| self.dump(c, rename && Some(c) != module_name))
            .collect();
        Some(sexp(alias_kind(node.kind()), parts))
    }

    /// A module or block body: the leading docstring of a module/class/def is dropped and an
    /// emptied body becomes `pass`, as `_Normalize._body` did.
    fn body(&self, node: Node<'_>, label: &str, def_body: bool, rename: bool) -> String {
        let mut cursor = node.walk();
        let mut first_stmt = true;
        let mut parts = Vec::new();
        for child in node.children(&mut cursor) {
            if is_extra(child) || !child.is_named() {
                if let Some(p) = self.dump(child, rename) {
                    parts.push(p);
                }
                continue;
            }
            let skip = def_body && first_stmt && is_docstring_stmt(self.src, child);
            first_stmt = false;
            if skip {
                continue;
            }
            if let Some(p) = self.dump(child, rename) {
                parts.push(p);
            }
        }
        if def_body && parts.is_empty() {
            parts.push("(pass_statement pass)".to_string());
        }
        sexp(label, parts)
    }

    fn definition(&self, node: Node<'_>, rename: bool) -> String {
        let body = node.child_by_field_name("body");
        let return_type = node.child_by_field_name("return_type");
        let mut cursor = node.walk();
        let mut parts = Vec::new();
        for child in node.children(&mut cursor) {
            if self.strip && (child.kind() == "->" || Some(child) == return_type) {
                continue;
            }
            let part = if Some(child) == body {
                Some(self.body(child, "block", true, rename))
            } else {
                self.dump(child, rename)
            };
            parts.extend(part);
        }
        sexp(node.kind(), parts)
    }

    /// A plain literal by its value; an f-/t-string by its pieces (the literal text of a format
    /// spec is not a CST node, so it is spliced in from the source).
    fn string(&self, node: Node<'_>, rename: bool) -> String {
        let t = text(self.src, node);
        if !is_format_string(self.src, node) {
            return match eval_python_string(t) {
                Some(lit) => format!("(string {})", py_repr(&lit)),
                None => format!("(string {t})"),
            };
        }
        let mut prefix: Vec<char> = node
            .child(0)
            .map(|c| text(self.src, c))
            .unwrap_or("")
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        prefix.sort_unstable();
        let mut parts = vec![prefix.into_iter().collect::<String>()];
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "string_start" | "string_end" => {}
                "string_content" => parts.push(format!("{:?}", text(self.src, child))),
                _ => parts.extend(self.dump(child, rename)),
            }
        }
        sexp("fstring", parts)
    }

    /// `:spec`: the literal pieces are not CST nodes (only nested `format_expression`s are),
    /// so they are spliced in from the source between the children.
    fn format_spec(&self, node: Node<'_>, rename: bool) -> String {
        let mut parts = Vec::new();
        let mut cur = node.start_byte();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == ":" && child.start_byte() == node.start_byte() {
                cur = child.end_byte();
                continue;
            }
            if cur < child.start_byte() {
                parts.push(format!("{:?}", &self.src[cur..child.start_byte()]));
            }
            parts.extend(self.dump(child, rename));
            cur = child.end_byte();
        }
        if cur < node.end_byte() {
            parts.push(format!("{:?}", &self.src[cur..node.end_byte()]));
        }
        sexp("format_specifier", parts)
    }

    fn concatenated(&self, node: Node<'_>, rename: bool) -> String {
        let mut cursor = node.walk();
        let pieces: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| !is_extra(*c))
            .collect();
        let values: Option<Vec<PyLiteral>> = pieces
            .iter()
            .map(|p| {
                (p.kind() == "string" && !is_format_string(self.src, *p))
                    .then(|| eval_python_string(text(self.src, *p)))
                    .flatten()
            })
            .collect();
        if let Some(values) = values {
            let folded = values.into_iter().try_fold(None, |acc, v| match (acc, v) {
                (None, v) => Some(Some(v)),
                (Some(PyLiteral::Str(mut a)), PyLiteral::Str(b)) => {
                    a.push_str(&b);
                    Some(Some(PyLiteral::Str(a)))
                }
                (Some(PyLiteral::Bytes(mut a)), PyLiteral::Bytes(b)) => {
                    a.extend(b);
                    Some(Some(PyLiteral::Bytes(a)))
                }
                _ => None,
            });
            if let Some(Some(lit)) = folded {
                return format!("(string {})", py_repr(&lit));
            }
        }
        let parts = pieces
            .iter()
            .filter_map(|p| self.dump(*p, rename))
            .collect();
        sexp("concatenated_string", parts)
    }
}

/// `x: T` without a value: removed entirely when annotations are stripped.
fn is_bare_annotation(stmt: Node<'_>) -> bool {
    let mut c = stmt.walk();
    let mut kids = stmt.named_children(&mut c).filter(|c| !is_extra(*c));
    match (kids.next(), kids.next()) {
        (Some(a), None) => {
            a.kind() == "assignment"
                && a.child_by_field_name("type").is_some()
                && a.child_by_field_name("right").is_none()
        }
        _ => false,
    }
}

fn sexp(kind: &str, parts: Vec<String>) -> String {
    if parts.is_empty() {
        format!("({kind})")
    } else {
        format!("({kind} {})", parts.join(" "))
    }
}

/// Shapes that are one AST node in CPython: `a, b` and `(a, b)` are both `Tuple`.
fn alias_kind(kind: &str) -> &str {
    match kind {
        "expression_list" => "tuple",
        "pattern_list" => "tuple_pattern",
        other => other,
    }
}

/// Canonical spelling of a numeric literal: underscores removed, hex/octal/binary integers in
/// decimal, floats (and imaginary parts) through an `f64` round trip.
fn canon_number(t: &str) -> String {
    let s: String = t
        .chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let (body, imag) = match s.strip_suffix('j') {
        Some(b) => (b, true),
        None => (s.as_str(), false),
    };
    let canon = if imag {
        canon_float(body).map(|f| format!("{f}j"))
    } else if let Some(h) = body.strip_prefix("0x") {
        u128::from_str_radix(h, 16).ok().map(|n| n.to_string())
    } else if let Some(o) = body.strip_prefix("0o") {
        u128::from_str_radix(o, 8).ok().map(|n| n.to_string())
    } else if let Some(b) = body.strip_prefix("0b") {
        u128::from_str_radix(b, 2).ok().map(|n| n.to_string())
    } else if body.contains(['.', 'e']) {
        canon_float(body)
    } else {
        body.parse::<u128>().ok().map(|n| n.to_string())
    };
    canon.unwrap_or(s)
}

fn canon_float(body: &str) -> Option<String> {
    body.parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .map(|v| format!("{v:?}"))
}

#[cfg(test)]
mod tests {
    use super::super::super::{AstVerifier, LanguageAnalyzer};
    use super::super::PythonAnalyzer;
    use super::*;

    fn dump_with(src: &str, renames: &[(&str, &str)], strip: bool) -> String {
        let py = PythonAnalyzer::new();
        let syntax = py
            .parse_block(src)
            .unwrap_or_else(|| panic!("parses: {src:?}"));
        let renames: HashMap<String, String> = renames
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        py.normalized_dump(&syntax, None, &renames, strip)
    }

    fn dump(src: &str) -> String {
        dump_with(src, &[], false)
    }

    fn same(a: &str, b: &str) {
        assert_eq!(dump(a), dump(b), "{a:?} vs {b:?}");
    }

    fn differ(a: &str, b: &str) {
        assert_ne!(dump(a), dump(b), "{a:?} vs {b:?}");
    }

    #[test]
    fn quote_style_and_concatenation() {
        same("x = 'a'\n", "x = \"a\"\n");
        same("x = 'a'\n", "x = \"\"\"a\"\"\"\n");
        same("x = 'it\\'s'\n", "x = \"it's\"\n");
        same("x = 'ab'\n", "x = 'a' \"b\"\n");
        same("x = ('a'\n     'b')\n", "x = 'ab'\n");
        same("x = b'ab'\n", "x = b'a' b'b'\n");
        differ("x = 'a'\n", "x = b'a'\n");
        differ("x = 'a'\n", "x = 'b'\n");
        same("x = f'{y}'\n", "x = f\"{y}\"\n");
        same("x = rf'{y}'\n", "x = Fr\"{y}\"\n");
        differ("x = f'{y:.2f}'\n", "x = f'{y:.3f}'\n");
        differ("x = f'{y!r}'\n", "x = f'{y}'\n");
        differ("x = f'{y}'\n", "x = f'{z}'\n");
        assert_eq!(
            dump("x = 'a'\n"),
            "(module (expression_statement (assignment (identifier x) = (string 'a'))))"
        );
    }

    #[test]
    fn trailing_commas_parentheses_and_layout() {
        same("f(a, b,)\n", "f(a, b)\n");
        same("x = [1, 2,]\n", "x = [1, 2]\n");
        same("f(a,\n  b)\n", "f(a, b)\n");
        same("x = (a)\n", "x = a\n");
        same("x = ((a))\n", "x = a\n");
        same("return_ = a, b\n", "return_ = (a, b)\n");
        same("for i, j in z: pass\n", "for (i, j) in z: pass\n");
        same("(y): int = 1\n", "y: int = 1\n");
        differ("x = (a,)\n", "x = a\n");
        same("x = 1 + \\\n    2\n", "x = 1 + 2\n");
        same("x = 1  # note\n", "x = 1\n");
        same(
            "def f():\n    x = 1\n    # type: ignore\n    return x\n",
            "def f():\n    x = 1\n    return x\n",
        );
        same("if x:\n    pass\n", "if x:\n\n    pass\n\n");
    }

    #[test]
    fn numeric_forms() {
        same("x = 0x10\n", "x = 16\n");
        same("x = 0o17\n", "x = 15\n");
        same("x = 0b11\n", "x = 3\n");
        same("x = 1_000\n", "x = 1000\n");
        same("x = 1.50\n", "x = 1.5\n");
        same("x = 1e3\n", "x = 1000.0\n");
        same("x = 1J\n", "x = 1j\n");
        same("x = 1.0j\n", "x = 1j\n");
        differ("x = 1\n", "x = 1.0\n");
        differ("x = 1\n", "x = 2\n");
        assert_eq!(canon_number("0XfF"), "255");
        assert_eq!(canon_number("1_0.5_0"), "10.5");
        assert_eq!(canon_number("1."), "1.0");
        assert_eq!(canon_number(".5"), "0.5");
    }

    #[test]
    fn docstrings_drop_and_empty_bodies_pass() {
        same("def f():\n    \"\"\"doc\"\"\"\n", "def f():\n    pass\n");
        same(
            "def f():\n    'doc'\n    return 1\n",
            "def f():\n    return 1\n",
        );
        same(
            "class C:\n    \"\"\"doc\"\"\"\n    x = 1\n",
            "class C:\n    x = 1\n",
        );
        same("\"\"\"module doc\"\"\"\nx = 1\n", "x = 1\n");
        same("\"\"\"module doc\"\"\"\n", "pass\n");
        same("def f():\n    ('doc')\n", "def f():\n    pass\n");
        // Only the first statement is a docstring; bytes are not docstrings.
        differ("def f():\n    x = 1\n    'doc'\n", "def f():\n    x = 1\n");
        differ("def f():\n    b'doc'\n", "def f():\n    pass\n");
        // Other blocks keep bare strings.
        differ("if x:\n    'doc'\n", "if x:\n    pass\n");
        assert_eq!(dump("'doc'\n"), "(module (pass_statement pass))");
    }

    #[test]
    fn renames_apply_everywhere_names_can_appear() {
        let r = &[("old", "new")];
        let eq = |a: &str, b: &str| assert_eq!(dump_with(a, r, false), dump(b), "{a:?}");
        eq("old\n", "new\n");
        eq("obj.old.x\n", "obj.new.x\n");
        eq("obj.x.old\n", "obj.x.new\n");
        eq("f(old=1)\n", "f(new=1)\n");
        eq(
            "def old(old: int = old) -> old:\n    return old(old)\n",
            "def new(new: int = new) -> new:\n    return new(new)\n",
        );
        eq(
            "class old(old, metaclass=old):\n    pass\n",
            "class new(new, metaclass=new):\n    pass\n",
        );
        eq("import a.old as old\n", "import a.new as new\n");
        eq("from m import old as old\n", "from m import new as new\n");
        eq(
            "def f():\n    global old\n    nonlocal old\n",
            "def f():\n    global new\n    nonlocal new\n",
        );
        eq(
            "try:\n    pass\nexcept E as old:\n    pass\n",
            "try:\n    pass\nexcept E as new:\n    pass\n",
        );
        eq("@old\ndef f():\n    pass\n", "@new\ndef f():\n    pass\n");
        eq(
            "def f(*old, **old):\n    pass\n",
            "def f(*new, **new):\n    pass\n",
        );
        // `ImportFrom.module` was never renamed by the Python implementation.
        assert_ne!(
            dump_with("from old import x\n", r, false),
            dump("from new import x\n")
        );
        assert_eq!(
            dump_with("from old import x\n", r, false),
            dump("from old import x\n")
        );
        // Strings are values, not names.
        assert_ne!(dump_with("x = 'old'\n", r, false), dump("x = 'new'\n"));
    }

    #[test]
    fn strip_annotations() {
        let s = |a: &str, b: &str| {
            assert_eq!(dump_with(a, &[], true), dump_with(b, &[], true), "{a:?}")
        };
        s("x: int = 1\n", "x = 1\n");
        s("self.x: int = 1\n", "self.x = 1\n");
        s("x: int\ny = 2\n", "y = 2\n");
        s("x: int\n", "pass\n");
        s("def f():\n    x: int\n", "def f():\n    pass\n");
        // A non-def block emptied by stripping stays empty (no `pass`), like `If(body=[])`.
        assert_eq!(
            dump_with("if c:\n    x: int\n", &[], true),
            "(module (if_statement if (identifier c) : (block)))"
        );
        s(
            "def f(a: int = 1, /, b: str, *c: T, d: int = 2, **e: U) -> R:\n    pass\n",
            "def f(a=1, /, b, *c, d=2, **e):\n    pass\n",
        );
        s(
            "async def f(x: int) -> None: ...\n",
            "async def f(x): ...\n",
        );
        s("lam = lambda x: x\n", "lam = lambda x: x\n");
        // Without stripping, annotations matter.
        differ("x: int = 1\n", "x = 1\n");
        differ("x: int = 1\n", "x: str = 1\n");
        differ("def f(x: int): pass\n", "def f(x): pass\n");
        differ("def f() -> int: pass\n", "def f(): pass\n");
        assert_ne!(
            dump_with("def f(x: int): pass\n", &[], true),
            dump("def f(x: str): pass\n")
        );
    }

    #[test]
    fn verify_scenarios_from_the_python_test_suite() {
        let before = "def show(uid):\n    user = get_user(uid)\n    print(user)\n";
        let after = before.replace("get_user", "fetch_user");
        assert_eq!(
            dump_with(before, &[("get_user", "fetch_user")], false),
            dump(&after)
        );
        assert_ne!(dump(before), dump(&after));
        let logic = after.replace("    print(user)", "    if user:\n        print(user)");
        assert_ne!(
            dump_with(before, &[("get_user", "fetch_user")], false),
            dump(&logic)
        );
        // Docstring change plus retype.
        let a = "def f(xs: List[int]) -> List[int]:\n    \"\"\"Old.\"\"\"\n    return xs\n";
        let b = a
            .replace("List[int]", "list[int]")
            .replace("Old.", "New words.");
        assert_eq!(dump_with(a, &[], true), dump_with(&b, &[], true));
        assert_ne!(dump(a), dump(&b));
        // Conflicting renames: one table cannot map `a` to both `b` and `c`.
        let f1 = "def f(a):\n    x = a\n    y = a\n    return x, y\n";
        let f2 = "def f(a):\n    x = b\n    y = c\n    return x, y\n";
        assert_ne!(dump_with(f1, &[("a", "b")], false), dump(f2));
        // Decorator rename.
        let d1 = "@old_dec\ndef f():\n    return 1\n";
        assert_eq!(
            dump_with(d1, &[("old_dec", "new_dec")], false),
            dump(&d1.replace("old_dec", "new_dec"))
        );
    }

    #[test]
    fn statement_nodes_dump_through_node_refs() {
        let py = PythonAnalyzer::new();
        let a = py.analyze(
            "def f(a):\n    \"\"\"doc\"\"\"\n    return (a)\n\n\ndef g(b):\n    return b\n",
        );
        let syntax = a.syntax.as_ref().expect("parsed");
        let f = a.statements[0].node.expect("node");
        let g = a.statements[1].node.expect("node");
        let renames = HashMap::from([
            ("f".to_string(), "g".to_string()),
            ("a".to_string(), "b".to_string()),
        ]);
        let none = HashMap::new();
        assert_eq!(
            py.normalized_dump(syntax, Some(f), &renames, false),
            py.normalized_dump(syntax, Some(g), &none, false)
        );
        assert_ne!(
            py.normalized_dump(syntax, Some(f), &none, false),
            py.normalized_dump(syntax, Some(g), &none, false)
        );
    }

    #[test]
    fn parse_block_dedents_and_rejects_errors() {
        let py = PythonAnalyzer::new();
        assert!(
            py.parse_block("    x = 1\n    if x:\n        y = 2\n")
                .is_some()
        );
        assert!(py.parse_block("\tx = 1\n\n\ty = 2").is_some());
        assert!(py.parse_block("x = (\n").is_none());
        assert!(py.parse_block("if a:\n        b\n    c\n").is_none());
        assert!(py.parse_block("").is_some());
        assert_eq!(dedent("    a\n  \n\t\n    b\n"), "a\n\n\nb\n");
        assert_eq!(dedent("  \ta\n  \tb\n"), "a\nb\n");
        assert_eq!(dedent("    a\n\n      b"), "a\n\n  b");
        assert_eq!(dedent("  a\n b\n"), " a\nb\n");
        assert_eq!(dedent(""), "");
    }
}
