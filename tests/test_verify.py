from pathlib import Path

from conftest import commit, git

from refactor_diff.engine import analyze


def report_for(tmp_path: Path, before: dict, after: dict):
    repo = tmp_path / "repo"
    commit(repo, before, "before")
    git(repo, "checkout", "-qb", "feature")
    commit(repo, after, "after")
    return analyze(repo, "main", "feature")


def verified(report) -> dict[tuple[str, int], bool]:
    return {(u.path, u.new_start): u.verified for u in report.units.values()}


BEFORE = '''def get_user(uid):
    """Load a user."""
    return db[uid]


def show(uid):
    user = get_user(uid)
    print(user)


def hide(uid):
    user = get_user(uid)
    del user
'''


def test_mechanical_rename_is_verified(tmp_path):
    report = report_for(
        tmp_path, {"a.py": BEFORE}, {"a.py": BEFORE.replace("get_user", "fetch_user")}
    )
    assert all(verified(report).values())
    assert report.stats()["verified_units"] == 3


def test_rename_plus_logic_change_is_not_verified(tmp_path):
    after = BEFORE.replace("get_user", "fetch_user").replace(
        "    print(user)", "    if user:\n        print(user)"
    )
    report = report_for(tmp_path, {"a.py": BEFORE}, {"a.py": after})
    v = verified(report)
    assert v[("a.py", 1)] is True  # the def
    assert v[("a.py", 13)] is True  # hide()
    assert not any(ok for (p, line), ok in v.items() if 6 <= line <= 10)  # show()


def test_docstring_and_retype_are_verified(tmp_path):
    before = (
        "from typing import List\n\n\n"
        'def f(xs: List[int]) -> List[int]:\n    """Old."""\n    return xs\n'
    )
    after = before.replace("List[int]", "list[int]").replace("Old.", "New words.")
    report = report_for(tmp_path, {"a.py": before}, {"a.py": after})
    assert all(verified(report).values())


def test_conflicting_renames_in_one_function_are_not_verified(tmp_path):
    before = "def f(a):\n    x = a\n    y = a\n    return x, y\n\n\ndef g(a):\n    return a\n"
    after = "def f(a):\n    x = b\n    y = c\n    return x, y\n\n\ndef g(b):\n    return b\n"
    report = report_for(tmp_path, {"a.py": before}, {"a.py": after})
    assert not any(ok for (p, line), ok in verified(report).items() if line < 6)


def test_inserted_top_level_function_is_not_verified(tmp_path):
    before = "def f():\n    return 1\n"
    report = report_for(
        tmp_path, {"a.py": before}, {"a.py": before + "\n\ndef g():\n    return 2\n"}
    )
    assert not any(verified(report).values())


def test_decorator_rename_is_verified(tmp_path):
    before = "@old_dec\ndef f():\n    return 1\n\n\n@old_dec\ndef g():\n    return 2\n"
    report = report_for(tmp_path, {"a.py": before}, {"a.py": before.replace("old_dec", "new_dec")})
    assert all(verified(report).values())


def test_syntax_error_side_does_not_break(tmp_path):
    report = report_for(tmp_path, {"a.py": "def f(:\n    pass\n"}, {"a.py": "def f():\n    pass\n"})
    assert not any(verified(report).values())
