import shutil
import subprocess
from pathlib import Path

import pytest

FIXTURES = Path(__file__).parent / "fixtures"


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


def git(repo: Path, *args: str) -> str:
    """Run git with a fixed identity and return its output."""
    proc = subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
        check=True,
        capture_output=True,
    )
    return proc.stdout.decode().strip()


def commit(repo: Path, files: dict[str, str | None], message: str = "edit") -> str:
    """Write ``files`` (None deletes), commit everything and return the commit sha. Creates
    the repository on first use."""
    if not (repo / ".git").exists():
        repo.mkdir(parents=True, exist_ok=True)
        git(repo, "init", "-q", "-b", "main")
    for path, text in files.items():
        f = repo / path
        if text is None:
            f.unlink()
        else:
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(text)
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", message)
    return git(repo, "rev-parse", "HEAD")


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


@pytest.fixture(autouse=True)
def isolated_config(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Keep review state written by tests out of the real ~/.config."""
    cfg = tmp_path / "config"
    monkeypatch.setenv("XDG_CONFIG_HOME", str(cfg))
    return cfg
