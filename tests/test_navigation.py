import subprocess
import sys

import pytest
from starlette.testclient import TestClient

from refactor_diff.navigation import LIBRARY, REPO, NavigationError, Navigator
from refactor_diff.snapshots import Snapshots
from refactor_diff.sources import rev
from refactor_diff.web.server import create_app


def git(repo, *args):
    subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
        check=True,
        capture_output=True,
    )


@pytest.fixture
def nav(rename_repo, tmp_path):
    snaps = Snapshots(rename_repo, tmp_path / "snaps")
    yield (
        Navigator(rename_repo, snaps, sys.executable),
        rev(rename_repo, "main"),
        rev(rename_repo, "feature"),
    )
    snaps.close()


def where(locations):
    return [(loc.kind, loc.path, loc.line) for loc in locations]


def test_definition_on_new_side(nav):
    navigator, _, head = nav
    # api.py line 5: "    user = fetch_user(user_id)"
    assert where(navigator.definitions(head, "api.py", 5, 12)) == [(REPO, "users.py", 4)]


def test_definition_on_old_side_uses_base_revision(nav):
    navigator, base, _ = nav
    # api.py line 5 at base: "    user = get_user(user_id)"
    assert where(navigator.definitions(base, "api.py", 5, 12)) == [(REPO, "users.py", 4)]
    assert navigator.definitions(base, "api.py", 5, 12)[0].name == "get_user"


def test_references_follow_the_revision(nav):
    navigator, base, head = nav
    new_files = {loc.path for loc in navigator.references(head, "users.py", 4, 4)}
    old_files = {loc.path for loc in navigator.references(base, "users.py", 4, 4)}
    assert new_files == {"api.py", "billing.py", "reports.py", "users.py"}
    assert old_files == new_files | {"legacy.py"}  # legacy.py still calls get_user


def test_builtin_resolves_to_a_stub(nav):
    navigator, _, head = nav
    # reports.py line 7: '    return {"count": len(users), ...'
    [loc] = navigator.definitions(head, "reports.py", 7, 24)
    assert loc.kind == LIBRARY and loc.path.endswith("builtins.pyi") and loc.name == "len"
    assert "def len" in navigator.library_source(loc.path)


def test_library_source_only_for_navigated_files(nav):
    navigator, _, _ = nav
    with pytest.raises(NavigationError):
        navigator.library_source("/etc/hosts")


def test_missing_file_is_an_error(nav):
    navigator, base, _ = nav
    with pytest.raises(NavigationError):
        navigator.definitions(base, "nope.py", 1, 0)


def test_snapshot_has_only_python_files(rename_repo, tmp_path):
    snaps = Snapshots(rename_repo, tmp_path / "snaps")
    root = snaps.root(rev(rename_repo, "main"))
    assert sorted(p.name for p in root.iterdir()) == sorted(
        p.name for p in rename_repo.iterdir() if p.suffix == ".py"
    )
    assert snaps.root(None) == rename_repo


def test_editable_install_resolves_into_the_snapshot(tmp_path):
    """The venv's editable install points at the checkout; navigation at head must still land
    in the head revision, not the (older) checked-out file."""
    repo = tmp_path / "proj"
    (repo / "src/pkg").mkdir(parents=True)
    (repo / "src/pkg/__init__.py").write_text("")
    (repo / "src/pkg/mod.py").write_text("def f():\n    pass\n")
    (repo / "src/pkg/use.py").write_text("from pkg.mod import f\n\nf()\n")
    git(repo, "init", "-q", "-b", "main")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "base")
    (repo / "src/pkg/mod.py").write_text("X = 1\n\n\ndef f():\n    pass\n")
    git(repo, "commit", "-qam", "move f")
    head = rev(repo, "HEAD")
    git(repo, "checkout", "-q", "HEAD~1")  # the checkout now has f() on line 1

    subprocess.run([sys.executable, "-m", "venv", "--without-pip", str(repo / ".venv")], check=True)
    site = next((repo / ".venv/lib").glob("python*/site-packages"))
    (site / "_pkg_editable.pth").write_text(str(repo / "src") + "\n")

    snaps = Snapshots(repo, tmp_path / "snaps")
    navigator = Navigator(repo, snaps)  # finds .venv on its own
    [loc] = navigator.definitions(head, "src/pkg/use.py", 3, 0)
    assert (loc.kind, loc.path, loc.line) == (REPO, "src/pkg/mod.py", 4)
    snaps.close()


def test_navigate_and_source_endpoints(rename_repo):
    with TestClient(create_app(rename_repo, python=sys.executable)) as client:
        report = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
        url = f"/api/report/{report['id']}"
        res = client.post(
            f"{url}/navigate",
            json={"action": "definition", "side": "new", "path": "api.py", "line": 5, "col": 12},
        ).json()
        assert [(loc["path"], loc["line"]) for loc in res["locations"]] == [("users.py", 4)]

        refs = client.post(
            f"{url}/navigate",
            json={"action": "references", "side": "old", "path": "users.py", "line": 4, "col": 4},
        ).json()
        assert "legacy.py" in {loc["path"] for loc in refs["locations"]}

        src = client.get(f"{url}/source", params={"side": "old", "path": "users.py"}).json()
        assert src["lines"][3] == "def get_user(user_id: int) -> dict:"
        missing = client.get(f"{url}/source", params={"side": "new", "path": "nope.py"})
        assert missing.status_code == 404

        bad = client.post(
            f"{url}/navigate", json={"action": "x", "side": "new", "line": 1, "col": 0}
        )
        assert bad.status_code == 400
        assert client.get("/api/library", params={"path": "/etc/hosts"}).status_code == 404
