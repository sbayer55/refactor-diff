//! Parity of the Python analyzer with the CPython `tokenize`/`ast` implementation it replaces,
//! against `tests/goldens/python_tokens.json` (see `tests/goldens/README.md`).
//!
//! Every snippet is compared field by field. The deliberate divergences of the port are
//! encoded explicitly below (see `UNPARSED`), with the Rust behaviour asserted in their place.

use std::collections::BTreeSet;
use std::fmt::Debug;

use pretty_assertions::Comparison;
use refactor_diff_core::{ArgKeyword, FileAnalysis, LanguageAnalyzer, StmtSpan, python};
use serde::Deserialize;

const GOLDENS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/goldens");

type P = (u32, u32);

#[derive(Deserialize, Debug, PartialEq, Clone)]
struct Tok {
    kind: String,
    value: String,
    text: String,
    start: P,
    end: P,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Ann {
    start: P,
    end: P,
    text: String,
    target: String,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Stmt {
    start: u32,
    end: u32,
    kind: String,
    qualname: String,
    children: Vec<Stmt>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Call {
    name: String,
    start: P,
    end: P,
    args_start: P,
    args_end: P,
    args: Vec<(P, P, Option<String>)>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Param {
    name: String,
    kind: String,
    has_default: bool,
    start: P,
    end: P,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Def {
    name: String,
    start: P,
    end: P,
    params_start: P,
    params_end: P,
    params: Vec<Param>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Bind {
    module: String,
    name: Option<String>,
    alias: String,
    level: u32,
    text: String,
}

#[derive(Deserialize, Debug, PartialEq)]
struct Import {
    start: u32,
    end: u32,
    bindings: Vec<Bind>,
}

#[derive(Deserialize)]
struct Snippet {
    name: String,
    source: String,
    parsed: bool,
    tokens: Vec<Tok>,
    annotations: Vec<Ann>,
    docstrings: Vec<(P, P)>,
    statements: Vec<Stmt>,
    calls: Vec<Call>,
    defs: Vec<Def>,
    imports: Vec<Import>,
}

#[derive(Deserialize)]
struct Builtins {
    builtins: Vec<String>,
    keywords: Vec<String>,
    softkeywords: Vec<String>,
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let text = std::fs::read_to_string(format!("{GOLDENS}/{name}")).expect("golden file");
    serde_json::from_str(&text).expect("golden json")
}

/// Snippets the Python tokenizer could not tokenize (`parsed: false`, regex fallback tokens
/// with no structural tokens). The port tokenizes them from tree-sitter's error-recovering
/// parse instead, so only `parsed` and the plain tokens' sanity are pinned.
const UNPARSED: &[&str] = &[
    "syntax_error_unclosed_paren",
    "indentation_error",
    "unterminated_triple_quote",
];

fn p(pos: refactor_diff_core::Pos) -> P {
    (pos.line, pos.col)
}

fn tokens(an: &FileAnalysis) -> Vec<Tok> {
    an.tokens
        .iter()
        .map(|t| Tok {
            kind: t.kind.as_str().to_string(),
            value: t.value.clone(),
            text: t.text.clone(),
            start: p(t.start),
            end: p(t.end),
        })
        .collect()
}

fn annotations(an: &FileAnalysis) -> Vec<Ann> {
    an.annotations
        .iter()
        .map(|a| Ann {
            start: p(a.start),
            end: p(a.end),
            text: a.text.clone(),
            target: a.target.clone(),
        })
        .collect()
}

fn statements(spans: &[StmtSpan]) -> Vec<Stmt> {
    spans
        .iter()
        .map(|s| Stmt {
            start: s.start,
            end: s.end,
            kind: s.kind.as_str().to_string(),
            qualname: s.qualname.clone(),
            children: statements(&s.children),
        })
        .collect()
}

fn calls(an: &FileAnalysis) -> Vec<Call> {
    an.calls
        .iter()
        .map(|c| Call {
            name: c.name.clone(),
            start: p(c.start),
            end: p(c.end),
            args_start: p(c.args_start),
            args_end: p(c.args_end),
            args: c
                .args
                .iter()
                .map(|a| {
                    let kw = match &a.keyword {
                        ArgKeyword::Positional => None,
                        ArgKeyword::Star => Some("*".to_string()),
                        ArgKeyword::StarStar => Some("**".to_string()),
                        ArgKeyword::Named(n) => Some(n.clone()),
                    };
                    (p(a.start), p(a.end), kw)
                })
                .collect(),
        })
        .collect()
}

fn defs(an: &FileAnalysis) -> Vec<Def> {
    an.defs
        .iter()
        .map(|d| Def {
            name: d.name.clone(),
            start: p(d.start),
            end: p(d.end),
            params_start: p(d.params_start),
            params_end: p(d.params_end),
            params: d
                .params
                .iter()
                .map(|q| Param {
                    name: q.name.clone(),
                    kind: q.kind.as_str().to_string(),
                    has_default: q.has_default,
                    start: p(q.start),
                    end: p(q.end),
                })
                .collect(),
        })
        .collect()
}

fn imports(an: &FileAnalysis) -> Vec<Import> {
    an.imports
        .iter()
        .map(|i| Import {
            start: i.start,
            end: i.end,
            bindings: i
                .bindings
                .iter()
                .map(|b| Bind {
                    module: b.module.clone(),
                    name: b.name.clone(),
                    alias: b.alias.clone(),
                    level: b.level,
                    text: b.text.clone(),
                })
                .collect(),
        })
        .collect()
}

/// Record a mismatch with a readable diff instead of stopping at the first one.
fn check<T: PartialEq + Debug>(
    failures: &mut Vec<String>,
    name: &str,
    field: &str,
    got: &T,
    want: &T,
) {
    if got != want {
        failures.push(format!(
            "--- {name}: {field} (left = Rust, right = golden)\n{}",
            Comparison::new(got, want)
        ));
    }
}

#[test]
fn python_analysis_matches_cpython_goldens() {
    let snippets: Vec<Snippet> = load("python_tokens.json");
    assert_eq!(snippets.len(), 21);
    let py = python::PythonAnalyzer::new();
    let mut failures = Vec::new();
    let mut exact = Vec::new();

    for s in &snippets {
        let an = py.analyze(&s.source);
        let before = failures.len();
        if UNPARSED.contains(&s.name.as_str()) {
            // Divergence: CPython's tokenizer failed and the regex fallback tokenizer ran; the
            // port's tokens come from tree-sitter's error-recovering parse (with structural
            // tokens where its line scan can place them). Require the same `parsed` verdict,
            // empty structure, and that the plain tokens cover the golden's names and operators.
            check(&mut failures, &s.name, "parsed", &an.parsed, &false);
            assert!(an.syntax.is_none() && an.statements.is_empty() && an.defs.is_empty());
            assert!(an.calls.is_empty() && an.imports.is_empty() && an.annotations.is_empty());
            let got: BTreeSet<(String, P)> = tokens(&an)
                .into_iter()
                .filter(|t| t.kind == "name" || t.kind == "op" || t.kind == "number")
                .map(|t| (t.value, t.start))
                .collect();
            let want: BTreeSet<(String, P)> = s
                .tokens
                .iter()
                .filter(|t| t.kind == "name" || t.kind == "number")
                .map(|t| (t.value.clone(), t.start))
                .collect();
            let missing: Vec<_> = want.difference(&got).collect();
            check(
                &mut failures,
                &s.name,
                "names/numbers present",
                &missing,
                &vec![],
            );
            assert!(!an.tokens.is_empty(), "{}: tokens", s.name);
        } else {
            check(&mut failures, &s.name, "parsed", &an.parsed, &s.parsed);
            check(&mut failures, &s.name, "tokens", &tokens(&an), &s.tokens);
            check(
                &mut failures,
                &s.name,
                "annotations",
                &annotations(&an),
                &s.annotations,
            );
            let docs: Vec<(P, P)> = an.docstrings.iter().map(|(a, b)| (p(*a), p(*b))).collect();
            check(&mut failures, &s.name, "docstrings", &docs, &s.docstrings);
            check(
                &mut failures,
                &s.name,
                "statements",
                &statements(&an.statements),
                &s.statements,
            );
            check(&mut failures, &s.name, "calls", &calls(&an), &s.calls);
            check(&mut failures, &s.name, "defs", &defs(&an), &s.defs);
            check(&mut failures, &s.name, "imports", &imports(&an), &s.imports);
            assert_eq!(an.syntax.is_some(), an.parsed, "{}: syntax", s.name);
            assert_eq!(an.lines, refactor_diff_core::split_lines(&s.source));
            // Every statement node must re-locate to a node of the recorded kind.
            let syntax = an.syntax.as_ref().expect("parsed");
            for span in an.flat_statements() {
                let node = span.node.expect("node ref");
                let found = syntax.node(node).expect("node relocates");
                assert_eq!(
                    found.kind_id(),
                    node.kind_id,
                    "{}: {:?}",
                    s.name,
                    span.qualname
                );
            }
        }
        if failures.len() == before {
            exact.push(s.name.as_str());
        }
    }
    eprintln!(
        "snippets without divergence: {}/{}: {}",
        exact.len(),
        snippets.len(),
        exact.join(", ")
    );
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n\n"));
}

#[test]
fn keyword_and_builtin_tables_match_cpython() {
    let b: Builtins = load("builtins.json");
    let py = python::PythonAnalyzer::new();
    for k in b.keywords.iter().chain(&b.softkeywords) {
        assert!(py.is_keyword(k), "keyword {k}");
    }
    for k in &b.builtins {
        assert!(py.is_builtin(k), "builtin {k}");
    }
    for word in ["foo", "self", "cls", "Foo", "print_", "", "def "] {
        assert!(!py.is_keyword(word) && !py.is_builtin(word), "{word:?}");
    }
    // Keywords and builtins are disjoint except the constants.
    let kw: BTreeSet<&String> = b.keywords.iter().collect();
    let common: Vec<&String> = b.builtins.iter().filter(|x| kw.contains(x)).collect();
    assert_eq!(common.len(), 3, "{common:?}");
}
