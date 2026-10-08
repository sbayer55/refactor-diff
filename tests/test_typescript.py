from refactor_diff.languages import analyzer_for
from refactor_diff.languages.base import STRUCTURAL
from refactor_diff.model import DOCS, FORMATTING, RENAME, RETYPE
from refactor_diff.patterns import classify

TS = analyzer_for("a.ts")


def sigs(old: str, new: str, analyzer=TS):
    a, b = analyzer.analyze(old), analyzer.analyze(new)
    result = classify(analyzer, a, b, (1, len(a.lines)), (1, len(b.lines)))
    return [(s.kind, s.old, s.new, s.detail) for s in result.signatures]


def test_dialects_by_extension():
    assert [analyzer_for(p).name for p in ("a.ts", "b.mts", "c.tsx", "d.js", "e.jsx", "f.cjs")] == [
        "typescript",
        "typescript",
        "tsx",
        "javascript",
        "javascript",
        "javascript",
    ]
    assert analyzer_for("a.d.ts").name == "typescript"
    assert "*.js" in TS.globs and "*.tsx" in analyzer_for("a.js").globs


def test_call_rename():
    assert sigs("const x = getUser(1);\n", "const x = fetchUser(1);\n") == [
        (RENAME, "getUser", "fetchUser", "call")
    ]


def test_definition_rename():
    assert sigs("export function getUser() {}\n", "export function fetchUser() {}\n") == [
        (RENAME, "getUser", "fetchUser", "definition")
    ]


def test_const_definition_rename():
    assert sigs("const limit = 1;\n", "const maxItems = 1;\n") == [
        (RENAME, "limit", "maxItems", "definition")
    ]


def test_import_rename():
    assert sigs('import { getUser } from "./u";\n', 'import { fetchUser } from "./u";\n') == [
        (RENAME, "getUser", "fetchUser", "import")
    ]


def test_import_after_other_statements():
    old = 'const a = 1;\nimport { getUser } from "./u";\n'
    new = 'const a = 1;\nimport { fetchUser } from "./u";\n'
    assert sigs(old, new) == [(RENAME, "getUser", "fetchUser", "import")]


def test_attribute_rename():
    assert sigs("y = obj.oldName\n", "y = obj.newName\n") == [
        (RENAME, "oldName", "newName", "attribute")
    ]


def test_param_retype():
    assert sigs("function f(x: number) {}\n", "function f(x: string) {}\n") == [
        (RETYPE, "number", "string", "param x")
    ]


def test_optional_param_retype():
    assert sigs("function f(x?: number) {}\n", "function f(x?: string) {}\n") == [
        (RETYPE, "number", "string", "param x")
    ]


def test_return_retype():
    old = "function f(): Promise<void> {}\n"
    new = "function f(): Promise<string> {}\n"
    assert sigs(old, new) == [(RETYPE, "Promise<void>", "Promise<string>", "return of f")]


def test_arrow_return_retype():
    old = "const f = (a: number): number => a;\n"
    new = "const f = (a: number): string => a;\n"
    assert sigs(old, new) == [(RETYPE, "number", "string", "return of f")]


def test_variable_and_property_retype():
    assert sigs("let z: Map<string, number>;\n", "let z: Map<string, string>;\n") == [
        (RETYPE, "Map<string, number>", "Map<string, string>", "variable z")
    ]
    assert sigs("interface I { p: number }\n", "interface I { p: string }\n") == [
        (RETYPE, "number", "string", "property p")
    ]


def test_quotes_and_semicolons_are_formatting():
    old = "const a = 'hi'\nf(`x`)\n"
    new = 'const a = "hi";\nf("x");\n'
    assert sigs(old, new) == [(FORMATTING, "", "", "")]


def test_escapes_compare_by_value():
    assert sigs("const a = 'it\\'s'\n", 'const a = "it\'s"\n') == [(FORMATTING, "", "", "")]


def test_reindent_is_formatting():
    assert sigs("if (a) {\n  b();\n}\n", "if (a) {\n    b();\n}\n") == [(FORMATTING, "", "", "")]


def test_comment_only_change_is_docs():
    old = "/** Old docs. */\nconst a = 1;\n"
    new = "/** New docs. */\nconst a = 1;\n"
    assert sigs(old, new) == [(DOCS, "", "", "")]


def test_renames_inside_template_substitutions():
    assert sigs("const s = `id ${getUser()}`;\n", "const s = `id ${fetchUser()}`;\n") == [
        (RENAME, "getUser", "fetchUser", "call")
    ]


def test_tsx_and_jsx_parse():
    tsx = analyzer_for("a.tsx").analyze('export const A = () => <div className="x">{y}</div>;\n')
    jsx = analyzer_for("a.jsx").analyze("const A = () => <b>{`t ${z}`}</b>;\n")
    assert tsx.parsed and jsx.parsed
    assert "className" in [t.value for t in tsx.tokens]
    assert "z" in [t.value for t in jsx.tokens]


def test_broken_file_still_tokenizes():
    analysis = TS.analyze("function (( {\nconst x = getUser(1);\n")
    assert not analysis.parsed
    assert "getUser" in [t.value for t in analysis.tokens]


def test_statements_end_with_structural_tokens():
    analysis = TS.analyze("a();\nb()\n")
    assert [t.kind == STRUCTURAL for t in analysis.tokens] == [
        False,
        False,
        False,
        True,
        False,
        False,
        False,
        True,
    ]


def test_columns_count_characters():
    [_, name, *_] = TS.analyze('const é = "ü"; const name = 1;\n').tokens[5:]
    assert name.value == "name" and name.start == (1, 21)


def test_builtins_and_keywords():
    assert TS.is_builtin("Promise") and TS.is_builtin("console") and not TS.is_builtin("getUser")
    assert TS.is_keyword("interface") and not TS.is_keyword("getUser")


def test_statement_spans():
    an = TS.analyze(
        "import { a } from './m';\n"
        "@dec\n"
        "export class C {\n"
        "  @x\n"
        "  m(a: number) { return 1; }\n"
        "  f = 2;\n"
        "}\n"
        "export const g = (x) => x;\n"
        "function h() {}\n"
        "interface I { a: number }\n"
        "let z = 1;\n"
    )
    spans = [(s.start, s.end, s.kind, s.qualname) for s in an.statements]
    assert spans == [
        (1, 1, "stmt", ""),
        (2, 7, "class", "C"),
        (8, 8, "def", "g"),
        (9, 9, "def", "h"),
        (10, 10, "class", "I"),
        (11, 11, "stmt", ""),
    ]
    members = [(s.start, s.end, s.kind, s.qualname) for s in an.statements[1].children]
    assert members == [(4, 5, "def", "C.m"), (6, 6, "stmt", "")]


def test_import_bindings():
    an = TS.analyze(
        "import d, { a as b, c } from './m';\nimport * as ns from \"../n\";\nimport 'side';\n"
    )
    found = [(b.module, b.name, b.alias) for s in an.imports for b in s.bindings]
    assert found == [
        ("./m", "default", "d"),
        ("./m", "a", "b"),
        ("./m", "c", "c"),
        ("../n", None, "ns"),
        ("side", None, ""),
    ]
    assert [(s.start, s.end) for s in an.imports] == [(1, 1), (2, 2), (3, 3)]


def test_import_path_change():
    assert sigs('import { a } from "./old";\n', 'import { a } from "./new";\n') == [
        ("import", "./old", "./new", "a")
    ]
