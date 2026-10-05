import subprocess

import pytest

from refactor_diff import sources
from refactor_diff.engine import analyze


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True, capture_output=True, text=True
    ).stdout.strip()


@pytest.fixture
def clone(rename_repo, tmp_path):
    """A clone of rename_repo: `feature` exists only as origin/feature."""
    path = tmp_path / "clone"
    subprocess.run(["git", "clone", "-q", "-b", "main", str(rename_repo), str(path)], check=True)
    return path


def test_ref_falls_back_to_remote_tracking_branch(clone):
    assert sources.resolve_ref(clone, "feature") == git(clone, "rev-parse", "origin/feature")
    report = analyze(clone, "main", "feature")
    assert report.stats()["residual_units"] == 1


def test_ref_missing_locally_is_fetched_from_remote(rename_repo, clone):
    git(rename_repo, "branch", "late", "feature")
    sha = sources.resolve_ref(clone, "late")
    assert sha == git(rename_repo, "rev-parse", "late")
    assert git(clone, "rev-parse", "origin/late") == sha


def test_unknown_ref_error_mentions_remote(clone):
    with pytest.raises(sources.SourceError, match="origin/nope"):
        sources.resolve_ref(clone, "nope")
