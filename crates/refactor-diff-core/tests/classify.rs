//! Ports of `tests/test_patterns.py` and the classification tests of `tests/test_typescript.py`:
//! the classifier on the real Python and TypeScript analyzers.

use refactor_diff_core::classify::{Classification, classify};
use refactor_diff_core::{LanguageAnalyzer, SignatureKind, analyzer_for};

fn py() -> &'static dyn LanguageAnalyzer {
    analyzer_for("x.py").expect("python analyzer")
}

fn ts() -> &'static dyn LanguageAnalyzer {
    analyzer_for("a.ts").expect("typescript analyzer")
}

fn classify_one(lang: &dyn LanguageAnalyzer, old: &str, new: &str) -> Classification {
    let (a, b) = (lang.analyze(old), lang.analyze(new));
    let (old_n, new_n) = (a.lines.len() as u32, b.lines.len() as u32);
    classify(
        lang,
        &a,
        &b,
        (old_n > 0).then_some((1, old_n)),
        (new_n > 0).then_some((1, new_n)),
        0,
        0,
    )
}

type Sig = (SignatureKind, String, String, String);

fn tuple(kind: SignatureKind, old: &str, new: &str, detail: &str) -> Sig {
    (kind, old.into(), new.into(), detail.into())
}

fn sigs_with(lang: &dyn LanguageAnalyzer, old: &str, new: &str) -> Vec<Sig> {
    classify_one(lang, old, new)
        .signatures
        .iter()
        .map(|s| (s.kind, s.old.clone(), s.new.clone(), s.detail.clone()))
        .collect()
}

fn sigs(old: &str, new: &str) -> Vec<Sig> {
    sigs_with(py(), old, new)
}

fn keys(old: &str, new: &str) -> Vec<String> {
    classify_one(py(), old, new)
        .signatures
        .iter()
        .map(|s| s.key.clone())
        .collect()
}

use SignatureKind::{Args, Docs, Formatting, Import, Rename, Replace, Retype};

// --- test_patterns.py ---

#[test]
fn function_rename_at_call_site() {
    assert_eq!(
        sigs("x = get_user(1)\n", "x = fetch_user(1)\n"),
        vec![tuple(Rename, "get_user", "fetch_user", "call")]
    );
}

#[test]
fn definition_rename() {
    assert_eq!(
        sigs(
            "def get_user():\n    pass\n",
            "def fetch_user():\n    pass\n"
        )[0],
        tuple(Rename, "get_user", "fetch_user", "definition")
    );
}

#[test]
fn attribute_rename() {
    assert_eq!(
        sigs("y = obj.old_name\n", "y = obj.new_name\n"),
        vec![tuple(Rename, "old_name", "new_name", "attribute")]
    );
}

#[test]
fn import_rename() {
    assert_eq!(
        sigs("from m import a\n", "from m import b\n"),
        vec![tuple(Rename, "a", "b", "import")]
    );
}

#[test]
fn repeated_rename_on_one_line_is_one_signature() {
    assert_eq!(
        sigs("f(a) + f(b)\n", "g(a) + g(b)\n"),
        vec![tuple(Rename, "f", "g", "call")]
    );
}

#[test]
fn param_type_change() {
    assert_eq!(
        sigs("def f(x: int):\n    pass\n", "def f(x: str):\n    pass\n"),
        vec![tuple(Retype, "int", "str", "param x")]
    );
}

#[test]
fn return_type_change() {
    assert_eq!(
        sigs(
            "def f() -> List[int]:\n    pass\n",
            "def f() -> list[int]:\n    pass\n"
        ),
        vec![tuple(Retype, "List[int]", "list[int]", "return of f")]
    );
}

#[test]
fn annotation_added() {
    assert_eq!(
        sigs("def f(x):\n    pass\n", "def f(x: int):\n    pass\n"),
        vec![tuple(Retype, "(untyped)", "int", "param x")]
    );
}

#[test]
fn variable_annotation_change() {
    assert_eq!(
        sigs("count: int = 0\n", "count: float = 0\n"),
        vec![tuple(Retype, "int", "float", "variable count")]
    );
}

#[test]
fn formatting_only() {
    assert_eq!(
        sigs("x = {'a':1}\n", "x = {\"a\": 1}\n"),
        vec![tuple(Formatting, "", "", "")]
    );
}

#[test]
fn rewrapped_call_is_formatting() {
    assert_eq!(
        sigs("f(a,\n  b)\n", "f(a, b)\n"),
        vec![tuple(Formatting, "", "", "")]
    );
}

#[test]
fn dotted_replacement_is_one_signature() {
    assert_eq!(
        sigs("t = cfg.get(\"timeout\")\n", "t = settings.timeout\n"),
        vec![tuple(
            Replace,
            "cfg.get(\"timeout\")",
            "settings.timeout",
            ""
        )]
    );
}

#[test]
fn keyword_argument_rename() {
    assert_eq!(
        sigs("f(a, role=1)\n", "f(a, roles=1)\n"),
        vec![tuple(Rename, "role", "roles", "keyword")]
    );
}

#[test]
fn wrap_in_tuple_is_one_template() {
    assert_eq!(
        sigs("f(role=\"admin\")\n", "f(roles=(\"admin\",))\n"),
        vec![tuple(Replace, "role=…", "roles=(…,)", "")]
    );
}

#[test]
fn wrap_templates_group_across_different_values() {
    let a = classify_one(py(), "f(role=\"admin\")\n", "f(roles=(\"admin\",))\n");
    let b = classify_one(
        py(),
        "g(x, role=\"faculty\")\n",
        "g(x, roles=(\"faculty\",))\n",
    );
    assert_eq!(a.signatures[0].key, b.signatures[0].key);
}

#[test]
fn wrap_in_call_is_one_template() {
    assert_eq!(
        sigs(
            "save(created_by=actor)\n",
            "save(created_by=str(actor.user_id))\n"
        ),
        vec![tuple(Replace, "…", "str(….user_id)", "")]
    );
}

#[test]
fn rename_plus_logic_change_has_both_signatures() {
    let kinds: Vec<SignatureKind> = sigs(
        "if get_user(u):\n    pass\n",
        "if fetch_user(u) and ok:\n    pass\n",
    )
    .into_iter()
    .map(|s| s.0)
    .collect();
    assert_eq!(kinds, vec![Rename, Replace]);
}

#[test]
fn keyword_swap_is_not_a_rename() {
    assert_eq!(sigs("x = a and b\n", "x = a or b\n")[0].0, Replace);
}

#[test]
fn unparseable_file_still_classifies() {
    assert_eq!(
        sigs("x = get_user(\n", "x = fetch_user(\n"),
        vec![tuple(Rename, "get_user", "fetch_user", "call")]
    );
}

#[test]
fn comment_change_is_docs() {
    assert_eq!(
        sigs("x = 1  # old note\n", "x = 1  # new note\n"),
        vec![tuple(Docs, "", "", "")]
    );
}

#[test]
fn added_comment_line_is_docs() {
    assert_eq!(
        sigs("", "# explain the next line\n"),
        vec![tuple(Docs, "", "", "")]
    );
}

#[test]
fn docstring_change_is_docs() {
    let old = "def f():\n    \"\"\"Old summary.\"\"\"\n    return 1\n";
    let new = "def f():\n    \"\"\"New summary.\"\"\"\n    return 1\n";
    assert_eq!(sigs(old, new), vec![tuple(Docs, "", "", "")]);
}

#[test]
fn regular_string_change_is_not_docs() {
    assert_eq!(sigs("x = \"old\"\n", "x = \"new\"\n")[0].0, Replace);
}

#[test]
fn comment_and_code_change_keeps_code_signature() {
    let mut kinds: Vec<SignatureKind> = sigs("f(a)  # old\n", "g(a)  # new\n")
        .into_iter()
        .map(|s| s.0)
        .collect();
    kinds.sort_by_key(|k| k.as_str());
    assert_eq!(kinds, vec![Docs, Rename]);
}

// --- imports ---

#[test]
fn import_module_change() {
    assert_eq!(
        sigs("from a import x\n", "from b import x\n"),
        vec![tuple(Import, "a", "b", "x")]
    );
    assert_eq!(
        keys("from a import x\n", "from b import x\n"),
        vec!["import\0a\0b".to_string()]
    );
    assert_eq!(
        keys("from a import x\n", "from b import x\n"),
        keys("from a import x as y\n", "from b import x as y\n")
    );
}

#[test]
fn import_renamed_name_stays_a_rename() {
    assert_eq!(
        sigs("from a import x\n", "from a import y\n"),
        vec![tuple(Rename, "x", "y", "import")]
    );
}

#[test]
fn import_module_and_name_change_falls_through() {
    assert!(
        sigs("from a import x\n", "from b import y\n")
            .iter()
            .all(|s| s.0 != Import)
    );
}

#[test]
fn import_block_with_rename_module_change_and_addition() {
    let old = "from a import x\nfrom p import h\n";
    let new = "import logging\n\nfrom a import y\nfrom q import h\n";
    let mut found = sigs(old, new);
    found.sort_by(|a, b| (a.0.as_str(), &a.1, &a.2).cmp(&(b.0.as_str(), &b.1, &b.2)));
    let mut expected = vec![
        tuple(Rename, "x", "y", "import"),
        tuple(Import, "p", "q", "h"),
        tuple(Import, "", "import logging", "logging"),
    ];
    expected.sort_by(|a, b| (a.0.as_str(), &a.1, &a.2).cmp(&(b.0.as_str(), &b.1, &b.2)));
    assert_eq!(found, expected);
}

#[test]
fn import_added_and_removed_names() {
    assert_eq!(
        sigs("import os\n", "import os\nimport sys\n"),
        vec![tuple(Import, "", "import sys", "sys")]
    );
    assert_eq!(
        sigs("from a import x, y\n", "from a import x\n"),
        vec![tuple(Import, "from a import y", "", "y")]
    );
    assert_eq!(
        keys("", "from __future__ import annotations\n"),
        vec!["import\0\0__future__.annotations".to_string()]
    );
}

#[test]
fn import_multiline_only_module_line_changed() {
    let old = "from a import (\n    x,\n    y,\n)\n";
    let new = old.replace("from a", "from b");
    let (a, b) = (py().analyze(old), py().analyze(&new));
    // the unit is the first line only
    let result = classify(py(), &a, &b, Some((1, 1)), Some((1, 1)), 0, 0);
    let found: Vec<(SignatureKind, &str, &str)> = result
        .signatures
        .iter()
        .map(|s| (s.kind, s.old.as_str(), s.new.as_str()))
        .collect();
    assert_eq!(found, vec![(Import, "a", "b")]);
}

#[test]
fn import_mixed_with_code_is_not_an_import_change() {
    assert!(
        sigs("from a import x\nz = 1\n", "from b import x\nz = 2\n")
            .iter()
            .all(|s| s.0 != Import)
    );
}

// --- argument lists ---

#[test]
fn added_keyword_argument_groups_across_values_and_with_the_def() {
    let call1 = keys("fetch(1, 2)\n", "fetch(1, 2, timeout=5)\n");
    let call2 = keys("fetch(x, y)\n", "fetch(x, y, timeout=cfg.t)\n");
    let definition = keys(
        "def fetch(a, b):\n    pass\n",
        "def fetch(a, b, timeout=None):\n    pass\n",
    );
    assert_eq!(call1, vec!["args\0fetch\0+kw:timeout".to_string()]);
    assert_eq!(call1, call2);
    assert_eq!(call1, definition);
    assert_eq!(
        sigs("fetch(1, 2)\n", "fetch(1, 2, timeout=5)\n"),
        vec![tuple(Args, "fetch(…)", "fetch(…, timeout=…)", "call")]
    );
    assert_eq!(
        sigs(
            "def fetch(a, b):\n    pass\n",
            "def fetch(a, b, timeout=None):\n    pass\n"
        )[0]
        .3,
        "definition"
    );
}

#[test]
fn removed_keyword_argument() {
    assert_eq!(
        keys("f(x, verbose=True)\n", "f(x)\n"),
        vec!["args\0f\0-kw:verbose".to_string()]
    );
}

#[test]
fn positional_to_keyword() {
    assert_eq!(
        keys("f(x, 5)\n", "f(x, timeout=5)\n"),
        vec!["args\0f\0pos>kw:timeout".to_string()]
    );
}

#[test]
fn edit_inside_an_argument_is_not_an_args_change() {
    assert!(
        sigs("fetch(x, y)\n", "fetch(x, y + 1, timeout=5)\n")
            .iter()
            .all(|s| s.0 != Args)
    );
}

#[test]
fn method_and_function_calls_share_the_key() {
    assert_eq!(
        keys("client.fetch(x)\n", "client.fetch(x, timeout=1)\n"),
        keys("fetch(x)\n", "fetch(x, timeout=2)\n")
    );
}

#[test]
fn nested_call_attributes_to_the_inner_call() {
    assert_eq!(
        keys("log(fetch(x))\n", "log(fetch(x, timeout=1))\n"),
        vec!["args\0fetch\0+kw:timeout".to_string()]
    );
}

#[test]
fn replaced_positional_plus_added_kwarg_is_not_an_args_change() {
    assert!(
        sigs("fetch(x, y)\n", "fetch(x, z, timeout=5)\n")
            .iter()
            .all(|s| s.0 != Args)
    );
}

#[test]
fn added_argument_line_in_multiline_call() {
    let old = "r = fetch(\n    x,\n)\n";
    let new = "r = fetch(\n    x,\n    timeout=5,\n)\n";
    let (a, b) = (py().analyze(old), py().analyze(new));
    let result = classify(py(), &a, &b, None, Some((3, 3)), 2, 2);
    let found: Vec<&str> = result.signatures.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(found, vec!["args\0fetch\0+kw:timeout"]);
}

#[test]
fn added_positional_parameter() {
    assert_eq!(
        keys("def f(a):\n    pass\n", "def f(a, b):\n    pass\n"),
        vec!["args\0f\0+pos".to_string()]
    );
    assert_eq!(
        keys("f(1)\n", "f(1, 2)\n"),
        vec!["args\0f\0+pos".to_string()]
    );
}

// --- test_typescript.py (classification) ---

fn ts_sigs(old: &str, new: &str) -> Vec<Sig> {
    sigs_with(ts(), old, new)
}

#[test]
fn ts_call_rename() {
    assert_eq!(
        ts_sigs("const x = getUser(1);\n", "const x = fetchUser(1);\n"),
        vec![tuple(Rename, "getUser", "fetchUser", "call")]
    );
}

#[test]
fn ts_definition_rename() {
    assert_eq!(
        ts_sigs(
            "export function getUser() {}\n",
            "export function fetchUser() {}\n"
        ),
        vec![tuple(Rename, "getUser", "fetchUser", "definition")]
    );
}

#[test]
fn ts_const_definition_rename() {
    assert_eq!(
        ts_sigs("const limit = 1;\n", "const maxItems = 1;\n"),
        vec![tuple(Rename, "limit", "maxItems", "definition")]
    );
}

#[test]
fn ts_import_rename() {
    assert_eq!(
        ts_sigs(
            "import { getUser } from \"./u\";\n",
            "import { fetchUser } from \"./u\";\n"
        ),
        vec![tuple(Rename, "getUser", "fetchUser", "import")]
    );
}

#[test]
fn ts_import_after_other_statements() {
    let old = "const a = 1;\nimport { getUser } from \"./u\";\n";
    let new = "const a = 1;\nimport { fetchUser } from \"./u\";\n";
    assert_eq!(
        ts_sigs(old, new),
        vec![tuple(Rename, "getUser", "fetchUser", "import")]
    );
}

#[test]
fn ts_attribute_rename() {
    assert_eq!(
        ts_sigs("y = obj.oldName\n", "y = obj.newName\n"),
        vec![tuple(Rename, "oldName", "newName", "attribute")]
    );
}

#[test]
fn ts_param_retype() {
    assert_eq!(
        ts_sigs("function f(x: number) {}\n", "function f(x: string) {}\n"),
        vec![tuple(Retype, "number", "string", "param x")]
    );
}

#[test]
fn ts_optional_param_retype() {
    assert_eq!(
        ts_sigs("function f(x?: number) {}\n", "function f(x?: string) {}\n"),
        vec![tuple(Retype, "number", "string", "param x")]
    );
}

#[test]
fn ts_return_retype() {
    assert_eq!(
        ts_sigs(
            "function f(): Promise<void> {}\n",
            "function f(): Promise<string> {}\n"
        ),
        vec![tuple(
            Retype,
            "Promise<void>",
            "Promise<string>",
            "return of f"
        )]
    );
}

#[test]
fn ts_arrow_return_retype() {
    assert_eq!(
        ts_sigs(
            "const f = (a: number): number => a;\n",
            "const f = (a: number): string => a;\n"
        ),
        vec![tuple(Retype, "number", "string", "return of f")]
    );
}

#[test]
fn ts_variable_and_property_retype() {
    assert_eq!(
        ts_sigs(
            "let z: Map<string, number>;\n",
            "let z: Map<string, string>;\n"
        ),
        vec![tuple(
            Retype,
            "Map<string, number>",
            "Map<string, string>",
            "variable z"
        )]
    );
    assert_eq!(
        ts_sigs("interface I { p: number }\n", "interface I { p: string }\n"),
        vec![tuple(Retype, "number", "string", "property p")]
    );
}

#[test]
fn ts_quotes_and_semicolons_are_formatting() {
    let old = "const a = 'hi'\nf(`x`)\n";
    let new = "const a = \"hi\";\nf(\"x\");\n";
    assert_eq!(ts_sigs(old, new), vec![tuple(Formatting, "", "", "")]);
}

#[test]
fn ts_escapes_compare_by_value() {
    assert_eq!(
        ts_sigs("const a = 'it\\'s'\n", "const a = \"it's\"\n"),
        vec![tuple(Formatting, "", "", "")]
    );
}

#[test]
fn ts_reindent_is_formatting() {
    assert_eq!(
        ts_sigs("if (a) {\n  b();\n}\n", "if (a) {\n    b();\n}\n"),
        vec![tuple(Formatting, "", "", "")]
    );
}

#[test]
fn ts_comment_only_change_is_docs() {
    assert_eq!(
        ts_sigs(
            "/** Old docs. */\nconst a = 1;\n",
            "/** New docs. */\nconst a = 1;\n"
        ),
        vec![tuple(Docs, "", "", "")]
    );
}

#[test]
fn ts_renames_inside_template_substitutions() {
    assert_eq!(
        ts_sigs(
            "const s = `id ${getUser()}`;\n",
            "const s = `id ${fetchUser()}`;\n"
        ),
        vec![tuple(Rename, "getUser", "fetchUser", "call")]
    );
}

#[test]
fn ts_import_path_change() {
    assert_eq!(
        ts_sigs(
            "import { a } from \"./old\";\n",
            "import { a } from \"./new\";\n"
        ),
        vec![tuple(Import, "./old", "./new", "a")]
    );
}
