//! Analyzer-level behaviour of the TypeScript / TSX / JavaScript analyzers, ported from
//! `tests/test_typescript.py`. Expected values were pinned against the Python analyzer on the
//! same grammar versions (tree-sitter-typescript 0.23.2, tree-sitter-javascript 0.25.0).
//!
//! The classify-level cases of `tests/test_typescript.py` (they go through `patterns.classify`
//! and belong with the classify port) are, for the record:
//!
//! - test_call_rename: `const x = getUser(1);` -> `fetchUser` => (rename, getUser, fetchUser, call)
//! - test_definition_rename: `export function getUser() {}` => (rename, ..., definition)
//! - test_const_definition_rename: `const limit = 1;` -> `maxItems` => (rename, ..., definition)
//! - test_import_rename: `import { getUser } from "./u";` => (rename, ..., import)
//! - test_import_after_other_statements: same, preceded by `const a = 1;`
//! - test_attribute_rename: `y = obj.oldName` -> `newName` => (rename, ..., attribute)
//! - test_param_retype: `function f(x: number) {}` -> `string` => (retype, number, string, param x)
//! - test_optional_param_retype: `function f(x?: number) {}` => (retype, ..., param x)
//! - test_return_retype: `function f(): Promise<void> {}` => (retype, ..., return of f)
//! - test_arrow_return_retype: `const f = (a: number): number => a;` => (retype, ..., return of f)
//! - test_variable_and_property_retype: `let z: Map<string, number>;` => variable z;
//!   `interface I { p: number }` => property p
//! - test_quotes_and_semicolons_are_formatting: `const a = 'hi'\nf(\`x\`)` vs
//!   `const a = "hi";\nf("x");` => formatting
//! - test_escapes_compare_by_value: `const a = 'it\'s'` vs `const a = "it's"` => formatting
//! - test_reindent_is_formatting: `if (a) {\n  b();\n}` vs four-space indent => formatting
//! - test_comment_only_change_is_docs: `/** Old docs. */` -> `/** New docs. */` => docs
//! - test_renames_inside_template_substitutions: `` `id ${getUser()}` `` => (rename, ..., call)
//! - test_import_path_change: `import { a } from "./old";` -> `"./new"` => (import, ./old, ./new, a)

use pretty_assertions::assert_eq;
use refactor_diff_core::{
    FileAnalysis, LanguageAnalyzer, StmtKind, StmtSpan, TokenKind, analyzer_for,
};

fn ts() -> &'static dyn LanguageAnalyzer {
    analyzer_for("a.ts").expect("typescript analyzer")
}

fn analyze(path: &str, text: &str) -> FileAnalysis {
    analyzer_for(path).expect("an analyzer").analyze(text)
}

type Tok = (&'static str, String, String, (u32, u32), (u32, u32));

fn tokens(an: &FileAnalysis) -> Vec<Tok> {
    an.tokens
        .iter()
        .map(|t| {
            (
                t.kind.as_str(),
                t.value.clone(),
                t.text.clone(),
                (t.start.line, t.start.col),
                (t.end.line, t.end.col),
            )
        })
        .collect()
}

fn tok(kind: &'static str, value: &str, text: &str, start: (u32, u32), end: (u32, u32)) -> Tok {
    (kind, value.to_string(), text.to_string(), start, end)
}

fn values(an: &FileAnalysis) -> Vec<&str> {
    an.tokens.iter().map(|t| t.value.as_str()).collect()
}

/// `(start, end, text, target)` of an annotation.
type Ann<'a> = ((u32, u32), (u32, u32), &'a str, &'a str);

fn annotations(an: &FileAnalysis) -> Vec<Ann<'_>> {
    an.annotations
        .iter()
        .map(|a| {
            (
                (a.start.line, a.start.col),
                (a.end.line, a.end.col),
                a.text.as_str(),
                a.target.as_str(),
            )
        })
        .collect()
}

fn spans(statements: &[StmtSpan]) -> Vec<(u32, u32, &'static str, &str)> {
    statements
        .iter()
        .map(|s| (s.start, s.end, s.kind.as_str(), s.qualname.as_str()))
        .collect()
}

// --- dispatch ----------------------------------------------------------------------------

#[test]
fn dialects_by_extension() {
    let names: Vec<&str> = ["a.ts", "b.mts", "c.tsx", "d.js", "e.jsx", "f.cjs"]
        .iter()
        .map(|p| analyzer_for(p).expect("handled").name())
        .collect();
    assert_eq!(
        names,
        [
            "typescript",
            "typescript",
            "tsx",
            "javascript",
            "javascript",
            "javascript"
        ]
    );
    assert_eq!(analyzer_for("a.d.ts").unwrap().name(), "typescript");
    assert_eq!(analyzer_for("a.cts").unwrap().name(), "typescript");
    assert_eq!(analyzer_for("a.mjs").unwrap().name(), "javascript");
    assert!(ts().globs().contains(&"*.js"));
    assert!(analyzer_for("a.js").unwrap().globs().contains(&"*.tsx"));
    assert!(ts().verifier().is_none());
    assert_eq!(ts().import_keywords(), ["import"]);
    assert_eq!(
        ts().definition_keywords(),
        [
            "function",
            "class",
            "interface",
            "type",
            "enum",
            "namespace",
            "const",
            "let",
            "var"
        ]
    );
}

#[test]
fn builtins_and_keywords() {
    assert!(ts().is_builtin("Promise"));
    assert!(ts().is_builtin("console"));
    assert!(ts().is_builtin("__dirname"));
    assert!(!ts().is_builtin("getUser"));
    assert!(ts().is_keyword("interface"));
    assert!(ts().is_keyword("satisfies"));
    assert!(!ts().is_keyword("getUser"));
    // The three dialects share both sets.
    let js = analyzer_for("a.js").unwrap();
    assert!(js.is_keyword("interface") && js.is_builtin("Promise"));
}

// --- tokens ------------------------------------------------------------------------------

#[test]
fn statements_end_with_structural_tokens() {
    let an = analyze("a.ts", "a();\nb()\n");
    let structural: Vec<bool> = an
        .tokens
        .iter()
        .map(|t| t.kind == TokenKind::Structural)
        .collect();
    assert_eq!(
        structural,
        [false, false, false, true, false, false, false, true]
    );
    // The terminating `;` folds into the line end, which sits after it.
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "a", "a", (1, 0), (1, 1)),
            tok("op", "(", "(", (1, 1), (1, 2)),
            tok("op", ")", ")", (1, 2), (1, 3)),
            tok("structural", "<NEWLINE>", "", (1, 4), (1, 4)),
            tok("name", "b", "b", (2, 0), (2, 1)),
            tok("op", "(", "(", (2, 1), (2, 2)),
            tok("op", ")", ")", (2, 2), (2, 3)),
            tok("structural", "<NEWLINE>", "", (2, 3), (2, 3)),
        ]
    );
}

#[test]
fn columns_count_characters() {
    let an = analyze("a.ts", "const é = \"ü\"; const name = 1;\n");
    let name = &an.tokens[6];
    assert_eq!(name.value, "name");
    assert_eq!((name.start.line, name.start.col), (1, 21));
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "const", "const", (1, 0), (1, 5)),
            // A non-ASCII-led identifier is not a name for the tokenizer.
            tok("op", "é", "é", (1, 6), (1, 7)),
            tok("op", "=", "=", (1, 8), (1, 9)),
            tok("string", "'ü'", "\"ü\"", (1, 10), (1, 13)),
            tok("structural", "<NEWLINE>", "", (1, 14), (1, 14)),
            tok("name", "const", "const", (1, 15), (1, 20)),
            tok("name", "name", "name", (1, 21), (1, 25)),
            tok("op", "=", "=", (1, 26), (1, 27)),
            tok("number", "1", "1", (1, 28), (1, 29)),
            tok("structural", "<NEWLINE>", "", (1, 30), (1, 30)),
        ]
    );
}

#[test]
fn identifier_shapes() {
    let an = analyze("a.ts", "let 日本 = 1; let a日 = 2; let $x = 3;\n");
    let kinds: Vec<(&str, &str)> = an
        .tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Structural)
        .map(|t| (t.kind.as_str(), t.value.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("name", "let"),
            ("op", "日本"),
            ("op", "="),
            ("number", "1"),
            ("name", "let"),
            ("name", "a日"),
            ("op", "="),
            ("number", "2"),
            ("name", "let"),
            ("name", "$x"),
            ("op", "="),
            ("number", "3"),
        ]
    );
}

#[test]
fn quotes_normalize_and_semicolons_fold() {
    let old = analyze("a.ts", "const a = 'hi'\nf(`x`)\n");
    let new = analyze("a.ts", "const a = \"hi\";\nf(\"x\");\n");
    assert_eq!(
        tokens(&old),
        vec![
            tok("name", "const", "const", (1, 0), (1, 5)),
            tok("name", "a", "a", (1, 6), (1, 7)),
            tok("op", "=", "=", (1, 8), (1, 9)),
            tok("string", "'hi'", "'hi'", (1, 10), (1, 14)),
            tok("structural", "<NEWLINE>", "", (1, 14), (1, 14)),
            tok("name", "f", "f", (2, 0), (2, 1)),
            tok("op", "(", "(", (2, 1), (2, 2)),
            tok("string", "'x'", "`x`", (2, 2), (2, 5)),
            tok("op", ")", ")", (2, 5), (2, 6)),
            tok("structural", "<NEWLINE>", "", (2, 6), (2, 6)),
        ]
    );
    let vals = |an: &FileAnalysis| -> Vec<(&'static str, String)> {
        an.tokens
            .iter()
            .map(|t| (t.kind.as_str(), t.value.clone()))
            .collect()
    };
    assert_eq!(vals(&old), vals(&new));
}

#[test]
fn escapes_compare_by_value() {
    let a = analyze("a.ts", "const a = 'it\\'s'\n");
    let b = analyze("a.ts", "const a = \"it's\"\n");
    assert_eq!(
        tokens(&a)[3],
        tok("string", "\"it's\"", "'it\\'s'", (1, 10), (1, 17))
    );
    assert_eq!(
        tokens(&b)[3],
        tok("string", "\"it's\"", "\"it's\"", (1, 10), (1, 16))
    );
}

#[test]
fn string_escapes_normalize_to_python_repr() {
    let an = analyze(
        "a.ts",
        "const e = \"\\n\\t\\x41\\u0042\\u{1F600}\\q\\\\\\\"\";\nconst f = 'a\\\nb';\n",
    );
    assert_eq!(
        tokens(&an)[3],
        tok(
            "string",
            "'\\n\\tAB😀q\\\\\"'",
            "\"\\n\\t\\x41\\u0042\\u{1F600}\\q\\\\\\\"\"",
            (1, 10),
            (1, 41),
        )
    );
    // A backslash-newline continuation disappears; the token spans both lines.
    assert_eq!(
        tokens(&an)[8],
        tok("string", "'ab'", "'a\\\nb'", (2, 10), (3, 2))
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 1, "stmt", ""), (2, 3, "stmt", "")]
    );

    let an = analyze(
        "a.ts",
        "const s3 = `\\u{41}`;\nconst s4 = \"\\\\\";\nconst s6 = \"\\u{}\\u12\\x4\";\nconst s7 = '\\r\\0\\b\\f\\v';\n",
    );
    let strings: Vec<(&str, &str)> = an
        .tokens
        .iter()
        .filter(|t| t.kind == TokenKind::String)
        .map(|t| (t.value.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(
        strings,
        vec![
            ("'A'", "`\\u{41}`"),
            ("'\\\\'", "\"\\\\\""),
            // Malformed escapes keep the escaped character.
            ("'u{}u12x4'", "\"\\u{}\\u12\\x4\""),
            ("'\\r\\x00\\x08\\x0c\\x0b'", "'\\r\\0\\b\\f\\v'"),
        ]
    );
}

#[test]
fn string_with_impossible_code_point_keeps_raw_text() {
    let an = analyze("a.ts", "const s1 = 'a\\u{110000}b';\n");
    let s = an
        .tokens
        .iter()
        .find(|t| t.kind == TokenKind::String)
        .unwrap();
    assert_eq!(s.value, "'a\\u{110000}b'");
    assert_eq!(s.value, s.text);
}

#[test]
fn predefined_string_type_is_tokenized_as_a_string() {
    // The `string` keyword's node type is also "string", so the tokenizer treats it as a
    // quoted literal and strips its first and last characters. Pinned for parity.
    let an = analyze("a.ts", "let z: string;\n");
    assert_eq!(
        tokens(&an)[3],
        tok("string", "'trin'", "string", (1, 7), (1, 13))
    );
}

#[test]
fn template_strings() {
    let an = analyze(
        "a.ts",
        "const s = `id ${getUser()}`;\nconst t = `a\\`b\\n${x}c`;\n",
    );
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "const", "const", (1, 0), (1, 5)),
            tok("name", "s", "s", (1, 6), (1, 7)),
            tok("op", "=", "=", (1, 8), (1, 9)),
            // A substituted template is walked: fragments are strings, the rest is code.
            tok("op", "`", "`", (1, 10), (1, 11)),
            tok("string", "id ", "id ", (1, 11), (1, 14)),
            tok("op", "${", "${", (1, 14), (1, 16)),
            tok("name", "getUser", "getUser", (1, 16), (1, 23)),
            tok("op", "(", "(", (1, 23), (1, 24)),
            tok("op", ")", ")", (1, 24), (1, 25)),
            tok("op", "}", "}", (1, 25), (1, 26)),
            tok("op", "`", "`", (1, 26), (1, 27)),
            tok("structural", "<NEWLINE>", "", (1, 28), (1, 28)),
            tok("name", "const", "const", (2, 0), (2, 5)),
            tok("name", "t", "t", (2, 6), (2, 7)),
            tok("op", "=", "=", (2, 8), (2, 9)),
            tok("op", "`", "`", (2, 10), (2, 11)),
            tok("string", "a", "a", (2, 11), (2, 12)),
            tok("op", "\\`", "\\`", (2, 12), (2, 14)),
            tok("string", "b", "b", (2, 14), (2, 15)),
            tok("op", "\\n", "\\n", (2, 15), (2, 17)),
            tok("op", "${", "${", (2, 17), (2, 19)),
            tok("name", "x", "x", (2, 19), (2, 20)),
            tok("op", "}", "}", (2, 20), (2, 21)),
            tok("string", "c", "c", (2, 21), (2, 22)),
            tok("op", "`", "`", (2, 22), (2, 23)),
            tok("structural", "<NEWLINE>", "", (2, 24), (2, 24)),
        ]
    );
    // A plain template is one normalized string, with escaped backticks unescaped.
    let an = analyze("a.ts", "f(`a\\`b`)\n");
    assert_eq!(
        tokens(&an)[2],
        tok("string", "'a`b'", "`a\\`b`", (1, 2), (1, 8))
    );
}

#[test]
fn comments_regexes_numbers_and_shebang() {
    let an = analyze(
        "a.ts",
        "// comment   \n/* block */\n#!/usr/bin/env node\nconst r = /ab+c/gi;\nconst n = 0x1F;\nx.y = 1n;\n",
    );
    assert_eq!(
        tokens(&an),
        vec![
            tok("comment", "// comment", "// comment   ", (1, 0), (1, 13)),
            tok("structural", "<NEWLINE>", "", (1, 13), (1, 13)),
            tok("comment", "/* block */", "/* block */", (2, 0), (2, 11)),
            tok("structural", "<NEWLINE>", "", (2, 11), (2, 11)),
            tok(
                "comment",
                "#!/usr/bin/env node",
                "#!/usr/bin/env node",
                (3, 0),
                (3, 19),
            ),
            tok("structural", "<NEWLINE>", "", (3, 19), (3, 19)),
            tok("name", "const", "const", (4, 0), (4, 5)),
            tok("name", "r", "r", (4, 6), (4, 7)),
            tok("op", "=", "=", (4, 8), (4, 9)),
            tok("string", "/ab+c/gi", "/ab+c/gi", (4, 10), (4, 18)),
            tok("structural", "<NEWLINE>", "", (4, 19), (4, 19)),
            tok("name", "const", "const", (5, 0), (5, 5)),
            tok("name", "n", "n", (5, 6), (5, 7)),
            tok("op", "=", "=", (5, 8), (5, 9)),
            tok("number", "0x1F", "0x1F", (5, 10), (5, 14)),
            tok("structural", "<NEWLINE>", "", (5, 15), (5, 15)),
            tok("name", "x", "x", (6, 0), (6, 1)),
            tok("op", ".", ".", (6, 1), (6, 2)),
            tok("name", "y", "y", (6, 2), (6, 3)),
            tok("op", "=", "=", (6, 4), (6, 5)),
            tok("number", "1n", "1n", (6, 6), (6, 8)),
            tok("structural", "<NEWLINE>", "", (6, 9), (6, 9)),
        ]
    );
    // Comments are not statements, but the shebang line is.
    assert_eq!(
        spans(&an.statements),
        vec![
            (3, 3, "stmt", ""),
            (4, 4, "stmt", ""),
            (5, 5, "stmt", ""),
            (6, 6, "stmt", ""),
        ]
    );
}

#[test]
fn html_comment_in_javascript() {
    let an = analyze("a.js", "<!-- html -->\nconst a = 1;\n");
    assert!(an.parsed);
    assert_eq!(
        tokens(&an)[0],
        tok("comment", "<!-- html -->", "<!-- html -->", (1, 0), (1, 13))
    );
    assert_eq!(
        tokens(&an)[1],
        tok("structural", "<NEWLINE>", "", (1, 13), (1, 13))
    );
}

#[test]
fn body_semicolons_are_dropped_but_for_header_semicolons_kept() {
    let an = analyze(
        "a.ts",
        "class C {\n  ;\n  m() {}\n  n = 1;\n}\nfor (;;) {}\n;\n",
    );
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "class", "class", (1, 0), (1, 5)),
            tok("name", "C", "C", (1, 6), (1, 7)),
            tok("op", "{", "{", (1, 8), (1, 9)),
            tok("name", "m", "m", (3, 2), (3, 3)),
            tok("op", "(", "(", (3, 3), (3, 4)),
            tok("op", ")", ")", (3, 4), (3, 5)),
            tok("op", "{", "{", (3, 6), (3, 7)),
            tok("op", "}", "}", (3, 7), (3, 8)),
            tok("structural", "<NEWLINE>", "", (3, 8), (3, 8)),
            tok("name", "n", "n", (4, 2), (4, 3)),
            tok("op", "=", "=", (4, 4), (4, 5)),
            tok("number", "1", "1", (4, 6), (4, 7)),
            tok("structural", "<NEWLINE>", "", (4, 7), (4, 7)),
            tok("op", "}", "}", (5, 0), (5, 1)),
            tok("structural", "<NEWLINE>", "", (5, 1), (5, 1)),
            tok("name", "for", "for", (6, 0), (6, 3)),
            tok("op", "(", "(", (6, 4), (6, 5)),
            tok("op", ";", ";", (6, 5), (6, 6)),
            tok("op", ";", ";", (6, 6), (6, 7)),
            tok("op", ")", ")", (6, 7), (6, 8)),
            tok("op", "{", "{", (6, 9), (6, 10)),
            tok("op", "}", "}", (6, 10), (6, 11)),
            tok("structural", "<NEWLINE>", "", (6, 11), (6, 11)),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 5, "class", "C"), (6, 6, "stmt", ""), (7, 7, "stmt", "")]
    );
    assert_eq!(
        spans(&an.statements[0].children),
        vec![(3, 3, "def", "C.m"), (4, 4, "stmt", "")]
    );
}

#[test]
fn switch_and_enum_bodies_end_lines() {
    let an = analyze(
        "a.ts",
        "switch (x) {\n  case 1:\n    a();\n    break\n  default:\n    b();\n}\nenum E {\n  A = 1,\n  B\n}\n",
    );
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "switch", "switch", (1, 0), (1, 6)),
            tok("op", "(", "(", (1, 7), (1, 8)),
            tok("name", "x", "x", (1, 8), (1, 9)),
            tok("op", ")", ")", (1, 9), (1, 10)),
            tok("op", "{", "{", (1, 11), (1, 12)),
            tok("name", "case", "case", (2, 2), (2, 6)),
            tok("number", "1", "1", (2, 7), (2, 8)),
            tok("structural", "<NEWLINE>", "", (2, 8), (2, 8)),
            tok("op", ":", ":", (2, 8), (2, 9)),
            tok("name", "a", "a", (3, 4), (3, 5)),
            tok("op", "(", "(", (3, 5), (3, 6)),
            tok("op", ")", ")", (3, 6), (3, 7)),
            tok("structural", "<NEWLINE>", "", (3, 8), (3, 8)),
            tok("name", "break", "break", (4, 4), (4, 9)),
            tok("structural", "<NEWLINE>", "", (4, 9), (4, 9)),
            tok("name", "default", "default", (5, 2), (5, 9)),
            tok("op", ":", ":", (5, 9), (5, 10)),
            tok("name", "b", "b", (6, 4), (6, 5)),
            tok("op", "(", "(", (6, 5), (6, 6)),
            tok("op", ")", ")", (6, 6), (6, 7)),
            tok("structural", "<NEWLINE>", "", (6, 8), (6, 8)),
            tok("op", "}", "}", (7, 0), (7, 1)),
            tok("structural", "<NEWLINE>", "", (7, 1), (7, 1)),
            tok("name", "enum", "enum", (8, 0), (8, 4)),
            tok("name", "E", "E", (8, 5), (8, 6)),
            tok("op", "{", "{", (8, 7), (8, 8)),
            tok("name", "A", "A", (9, 2), (9, 3)),
            tok("op", "=", "=", (9, 4), (9, 5)),
            tok("number", "1", "1", (9, 6), (9, 7)),
            tok("structural", "<NEWLINE>", "", (9, 7), (9, 7)),
            tok("op", ",", ",", (9, 7), (9, 8)),
            tok("name", "B", "B", (10, 2), (10, 3)),
            tok("structural", "<NEWLINE>", "", (10, 3), (10, 3)),
            tok("op", "}", "}", (11, 0), (11, 1)),
            tok("structural", "<NEWLINE>", "", (11, 1), (11, 1)),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 7, "stmt", ""), (8, 11, "class", "E")]
    );
}

#[test]
fn tsx_and_jsx_parse() {
    let tsx = analyze(
        "a.tsx",
        "export const A = () => <div className=\"x\">{y}</div>;\n",
    );
    let jsx = analyze("a.jsx", "const A = () => <b>{`t ${z}`}</b>;\n");
    assert!(tsx.parsed && jsx.parsed);
    assert!(values(&tsx).contains(&"className"));
    assert!(values(&jsx).contains(&"z"));
    assert_eq!(
        tokens(&tsx),
        vec![
            tok("name", "export", "export", (1, 0), (1, 6)),
            tok("name", "const", "const", (1, 7), (1, 12)),
            tok("name", "A", "A", (1, 13), (1, 14)),
            tok("op", "=", "=", (1, 15), (1, 16)),
            tok("op", "(", "(", (1, 17), (1, 18)),
            tok("op", ")", ")", (1, 18), (1, 19)),
            tok("op", "=>", "=>", (1, 20), (1, 22)),
            tok("op", "<", "<", (1, 23), (1, 24)),
            tok("name", "div", "div", (1, 24), (1, 27)),
            tok("name", "className", "className", (1, 28), (1, 37)),
            tok("op", "=", "=", (1, 37), (1, 38)),
            tok("string", "'x'", "\"x\"", (1, 38), (1, 41)),
            tok("op", ">", ">", (1, 41), (1, 42)),
            tok("op", "{", "{", (1, 42), (1, 43)),
            tok("name", "y", "y", (1, 43), (1, 44)),
            tok("op", "}", "}", (1, 44), (1, 45)),
            tok("op", "</", "</", (1, 45), (1, 47)),
            tok("name", "div", "div", (1, 47), (1, 50)),
            tok("op", ">", ">", (1, 50), (1, 51)),
            tok("structural", "<NEWLINE>", "", (1, 52), (1, 52)),
        ]
    );
    assert_eq!(spans(&tsx.statements), vec![(1, 1, "def", "A")]);
    assert_eq!(spans(&jsx.statements), vec![(1, 1, "def", "A")]);
    // JSX in a plain .ts file does not parse; the tokens are still there.
    let ts = analyze("a.ts", "const A = () => <div className=\"x\">{y}</div>;\n");
    assert!(!ts.parsed);
    assert!(values(&ts).contains(&"className"));
}

#[test]
fn broken_file_still_tokenizes() {
    let an = analyze("a.ts", "function (( {\nconst x = getUser(1);\n");
    assert!(!an.parsed);
    assert!(an.syntax.is_none());
    assert!(values(&an).contains(&"getUser"));
    assert_eq!(
        tokens(&an),
        vec![
            tok("name", "function", "function", (1, 0), (1, 8)),
            tok("op", "(", "(", (1, 9), (1, 10)),
            tok("op", "(", "(", (1, 10), (1, 11)),
            tok("op", "{", "{", (1, 12), (1, 13)),
            tok("name", "const", "const", (2, 0), (2, 5)),
            tok("name", "x", "x", (2, 6), (2, 7)),
            tok("op", "=", "=", (2, 8), (2, 9)),
            tok("name", "getUser", "getUser", (2, 10), (2, 17)),
            tok("op", "(", "(", (2, 17), (2, 18)),
            tok("number", "1", "1", (2, 18), (2, 19)),
            tok("op", ")", ")", (2, 19), (2, 20)),
            tok("structural", "<NEWLINE>", "", (2, 20), (2, 20)),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 2, "stmt", ""), (2, 2, "stmt", "")]
    );
}

#[test]
fn analysis_has_no_calls_defs_or_docstrings() {
    let an = analyze("a.ts", "function f(a) { return g(a); }\n");
    assert!(an.parsed);
    assert!(an.calls.is_empty());
    assert!(an.defs.is_empty());
    assert!(an.docstrings.is_empty());
    assert!(an.syntax.is_none());
    assert_eq!(an.lines, vec!["function f(a) { return g(a); }"]);
}

// --- annotations -------------------------------------------------------------------------

#[test]
fn annotation_targets() {
    let an = analyze(
        "a.ts",
        "function f(x: number, y?: string): Promise<void> {}\n\
         const g = (a: number): number => a;\n\
         const h = function(): string {};\n\
         let z: Map<string, number>;\n\
         interface I { p: number; m(): void }\n\
         class K { f: number = 1; m(): void {} }\n\
         (a: number): number => a;\n",
    );
    assert_eq!(
        annotations(&an),
        vec![
            ((1, 14), (1, 20), "number", "param x"),
            ((1, 26), (1, 32), "string", "param y"),
            ((1, 35), (1, 48), "Promise<void>", "return of f"),
            ((2, 14), (2, 20), "number", "param a"),
            ((2, 23), (2, 29), "number", "return of g"),
            ((3, 22), (3, 28), "string", "return of ?"),
            ((4, 7), (4, 26), "Map<string, number>", "variable z"),
            ((5, 17), (5, 23), "number", "property p"),
            ((5, 30), (5, 34), "void", "return of m"),
            ((6, 13), (6, 19), "number", "property f"),
            ((6, 30), (6, 34), "void", "return of m"),
            ((7, 4), (7, 10), "number", "param a"),
            ((7, 13), (7, 19), "number", "return of arrow function"),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![
            (1, 1, "def", "f"),
            (2, 2, "def", "g"),
            (3, 3, "def", "h"),
            (4, 4, "stmt", ""),
            (5, 5, "class", "I"),
            (6, 6, "class", "K"),
            (7, 7, "stmt", ""),
        ]
    );
    assert_eq!(
        spans(&an.statements[5].children),
        vec![(6, 6, "stmt", ""), (6, 6, "def", "K.m")]
    );
}

#[test]
fn more_annotation_targets() {
    let an = analyze(
        "a.ts",
        "function f(x: number = 1, ...rest: string[]): void {}\n\
         const o = { m(): number { return 1 } };\n\
         class A { m = (): number => 1; }\n\
         abstract class B { abstract p: number; }\n\
         interface J { (x: number): string; new (y: number): J; q?: number }\n\
         let [p, q]: number[] = [];\n\
         for (const i: number = 0;;) {}\n",
    );
    assert_eq!(
        annotations(&an),
        vec![
            ((1, 14), (1, 20), "number", "param x"),
            ((1, 35), (1, 43), "string[]", "param ...rest"),
            ((1, 46), (1, 50), "void", "return of f"),
            ((2, 17), (2, 23), "number", "return of m"),
            ((3, 18), (3, 24), "number", "return of arrow function"),
            ((4, 31), (4, 37), "number", "property p"),
            ((5, 18), (5, 24), "number", "param x"),
            ((5, 43), (5, 49), "number", "param y"),
            ((5, 59), (5, 65), "number", "property q"),
            ((6, 12), (6, 20), "number[]", "variable [p, q]"),
            ((7, 14), (7, 20), "number", "variable i"),
        ]
    );
}

#[test]
fn annotations_skip_comments_and_untargeted_types() {
    let an = analyze(
        "a.ts",
        "function f(x: /* c */ number) {}\n\
         function g(): void {}\n\
         const [a, b]: [number, string] = f();\n\
         type T = { a: number };\n",
    );
    assert_eq!(
        annotations(&an),
        vec![
            ((1, 22), (1, 28), "number", "param x"),
            ((2, 14), (2, 18), "void", "return of g"),
            ((3, 14), (3, 30), "[number, string]", "variable [a, b]"),
            ((4, 14), (4, 20), "number", "property a"),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![
            (1, 1, "def", "f"),
            (2, 2, "def", "g"),
            (3, 3, "stmt", ""),
            (4, 4, "class", "T"),
        ]
    );
    // The annotation's text collapses whitespace; the comment is tokenized separately.
    assert_eq!(
        tokens(&an)[5],
        tok("comment", "/* c */", "/* c */", (1, 14), (1, 21))
    );
    let an = analyze("a.ts", "let m: Map<\n  string,\n  number\n>;\n");
    assert_eq!(
        annotations(&an),
        vec![((1, 7), (4, 1), "Map< string, number >", "variable m")]
    );
}

#[test]
fn declared_and_abstract_signatures() {
    let an = analyze(
        "a.ts",
        "export abstract class Ab { abstract m(): void; }\ndeclare function df(): void;\n",
    );
    assert_eq!(
        annotations(&an),
        vec![
            ((1, 41), (1, 45), "void", "return of m"),
            ((2, 23), (2, 27), "void", "return of df"),
        ]
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 1, "class", "Ab"), (2, 2, "stmt", "")]
    );
    assert_eq!(spans(&an.statements[0].children), vec![(1, 1, "stmt", "")]);
}

// --- statements --------------------------------------------------------------------------

#[test]
fn statement_spans() {
    let an = analyze(
        "a.ts",
        "import { a } from './m';\n\
         @dec\n\
         export class C {\n\
         \x20 @x\n\
         \x20 m(a: number) { return 1; }\n\
         \x20 f = 2;\n\
         }\n\
         export const g = (x) => x;\n\
         function h() {}\n\
         interface I { a: number }\n\
         let z = 1;\n",
    );
    assert_eq!(
        spans(&an.statements),
        vec![
            (1, 1, "stmt", ""),
            (2, 7, "class", "C"),
            (8, 8, "def", "g"),
            (9, 9, "def", "h"),
            (10, 10, "class", "I"),
            (11, 11, "stmt", ""),
        ]
    );
    assert_eq!(
        spans(&an.statements[1].children),
        vec![(4, 5, "def", "C.m"), (6, 6, "stmt", "")]
    );
    assert_eq!(an.statements[1].kind, StmtKind::Class);
    assert!(an.statements.iter().all(|s| s.node.is_some()));
    assert!(an.statements[1].children.iter().all(|s| s.node.is_some()));
    assert!(an.statements[1].children[0].children.is_empty());
}

#[test]
fn statement_kinds_by_declaration_shape() {
    let an = analyze(
        "a.ts",
        "export { a } from './m';\n\
         export * from './n';\n\
         export default function () {}\n\
         export default class {}\n\
         export { a, b };\n\
         export enum E { A, B }\n\
         namespace N { }\n\
         var v = function* () {};\n\
         function* gen() {}\n\
         let a = () => 1, b = 2;\n\
         const { d } = function () {};\n\
         const k = function named() {};\n\
         export type Al = string;\n\
         if (a) {\n\
         \x20 b();\n\
         }\n",
    );
    assert_eq!(
        spans(&an.statements),
        vec![
            (1, 1, "stmt", ""),
            (2, 2, "stmt", ""),
            (3, 3, "stmt", ""),
            (4, 4, "stmt", ""),
            (5, 5, "stmt", ""),
            (6, 6, "class", "E"),
            (7, 7, "stmt", ""),
            (8, 8, "def", "v"),
            (9, 9, "def", "gen"),
            (10, 10, "stmt", ""),
            (11, 11, "stmt", ""),
            (12, 12, "def", "k"),
            (13, 13, "class", "Al"),
            (14, 16, "stmt", ""),
        ]
    );
    assert!(an.imports.is_empty());
}

#[test]
fn class_member_spans_include_decorators() {
    let an = analyze(
        "a.ts",
        "class K {\n\
         \x20 // leading\n\
         \x20 @a @b\n\
         \x20 static async *m() {}\n\
         \x20 @c\n\
         \x20 prop: number = 1;\n\
         \x20 get x(): number { return 1 }\n\
         \x20 constructor() {}\n\
         \x20 [key]: string;\n\
         }\n",
    );
    assert_eq!(spans(&an.statements), vec![(1, 10, "class", "K")]);
    assert_eq!(
        spans(&an.statements[0].children),
        vec![
            (3, 4, "def", "K.m"),
            (5, 6, "stmt", ""),
            (7, 7, "def", "K.x"),
            (8, 8, "def", "K.constructor"),
            (9, 9, "stmt", ""),
        ]
    );
    assert_eq!(
        annotations(&an),
        vec![
            ((6, 8), (6, 14), "number", "property prop"),
            ((7, 11), (7, 17), "number", "return of x"),
            ((9, 9), (9, 15), "string", "property [key]"),
        ]
    );
}

// --- imports -----------------------------------------------------------------------------

#[test]
fn import_bindings() {
    let an = analyze(
        "a.ts",
        "import d, { a as b, c } from './m';\nimport * as ns from \"../n\";\nimport 'side';\n",
    );
    let found: Vec<(&str, Option<&str>, &str)> = an
        .imports
        .iter()
        .flat_map(|s| s.bindings.iter())
        .map(|b| (b.module.as_str(), b.name.as_deref(), b.alias.as_str()))
        .collect();
    assert_eq!(
        found,
        vec![
            ("./m", Some("default"), "d"),
            ("./m", Some("a"), "b"),
            ("./m", Some("c"), "c"),
            ("../n", None, "ns"),
            ("side", None, ""),
        ]
    );
    let lines: Vec<(u32, u32)> = an.imports.iter().map(|s| (s.start, s.end)).collect();
    assert_eq!(lines, vec![(1, 1), (2, 2), (3, 3)]);
    let texts: Vec<&str> = an
        .imports
        .iter()
        .flat_map(|s| s.bindings.iter())
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(
        texts,
        vec![
            "import d from \"./m\"",
            "import { a as b } from \"./m\"",
            "import { c } from \"./m\"",
            "import * as ns from \"../n\"",
            "import \"side\"",
        ]
    );
    assert!(
        an.imports
            .iter()
            .flat_map(|s| s.bindings.iter())
            .all(|b| b.level == 0)
    );
    assert_eq!(
        spans(&an.statements),
        vec![(1, 1, "stmt", ""), (2, 2, "stmt", ""), (3, 3, "stmt", "")]
    );
}

/// `(start, end, [(module, name, alias, text)])` of an import site.
type Site<'a> = (u32, u32, Vec<(&'a str, Option<&'a str>, &'a str, &'a str)>);

#[test]
fn more_import_shapes() {
    let an = analyze(
        "a.ts",
        "import type { T } from './t';\n\
         import d2, * as all from 'x';\n\
         import {} from 'e';\n\
         export { a } from './m';\n\
         import {\n\
         \x20 p,\n\
         \x20 q as r,\n\
         } from \"./pq\";\n",
    );
    let found: Vec<Site<'_>> = an
        .imports
        .iter()
        .map(|s| {
            (
                s.start,
                s.end,
                s.bindings
                    .iter()
                    .map(|b| {
                        (
                            b.module.as_str(),
                            b.name.as_deref(),
                            b.alias.as_str(),
                            b.text.as_str(),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            (
                1,
                1,
                vec![("./t", Some("T"), "T", "import { T } from \"./t\"")]
            ),
            (
                2,
                2,
                vec![
                    ("x", Some("default"), "d2", "import d2 from \"x\""),
                    ("x", None, "all", "import * as all from \"x\""),
                ]
            ),
            (3, 3, vec![("e", None, "", "import \"e\"")]),
            (
                5,
                8,
                vec![
                    ("./pq", Some("p"), "p", "import { p } from \"./pq\""),
                    ("./pq", Some("q"), "r", "import { q as r } from \"./pq\""),
                ]
            ),
        ]
    );
}
