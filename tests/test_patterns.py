from refactor_diff.languages.python import PythonAnalyzer
from refactor_diff.model import DOCS, FORMATTING, RENAME, REPLACE, RETYPE
from refactor_diff.patterns import classify

PY = PythonAnalyzer()


def classify_one(old: str, new: str):
    a, b = PY.analyze(old), PY.analyze(new)
    old_n, new_n = len(a.lines), len(b.lines)
    return classify(PY, a, b, (1, old_n) if old_n else None, (1, new_n) if new_n else None)


def sigs(old: str, new: str):
    result = classify_one(old, new)
    return [(s.kind, s.old, s.new, s.detail) for s in result.signatures]


def test_function_rename_at_call_site():
    assert sigs("x = get_user(1)\n", "x = fetch_user(1)\n") == [
        (RENAME, "get_user", "fetch_user", "call")
    ]


def test_definition_rename():
    assert sigs("def get_user():\n    pass\n", "def fetch_user():\n    pass\n")[0] == (
        RENAME,
        "get_user",
        "fetch_user",
        "definition",
    )


def test_attribute_rename():
    assert sigs("y = obj.old_name\n", "y = obj.new_name\n") == [
        (RENAME, "old_name", "new_name", "attribute")
    ]


def test_import_rename():
    assert sigs("from m import a\n", "from m import b\n") == [(RENAME, "a", "b", "import")]


def test_repeated_rename_on_one_line_is_one_signature():
    assert sigs("f(a) + f(b)\n", "g(a) + g(b)\n") == [(RENAME, "f", "g", "call")]


def test_param_type_change():
    assert sigs("def f(x: int):\n    pass\n", "def f(x: str):\n    pass\n") == [
        (RETYPE, "int", "str", "param x")
    ]


def test_return_type_change():
    assert sigs("def f() -> List[int]:\n    pass\n", "def f() -> list[int]:\n    pass\n") == [
        (RETYPE, "List[int]", "list[int]", "return of f")
    ]


def test_annotation_added():
    assert sigs("def f(x):\n    pass\n", "def f(x: int):\n    pass\n") == [
        (RETYPE, "(untyped)", "int", "param x")
    ]


def test_variable_annotation_change():
    assert sigs("count: int = 0\n", "count: float = 0\n") == [
        (RETYPE, "int", "float", "variable count")
    ]


def test_formatting_only():
    assert sigs("x = {'a':1}\n", 'x = {"a": 1}\n') == [(FORMATTING, "", "", "")]


def test_rewrapped_call_is_formatting():
    assert sigs("f(a,\n  b)\n", "f(a, b)\n") == [(FORMATTING, "", "", "")]


def test_dotted_replacement_is_one_signature():
    assert sigs('t = cfg.get("timeout")\n', "t = settings.timeout\n") == [
        (REPLACE, 'cfg.get("timeout")', "settings.timeout", "")
    ]


def test_keyword_argument_rename():
    assert sigs("f(a, role=1)\n", "f(a, roles=1)\n") == [(RENAME, "role", "roles", "keyword")]


def test_wrap_in_tuple_is_one_template():
    assert sigs('f(role="admin")\n', 'f(roles=("admin",))\n') == [
        (REPLACE, "role=…", "roles=(…,)", "")
    ]


def test_wrap_templates_group_across_different_values():
    a = classify_one('f(role="admin")\n', 'f(roles=("admin",))\n')
    b = classify_one('g(x, role="faculty")\n', 'g(x, roles=("faculty",))\n')
    assert a.signatures[0].key == b.signatures[0].key


def test_wrap_in_call_is_one_template():
    assert sigs("save(created_by=actor)\n", "save(created_by=str(actor.user_id))\n") == [
        (REPLACE, "…", "str(….user_id)", "")
    ]


def test_rename_plus_logic_change_has_both_signatures():
    kinds = [
        s[0] for s in sigs("if get_user(u):\n    pass\n", "if fetch_user(u) and ok:\n    pass\n")
    ]
    assert kinds == [RENAME, REPLACE]


def test_keyword_swap_is_not_a_rename():
    assert sigs("x = a and b\n", "x = a or b\n")[0][0] == REPLACE


def test_unparseable_file_still_classifies():
    assert sigs("x = get_user(\n", "x = fetch_user(\n") == [
        (RENAME, "get_user", "fetch_user", "call")
    ]


def test_comment_change_is_docs():
    assert sigs("x = 1  # old note\n", "x = 1  # new note\n") == [(DOCS, "", "", "")]


def test_added_comment_line_is_docs():
    assert sigs("", "# explain the next line\n") == [(DOCS, "", "", "")]


def test_docstring_change_is_docs():
    old = 'def f():\n    """Old summary."""\n    return 1\n'
    new = 'def f():\n    """New summary."""\n    return 1\n'
    assert sigs(old, new) == [(DOCS, "", "", "")]


def test_regular_string_change_is_not_docs():
    assert sigs('x = "old"\n', 'x = "new"\n')[0][0] == REPLACE


def test_comment_and_code_change_keeps_code_signature():
    kinds = {s[0] for s in sigs("f(a)  # old\n", "g(a)  # new\n")}
    assert kinds == {RENAME, DOCS}
