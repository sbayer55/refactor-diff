import json

import pytest

from refactor_diff import prefs


def test_round_trip_and_delete(tmp_path):
    assert prefs.load(tmp_path) == {}
    prefs.update({"refactor-diff:layout": "split", "refactor-diff:panes": "{}"}, tmp_path)
    assert prefs.load(tmp_path) == {"refactor-diff:layout": "split", "refactor-diff:panes": "{}"}
    left = prefs.update({"refactor-diff:panes": None}, tmp_path)
    assert left == {"refactor-diff:layout": "split"}
    assert prefs.load(tmp_path) == {"refactor-diff:layout": "split"}


def test_corrupt_or_foreign_file_is_ignored(tmp_path):
    prefs.prefs_path(tmp_path).write_text("{not json")
    assert prefs.load(tmp_path) == {}
    prefs.prefs_path(tmp_path).write_text(json.dumps({"other": "x", "refactor-diff:a": 1}))
    assert prefs.load(tmp_path) == {}
    prefs.update({"refactor-diff:b": "y"}, tmp_path)
    assert prefs.load(tmp_path) == {"refactor-diff:b": "y"}


@pytest.mark.parametrize(
    "changes",
    [{"theme": "dark"}, {"refactor-diff:layout": 3}, ["refactor-diff:layout"], None],
)
def test_rejects_bad_changes(tmp_path, changes):
    with pytest.raises(ValueError):
        prefs.update(changes, tmp_path)
    assert not prefs.prefs_path(tmp_path).exists()


def test_size_cap(tmp_path):
    with pytest.raises(ValueError):
        prefs.update({"refactor-diff:big": "x" * (prefs.MAX_BYTES + 1)}, tmp_path)
