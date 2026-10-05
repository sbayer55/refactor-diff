from refactor_diff.grouping import (
    build_groups,
    find_leftovers,
    inconsistent_renames,
    near_misses,
    still_defined,
)
from refactor_diff.languages.python import PythonAnalyzer
from refactor_diff.model import RENAME, REPLACE, ChangeUnit, Line, Signature


def rename(old, new, detail="call"):
    return Signature(RENAME, f"{RENAME}\x00{old}\x00{new}", old, new, detail)


def replace(old_tokens, new_tokens):
    key = "\x00".join([REPLACE, *old_tokens, "\x01", *new_tokens])
    return Signature(REPLACE, key, " ".join(old_tokens), " ".join(new_tokens))


def unit(uid, path, *signatures):
    return ChangeUnit(uid, path, "h", 1, 1, [], [Line("x = 1")], list(signatures))


def near_for(units, min_count=2):
    groups = build_groups(units, min_count)
    warnings = near_misses({u.id: u for u in units}, groups)
    return groups, warnings


def test_min_count_threshold_and_explained():
    units = [
        unit("1", "a.py", rename("f", "g")),
        unit("2", "b.py", rename("f", "g")),
        unit("3", "c.py", rename("x", "y")),
    ]
    groups = {g.label: g for g in build_groups(units, min_count=2)}
    assert groups["f → g"].mechanical and groups["f → g"].files == ["a.py", "b.py"]
    assert not groups["x → y"].mechanical
    assert [u.explained for u in units] == [True, True, False]


def test_unit_needs_every_signature_mechanical():
    units = [
        unit("1", "a.py", rename("f", "g")),
        unit("2", "b.py", rename("f", "g"), rename("x", "y")),
    ]
    build_groups(units, min_count=2)
    assert [u.explained for u in units] == [True, False]


def test_inconsistent_rename_warning_for_symbols():
    units = [
        unit("1", "a.py", rename("f", "g", "definition")),
        unit("2", "b.py", rename("f", "h")),
    ]
    warnings = inconsistent_renames(build_groups(units, min_count=1))
    assert len(warnings) == 1 and "f was renamed" in warnings[0].message


def test_locals_renamed_differently_are_not_inconsistent():
    units = [
        unit("1", "a.py", rename("user_id", "member_id", "name")),
        unit("2", "b.py", rename("user_id", "staff_id", "keyword")),
    ]
    assert inconsistent_renames(build_groups(units, min_count=1)) == []


def test_leftovers_found_in_code_not_comments_or_strings():
    units = [unit("1", "a.py", rename("f", "g")), unit("2", "a.py", rename("f", "g"))]
    group = build_groups(units, min_count=2)[0]
    analysis = PythonAnalyzer().analyze("g()\n# f is gone\ns = 'f'\nf()\n")
    warning = find_leftovers(group, {"a.py": analysis})
    assert [(loc.path, loc.line) for loc in warning.locations] == [("a.py", 4)]
    assert warning.total == 1


def test_no_leftovers_is_no_warning():
    group = build_groups([unit("1", "a.py", rename("f", "g"))], min_count=1)[0]
    assert find_leftovers(group, {"a.py": PythonAnalyzer().analyze("g()\n")}) is None


def test_still_defined():
    py = PythonAnalyzer()
    assert still_defined("f", py.analyze("def f():\n    pass\n"), py)
    assert still_defined("C", py.analyze("class C:\n    pass\n"), py)
    assert still_defined("X", py.analyze("X: int = 1\n"), py)
    assert not still_defined("f", py.analyze("f()\ny = f\n"), py)
    assert not still_defined("x", py.analyze("def g(x=1):\n    x = 2\n"), py)


def test_near_miss_typo_rename():
    units = [unit(str(i), "a.py", rename("get_user", "fetch_user")) for i in range(3)]
    typo = unit("t", "b.py", rename("get_user", "fetch_users"))
    groups, warnings = near_for(units + [typo])
    [g] = [g for g in groups if g.mechanical]
    assert [n.group_id for n in typo.near] == [g.id]
    assert typo.near[0].score >= 0.9
    assert "fetch_users" in typo.near[0].hint
    [w] = warnings
    assert w.kind == "near-miss" and w.group_id == g.id
    assert [(loc.path, loc.line) for loc in w.locations] == [("b.py", 1)]
    assert all(not u.near for u in units)


def test_unrelated_rename_is_not_a_near_miss():
    units = [unit(str(i), "a.py", rename("get_user", "fetch_user")) for i in range(3)]
    other = unit("o", "b.py", rename("foo", "bar"))
    _, warnings = near_for(units + [other])
    assert other.near == [] and warnings == []


def test_near_miss_template():
    tmpl = ["cfg", ".", "get", "(", "\x02", ")"]
    units = [unit(str(i), "a.py", replace(tmpl, ["settings", ".", "\x02"])) for i in range(2)]
    typo = unit("t", "b.py", replace(tmpl, ["setting", ".", "\x02"]))
    near_for(units + [typo])
    assert len(typo.near) == 1


def test_replace_never_matches_a_rename_group():
    units = [unit(str(i), "a.py", rename("get_user", "fetch_user")) for i in range(3)]
    other = unit("o", "b.py", replace(["get_user"], ["fetch_user", "(", ")"]))
    near_for(units + [other])
    assert other.near == []


def test_near_misses_are_capped_per_unit():
    units = [
        unit(f"{i}-{k}", "a.py", rename("name", f"name_{i:03d}"))
        for i in range(300)
        for k in range(2)
    ]
    typo = unit("t", "b.py", rename("name", "name_9999"))
    near_for(units + [typo])
    assert 1 <= len(typo.near) <= 2
