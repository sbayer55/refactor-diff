import pytest

from refactor_diff.cli import defaults_from, parse_args


def test_range_defaults():
    assert defaults_from(parse_args(["main..feature"])) == {
        "mode": "refs",
        "base": "main",
        "head": "feature",
        "editor": "vscode://file/{path}:{line}:{col}",
    }


def test_filter_defaults():
    args = parse_args(
        ["--hide", "tests, docs,comments", "--exclude", "migrations", "--exclude", "*_pb2.py"]
    )
    assert defaults_from(args) == {
        "filters": {
            "hidden": ["docs", "tests"],
            "hideDocs": True,
            "exclude": ["migrations", "*_pb2.py"],
        },
        "editor": "vscode://file/{path}:{line}:{col}",
    }


def test_unknown_hide_value_exits():
    with pytest.raises(SystemExit):
        defaults_from(parse_args(["--hide", "testz"]))


def test_editor_presets_and_templates():
    assert (
        defaults_from(parse_args(["--editor", "Zed"]))["editor"] == "zed://file/{path}:{line}:{col}"
    )
    custom = "x-mine://{path}?l={line}"
    assert defaults_from(parse_args(["--editor", custom]))["editor"] == custom
    with pytest.raises(SystemExit):
        defaults_from(parse_args(["--editor", "notepad"]))
