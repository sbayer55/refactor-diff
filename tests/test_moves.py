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
    labels = {(g.kind, g.label) for g in report.groups if not g.mechanical}
    assert labels == {("args", "def helper(…) → def helper(…, …)")}
    assert "(indentation)" not in " ".join(g.label for g in report.groups)


def test_import_path_change_is_explained_by_the_move(tmp_path):
    repo = repo_with(
        tmp_path,
        {
            "pkg/__init__.py": "",
            "pkg/a.py": HELPER,
            "pkg/b.py": "X = 1\n",
            "pkg/c.py": "from pkg.a import helper\n\nprint(helper(1, 2))\n",
        },
        {
            "pkg/__init__.py": "",
            "pkg/a.py": "",
            "pkg/b.py": "X = 1\n\n\n" + HELPER,
            "pkg/c.py": "from pkg.b import helper\n\nprint(helper(1, 2))\n",
        },
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert len(move.unit_ids) == 3
    assert report.stats()["residual_units"] == 0
    [imp] = [u for u in report.units.values() if u.path == "pkg/c.py"]
    assert [s.kind for s in imp.signatures] == [MOVE]


def test_import_needed_by_the_moved_block_is_linked(tmp_path):
    block = "def stamp():\n    now = time.time()\n    label = str(now)\n    return label\n"
    repo = repo_with(
        tmp_path,
        {"a.py": "import time\n\n\n" + block, "b.py": "X = 1\n"},
        {"a.py": "", "b.py": "import time\n\nX = 1\n\n\n" + block},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert len(move.unit_ids) == 4  # block out, block in, import out, import in
    assert report.stats()["residual_units"] == 0


def test_relative_import_links_to_a_move_inside_the_package(tmp_path):
    repo = repo_with(
        tmp_path,
        {
            "pkg/__init__.py": "",
            "pkg/a.py": HELPER,
            "pkg/b.py": "X = 1\n",
            "pkg/c.py": "from .a import helper\n\nprint(helper(1, 2))\n",
        },
        {
            "pkg/__init__.py": "",
            "pkg/a.py": "",
            "pkg/b.py": "X = 1\n\n\n" + HELPER,
            "pkg/c.py": "from .b import helper\n\nprint(helper(1, 2))\n",
        },
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert len(move.unit_ids) == 3
    assert report.stats()["residual_units"] == 0


def test_repeated_import_path_change_groups(tmp_path):
    files = {f"m{i}.py": "from a import x\n\nprint(x)\n" for i in range(3)}
    repo = repo_with(tmp_path, files, {k: v.replace("from a", "from b") for k, v in files.items()})
    report = analyze(repo, "main", "feature")
    [g] = [g for g in report.groups if g.kind == "import"]
    assert g.mechanical and g.label == "a → b" and g.details == {"x": 3}


def test_added_parameter_groups_def_with_call_sites(tmp_path):
    before = "def fetch(a, b):\n    return a + b\n\n\nx = fetch(1, 2)\ny = fetch(3, 4)\n"
    after = (
        "def fetch(a, b, timeout=None):\n    return a + b\n\n\n"
        "x = fetch(1, 2, timeout=5)\ny = fetch(3, 4, timeout=cfg.t)\n"
    )
    repo = repo_with(tmp_path, {"a.py": before}, {"a.py": after})
    report = analyze(repo, "main", "feature")
    [g] = [g for g in report.groups if g.kind == "args"]
    assert g.mechanical and g.details == {"call": 2, "definition": 1}
    assert g.label == "def fetch(…) → def fetch(…, timeout=…)"
    assert report.stats()["residual_units"] == 0


def test_moves_in_typescript_do_not_need_verification(tmp_path):
    fn = (
        "export function helper(x: number, y: number): number {\n  const total = x + y;\n"
        "  if (total > 10) {\n    return total * 2;\n  }\n  return total;\n}\n"
    )
    repo = repo_with(
        tmp_path,
        {"a.ts": fn + "\nexport const X = 1;\n", "b.ts": "export const Y = 2;\n"},
        {"a.ts": "export const X = 1;\n", "b.ts": "export const Y = 2;\n\n" + fn},
    )
    report = analyze(repo, "main", "feature")
    [move] = move_groups(report)
    assert move.label == "moved helper: a.ts → b.ts"
    assert report.stats()["residual_units"] == 0
    assert not any(u.verified for u in report.units.values())
