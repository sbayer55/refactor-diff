from pathlib import Path

from conftest import commit, git

from refactor_diff.engine import analyze
from refactor_diff.model import MOVE, RENAME, REPLACE

HELPER = '''def helper(x, y):
    """Add things up."""
    total = x + y
    if total > 10:
        return total * 2
    return total
'''

OTHER = """def other(a):
    b = a - 1
    return b * b
"""


def repo_with(tmp_path: Path, before: dict, after: dict) -> Path:
    repo = tmp_path / "repo"
    commit(repo, before, "before")
    git(repo, "checkout", "-qb", "feature")
    commit(repo, after, "after")
    return repo


def move_groups(report):
    return [g for g in report.groups if g.kind == MOVE]


def test_exact_move_between_files(tmp_path):
    repo = repo_with(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER, "b.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": "X = 1\n\n\n" + HELPER},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert move.mechanical
    assert move.label == "moved helper: a.py → b.py"
    assert len(move.unit_ids) == 2
    assert report.stats()["residual_units"] == 0
    old, new = (report.units[u] for u in move.unit_ids)
    assert old.partner == new.id and new.partner == old.id
    assert old.verified and new.verified


def test_move_with_edit_inside_leaves_only_the_edit(tmp_path):
    edited = HELPER.replace("total * 2", "total * 3")
    repo = repo_with(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER, "b.py": "X = 1\n"},
        {"a.py": OTHER, "b.py": "X = 1\n\n\n" + edited},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    residual = [u for u in report.units.values() if not u.explained]
    assert [u.path for u in residual] == ["b.py"]
    [new] = residual
    kinds = {s.kind for s in new.signatures}
    assert kinds == {MOVE, REPLACE}
    # Only the changed token is highlighted on the moved block.
    marked = [ln.text[s:e] for ln in new.new for s, e in ln.hl]
    assert marked == ["3"]
    assert not new.verified


def test_move_with_rename_inside_joins_the_rename_group(tmp_path):
    renamed = HELPER.replace("total", "amount")
    repo = repo_with(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER + "\n\ntotal = 1\nprint(total)\n", "b.py": ""},
        {"a.py": OTHER + "\n\namount = 1\nprint(amount)\n", "b.py": renamed},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    [rename] = [g for g in report.groups if g.kind == RENAME]
    assert (rename.old, rename.new) == ("total", "amount")
    assert rename.mechanical
    assert report.stats()["residual_units"] == 0


def test_tiny_block_is_not_a_move(tmp_path):
    repo = repo_with(
        tmp_path,
        {"a.py": "def f():\n    return None\n\n\ndef g():\n    return 1\n"},
        {"a.py": "def f():\n    return 2\n\n\ndef g():\n    return None\n"},
    )
    report = analyze(repo, "main", "feature")
    assert not move_groups(report)


def test_same_file_reorder(tmp_path):
    repo = repo_with(
        tmp_path,
        {"a.py": HELPER + "\n\n" + OTHER},
        {"a.py": OTHER + "\n\n" + HELPER},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    # difflib keeps the larger block in place, so it is `other` that moved.
    assert move.label == "moved other: within a.py"
    assert report.stats()["residual_units"] == 0


def test_one_function_out_of_a_deleted_file(tmp_path):
    second = '''def second(q):
    """Another one."""
    for item in q:
        yield item * 2
'''
    repo = repo_with(
        tmp_path,
        {"util.py": HELPER + "\n\n" + second, "core.py": "Y = 2\n"},
        {"util.py": None, "core.py": "Y = 2\n\n\n" + HELPER + "\n\ndef brand_new():\n    pass\n"},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert move.label == "moved helper: util.py → core.py"
    first = lambda u: next(ln.text for ln in (u.old or u.new) if ln.text.strip())  # noqa: E731
    residual = sorted((u.path, first(u)) for u in report.units.values() if not u.explained)
    assert residual == [("core.py", "def brand_new():"), ("util.py", "def second(q):")]
    # The deleted file's hunk still lists every line, now split across units.
    [hunk] = [h for h in report.hunks.values() if h.path == "util.py"]
    assert all(ln.unit in report.units for ln in hunk.lines)
    assert len(hunk.unit_ids) >= 2


def test_function_moved_into_a_class(tmp_path):
    method = "\n".join(
        "    " + ln if ln else ln for ln in HELPER.replace("(x, y)", "(self, x, y)").splitlines()
    )
    cls = "class C:\n    def m(self):\n        return 1\n"
    repo = repo_with(
        tmp_path,
        {"a.py": HELPER + "\n\n" + cls},
        {"a.py": cls + "\n" + method + "\n"},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    labels = {g.label for g in report.groups if not g.mechanical}
    assert labels == {"insert self,"}
    assert "(indentation)" not in " ".join(g.label for g in report.groups)
