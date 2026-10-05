from refactor_diff.languages.python import PythonAnalyzer
from refactor_diff.model import ARGS, DOCS, FORMATTING, IMPORT, RENAME, REPLACE, RETYPE
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


def keys(old: str, new: str):
    return [s.key for s in classify_one(old, new).signatures]


# --- imports ---


def test_import_module_change():
    assert sigs("from a import x\n", "from b import x\n") == [(IMPORT, "a", "b", "x")]
    assert keys("from a import x\n", "from b import x\n") == keys(
        "from a import x as y\n", "from b import x as y\n"
    )[:0] + [f"{IMPORT}\x00a\x00b"]


def test_import_renamed_name_stays_a_rename():
    assert sigs("from a import x\n", "from a import y\n") == [(RENAME, "x", "y", "import")]


def test_import_module_and_name_change_falls_through():
    assert all(k != IMPORT for k, *_ in sigs("from a import x\n", "from b import y\n"))


def test_import_added_and_removed_names():
    assert sigs("import os\n", "import os\nimport sys\n") == [(IMPORT, "", "import sys", "sys")]
    assert sigs("from a import x, y\n", "from a import x\n") == [
        (IMPORT, "from a import y", "", "y")
    ]
    assert keys("", "from __future__ import annotations\n") == [
        f"{IMPORT}\x00\x00__future__.annotations"
    ]


def test_import_multiline_only_module_line_changed():
    old = "from a import (\n    x,\n    y,\n)\n"
    new = old.replace("from a", "from b")
    a, b = PY.analyze(old), PY.analyze(new)
    result = classify(PY, a, b, (1, 1), (1, 1))  # the unit is the first line only
    assert [(s.kind, s.old, s.new) for s in result.signatures] == [(IMPORT, "a", "b")]


def test_import_mixed_with_code_is_not_an_import_change():
    assert all(
        k != IMPORT for k, *_ in sigs("from a import x\nz = 1\n", "from b import x\nz = 2\n")
    )


# --- argument lists ---


def test_added_keyword_argument_groups_across_values_and_with_the_def():
    call1 = keys("fetch(1, 2)\n", "fetch(1, 2, timeout=5)\n")
    call2 = keys("fetch(x, y)\n", "fetch(x, y, timeout=cfg.t)\n")
    definition = keys("def fetch(a, b):\n    pass\n", "def fetch(a, b, timeout=None):\n    pass\n")
    assert call1 == call2 == definition == [f"{ARGS}\x00fetch\x00+kw:timeout"]
    assert sigs("fetch(1, 2)\n", "fetch(1, 2, timeout=5)\n") == [
        (ARGS, "fetch(…)", "fetch(…, timeout=…)", "call")
    ]
    assert sigs("def fetch(a, b):\n    pass\n", "def fetch(a, b, timeout=None):\n    pass\n")[0][
        3
    ] == ("definition")


def test_removed_keyword_argument():
    assert keys("f(x, verbose=True)\n", "f(x)\n") == [f"{ARGS}\x00f\x00-kw:verbose"]


def test_positional_to_keyword():
    assert keys("f(x, 5)\n", "f(x, timeout=5)\n") == [f"{ARGS}\x00f\x00pos>kw:timeout"]


def test_edit_inside_an_argument_is_not_an_args_change():
    found = sigs("fetch(x, y)\n", "fetch(x, y + 1, timeout=5)\n")
    assert all(k != ARGS for k, *_ in found)


def test_method_and_function_calls_share_the_key():
    assert keys("client.fetch(x)\n", "client.fetch(x, timeout=1)\n") == keys(
        "fetch(x)\n", "fetch(x, timeout=2)\n"
    )


def test_nested_call_attributes_to_the_inner_call():
    assert keys("log(fetch(x))\n", "log(fetch(x, timeout=1))\n") == [
        f"{ARGS}\x00fetch\x00+kw:timeout"
    ]


def test_replaced_positional_plus_added_kwarg_is_not_an_args_change():
    found = sigs("fetch(x, y)\n", "fetch(x, z, timeout=5)\n")
    assert all(k != ARGS for k, *_ in found)


def test_added_argument_line_in_multiline_call():
    old = "r = fetch(\n    x,\n)\n"
    new = "r = fetch(\n    x,\n    timeout=5,\n)\n"
    a, b = PY.analyze(old), PY.analyze(new)
    result = classify(PY, a, b, None, (3, 3), old_anchor=2, new_anchor=2)
    assert [s.key for s in result.signatures] == [f"{ARGS}\x00fetch\x00+kw:timeout"]


def test_added_positional_parameter():
    assert keys("def f(a):\n    pass\n", "def f(a, b):\n    pass\n") == [f"{ARGS}\x00f\x00+pos"]
    assert keys("f(1)\n", "f(1, 2)\n") == [f"{ARGS}\x00f\x00+pos"]
