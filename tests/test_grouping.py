from refactor_diff.grouping import build_groups, find_leftovers, inconsistent_renames
from refactor_diff.languages.python import PythonAnalyzer
from refactor_diff.model import RENAME, ChangeUnit, Signature


def rename(old, new, detail="call"):
    return Signature(RENAME, f"{RENAME}\x00{old}\x00{new}", old, new, detail)


def unit(uid, path, *signatures):
    return ChangeUnit(uid, path, "h", 1, 1, [], [], list(signatures))


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


def test_inconsistent_rename_warning():
    units = [unit("1", "a.py", rename("f", "g")), unit("2", "b.py", rename("f", "h"))]
    warnings = inconsistent_renames(build_groups(units, min_count=1))
    assert len(warnings) == 1 and "f was renamed" in warnings[0].message


def test_leftovers_found_in_code_not_comments_or_strings():
    units = [unit("1", "a.py", rename("f", "g")), unit("2", "a.py", rename("f", "g"))]
    group = build_groups(units, min_count=2)[0]
    analysis = PythonAnalyzer().analyze("g()\n# f is gone\ns = 'f'\nf()\n")
    leftovers = find_leftovers(group, {"a.py": analysis})
    assert [(w.path, w.line) for w in leftovers] == [("a.py", 4)]
