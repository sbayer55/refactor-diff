import sys

import pytest

from refactor_diff.ai import tasks
from refactor_diff.ai.context import ContextBuilder, ContextError, render
from refactor_diff.engine import analyze
from refactor_diff.navigation import Navigator
from refactor_diff.snapshots import Snapshots


def hunk_for(report, path):
    return next(h for h in report.hunks.values() if h.path == path)


@pytest.fixture
def py_report(rename_repo):
    return analyze(rename_repo, "main", "feature", None, 2)


@pytest.fixture
def builder(rename_repo, py_report):
    return ContextBuilder(py_report, rename_repo)


def test_focus_defaults_to_the_hunks_anchor_line(builder, py_report):
    hunk = hunk_for(py_report, "api.py")
    focus = builder.focus(hunk.id)
    # The first unexplained change, new side: the `if` line that gained a condition.
    assert (focus.path, focus.side, focus.line) == ("api.py", "new", 6)


def test_focus_validates_the_line(builder, py_report):
    hunk = hunk_for(py_report, "api.py")
    assert builder.focus(hunk.id, "o", 5).side == "old"
    assert builder.focus(hunk.id, "new", 7).line == 7
    with pytest.raises(ContextError):
        builder.focus(hunk.id, "n", 99)
    with pytest.raises(ContextError):
        builder.focus(hunk.id, "sideways", 5)
    with pytest.raises(ContextError):
        builder.focus("nope")


def test_hunk_piece_annotates_patterns(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id)
    text = builder.hunk_piece(focus)
    assert "[[pattern: rename get_user → fetch_user (×8 in" in text
    assert "[[unique replace:" in text
    assert (
        "    5     5 +     user = fetch_user(user_id)" in text
        or "5 +     user = fetch_user" in text
    )
    assert builder.unexplained_lines(focus) == 2  # the `if` line, old and new


def test_function_piece_finds_both_sides(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id)
    fn = builder.function_piece(focus)
    assert fn.qualname == "handle" and fn.kind == "def"
    assert fn.new.start == 4 and fn.new.end == 8 and 'user.get("active")' in fn.new.text
    assert fn.old.start == 4 and "user_id: int" in fn.old.text
    assert fn.old.path == "api.py"


def test_function_piece_follows_the_rename_to_the_old_side(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "users.py").id, "n", 4)
    fn = builder.function_piece(focus)
    assert fn.qualname == "fetch_user"
    assert "def get_user" in fn.old.text
    # Asked from the old side, the new counterpart is found the same way.
    focus = builder.focus(hunk_for(py_report, "users.py").id, "o", 4)
    fn = builder.function_piece(focus)
    assert fn.qualname == "get_user" and "def fetch_user" in fn.new.text


def test_function_piece_is_none_outside_a_def(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id, "n", 1)  # the import line
    assert builder.function_piece(focus) is None


def test_function_piece_for_typescript(ts_rename_repo):
    report = analyze(ts_rename_repo, "main", "feature", None, 2)
    builder = ContextBuilder(report, ts_rename_repo)
    focus = builder.focus(hunk_for(report, "api.ts").id, "n", 4)
    fn = builder.function_piece(focus)
    assert fn.qualname == "handle" and fn.old and fn.new
    assert "```ts" in render(builder.collect(focus, {"function"}))


def test_history_piece_names_the_commit_that_touched_the_line(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id, "n", 6)
    hist = builder.history_piece(focus)
    assert [c["subject"] for c in hist["range"]] == ["after"]
    assert [c["subject"] for c in hist["touching"]] == ["after"]
    # A deleted line is found by its text.
    focus = builder.focus(hunk_for(py_report, "api.py").id, "o", 6)
    assert [c["subject"] for c in builder.history_piece(focus)["touching"]] == ["after"]


def test_pr_and_patterns_pieces(builder, py_report):
    assert builder.pr_piece() is None
    py_report.source["pr"] = {"number": 7, "title": "Rename", "url": "u", "body": " why \n"}
    assert builder.pr_piece() == {"number": 7, "title": "Rename", "url": "u", "body": "why"}
    pats = builder.patterns_piece()
    assert pats[0].startswith("rename get_user → fetch_user ×8 in")
    assert not any("insert or not" in p for p in pats)  # unique edits aren't patterns


def test_render_lays_out_every_block(builder, py_report):
    py_report.source["pr"] = {"number": 7, "title": "Rename", "url": "u", "body": "because"}
    focus = builder.focus(hunk_for(py_report, "api.py").id)
    text = render(builder.collect(focus, {"function", "history", "pr", "references"}))
    headings = [ln for ln in text.splitlines() if ln.startswith("## ")]
    assert headings[0] == "## Location"
    assert headings[1].startswith("## Hunk (api.py")
    assert "## Enclosing def handle — before (api.py:4–8)" in headings
    assert "## Enclosing def handle — after (api.py:4–8)" in headings
    assert "## References" in headings  # unavailable without a navigator
    assert "## Commits that touched this line" in headings
    assert "## Pull request #7: Rename" in headings
    assert headings[-1] == "## Mechanical patterns in this diff"
    assert "```python\ndef handle(request, user_id: str):" in text
    assert "because" in text
    # Pieces that weren't asked for don't appear at all.
    text = render(builder.collect(focus, set()))
    assert "## Enclosing" not in text and "## Commits" not in text and "## Pull request" not in text


@pytest.fixture
def nav_builder(rename_repo, py_report, tmp_path):
    snaps = Snapshots(rename_repo, tmp_path / "snaps")
    navigator = Navigator(rename_repo, snaps, sys.executable)
    yield ContextBuilder(py_report, rename_repo, navigator)
    snaps.close()


def test_references_piece_uses_the_navigator(nav_builder, py_report):
    focus = nav_builder.focus(hunk_for(py_report, "users.py").id, "n", 4)
    fn = nav_builder.function_piece(focus)
    refs = nav_builder.references_piece(focus, fn)
    assert refs["name"] == "fetch_user" and refs["side"] == "new"
    assert {loc["path"] for loc in refs["locations"]} == {
        "api.py",
        "billing.py",
        "reports.py",
        "users.py",
    }
    assert any(loc["def"] for loc in refs["locations"])
    text = render(nav_builder.collect(focus, {"function", "references"}))
    assert (
        "## References to fetch_user (" in text and "api.py:5  user = fetch_user(user_id)" in text
    )
    assert nav_builder.references_available(focus) is None


def test_references_unavailable_without_navigation(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id)
    assert builder.references_available(focus) == "code navigation is off"
    assert builder.references_piece(focus, builder.function_piece(focus)) is None


# --- tasks --------------------------------------------------------------------------------------


def test_menu_rows_carry_hints_and_availability(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id)
    rows = {r["id"]: r for r in tasks.menu(builder, focus)}
    assert list(rows) == [
        "explain",
        "function",
        "compare",
        "uses",
        "why",
        "review",
        "preserving",
        "break",
    ]
    assert (
        rows["explain"]["hint"] == "2 unexplained lines" and rows["explain"]["unavailable"] is None
    )
    assert rows["function"]["hint"] == "handle()"
    assert rows["uses"]["unavailable"] == "code navigation is off"
    assert rows["why"]["hint"] == "1 commit" and rows["why"]["unavailable"] is None
    assert rows["preserving"]["hint"] == "not verified"
    assert rows["review"]["group"] == "Review and risk"


def test_menu_outside_a_def_and_on_the_working_tree(builder, py_report):
    focus = builder.focus(hunk_for(py_report, "api.py").id, "n", 1)
    rows = {r["id"]: r for r in tasks.menu(builder, focus)}
    assert rows["function"]["unavailable"] == "the line isn't inside a function or class"
    assert rows["compare"]["unavailable"] == "the line isn't inside a function or class"
    py_report.source["head_sha"] = None
    rows = {r["id"]: r for r in tasks.menu(builder, focus)}
    assert rows["why"]["unavailable"] == "no commits or pull request for the working tree"


def test_system_prompt_adds_the_task_and_verdict(builder):
    assert tasks.system_prompt(tasks.BY_ID["explain"]).startswith(tasks.SYSTEM_PROMPT)
    assert "Task: Explain what this hunk changes" in tasks.system_prompt(tasks.BY_ID["explain"])
    assert "Verdict:" not in tasks.system_prompt(tasks.BY_ID["explain"])
    assert "Verdict:" in tasks.system_prompt(tasks.BY_ID["review"])
    assert tasks.BY_ID["custom"] is tasks.CUSTOM
