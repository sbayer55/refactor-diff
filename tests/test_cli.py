import pytest

from refactor_diff.cli import defaults_from, parse_args


def test_range_defaults():
    assert defaults_from(parse_args(["main..feature"])) == {
        "mode": "refs",
        "base": "main",
        "head": "feature",
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
        }
    }


def test_unknown_hide_value_exits():
    with pytest.raises(SystemExit):
        defaults_from(parse_args(["--hide", "testz"]))
