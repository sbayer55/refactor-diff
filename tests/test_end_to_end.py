from refactor_diff.engine import analyze
from refactor_diff.model import FORMATTING, RENAME, REPLACE, RETYPE


def test_fixture_project(rename_repo):
    report = analyze(rename_repo, "main", "feature")
    groups = {(g.kind, g.old, g.new): g for g in report.groups if g.mechanical}

    rename = groups[(RENAME, "get_user", "fetch_user")]
    assert len(rename.unit_ids) == 8
    assert rename.details == {"call": 4, "import": 3, "definition": 1}
    assert groups[(RETYPE, "int", "str")].details == {
        "param user_id": 2,
        "variable owner_id": 1,
        "param owner_id": 1,
    }
    assert len(groups[(REPLACE, 'cfg.get("timeout")', "settings.timeout")].unit_ids) == 2
    assert (FORMATTING, "", "") in groups

    # The only thing a reviewer has to read: the new condition in api.py.
    residual = [u for u in report.units.values() if not u.explained]
    assert [(u.path, u.new_start) for u in residual] == [("api.py", 6)]
    assert [report.hunks[h].path for h in report.residual_hunk_ids] == ["api.py"]

    # legacy.py was never touched but still calls the old name.
    [missed] = [w for w in report.warnings if w.kind == "missed-rename"]
    assert missed.total == 2
    assert [(loc.path, loc.line) for loc in missed.locations] == [
        ("legacy.py", 1),
        ("legacy.py", 5),
    ]

    files = {f.path: f for f in report.files}
    assert not files["README.txt"].analyzed
    assert files["README.txt"].category == "docs"
    assert files["users.py"].category == "source"
    assert files["api.py"].residual_units == 1


def test_worktree_source(rename_repo):
    (rename_repo / "legacy.py").write_text(
        (rename_repo / "legacy.py").read_text().replace("get_user", "fetch_user")
    )
    report = analyze(rename_repo, "feature", ":worktree:")
    assert report.source["head_sha"] is None
    assert [g.label for g in report.groups if g.mechanical] == ["get_user → fetch_user"]
    assert report.stats()["residual_units"] == 0


def test_no_missed_rename_warning_when_old_name_still_defined(rename_repo):
    (rename_repo / "users.py").write_text(
        (rename_repo / "users.py").read_text() + "\n\ndef get_user(user_id):\n    return None\n"
    )
    report = analyze(rename_repo, "main", ":worktree:")
    assert not [w for w in report.warnings if w.kind == "missed-rename"]
