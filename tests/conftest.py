import shutil
import subprocess
from pathlib import Path

import pytest

FIXTURES = Path(__file__).parent / "fixtures"


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


@pytest.fixture
def rename_repo(tmp_path: Path) -> Path:
    """A git repo with `main` (before/) and `feature` (after/) branches."""
    return _fixture_repo(FIXTURES / "rename_project", tmp_path / "repo")


@pytest.fixture
def ts_rename_repo(tmp_path: Path) -> Path:
    """The TypeScript counterpart of `rename_repo`."""
    return _fixture_repo(FIXTURES / "ts_rename_project", tmp_path / "ts-repo")


def _fixture_repo(project: Path, repo: Path) -> Path:
    shutil.copytree(project / "before", repo)
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "add", "-A")
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "before")
    _git(repo, "checkout", "-qb", "feature")
    for f in (project / "after").iterdir():
        shutil.copy(f, repo / f.name)
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "after")
    return repo
