"""TypeScript navigation through tsserver. Skipped unless one is available: set TSSERVER to a
tsserver executable or TypeScript's lib/tsserver.js, or put tsserver on PATH."""

import os
import shutil

import pytest

from refactor_diff.navigation import LIBRARY, REPO, NavigationError, Navigator
from refactor_diff.snapshots import Snapshots
from refactor_diff.sources import rev
from refactor_diff.tsserver import _find_node

TSSERVER = os.environ.get("TSSERVER") or shutil.which("tsserver")
pytestmark = pytest.mark.skipif(
    TSSERVER is None or _find_node() is None, reason="needs tsserver (set TSSERVER) and Node.js"
)


@pytest.fixture
def nav(ts_rename_repo, tmp_path):
    snaps = Snapshots(ts_rename_repo, tmp_path / "snaps")
    navigator = Navigator(ts_rename_repo, snaps, tsserver=TSSERVER)
    yield navigator, rev(ts_rename_repo, "main"), rev(ts_rename_repo, "feature")
    navigator.close()
    snaps.close()


def where(locations):
    return [(loc.kind, loc.path, loc.line) for loc in locations]


def test_definition_on_new_side(nav):
    navigator, _, head = nav
    # api.ts line 4: "  const user = fetchUser(userId);"
    [loc] = navigator.definitions(head, "api.ts", 4, 16)
    assert (loc.kind, loc.path, loc.line, loc.col, loc.name) == (
        REPO,
        "users.ts",
        3,
        16,
        "fetchUser",
    )


def test_definition_on_old_side_uses_base_revision(nav):
    navigator, base, _ = nav
    [loc] = navigator.definitions(base, "api.ts", 4, 16)
    assert (loc.path, loc.line, loc.name) == ("users.ts", 3, "getUser")


def test_references_follow_the_revision(nav):
    navigator, base, head = nav
    new_files = {loc.path for loc in navigator.references(head, "users.ts", 3, 16)}
    old_files = {loc.path for loc in navigator.references(base, "users.ts", 3, 16)}
    assert new_files == {"api.ts", "billing.ts", "reports.ts", "users.ts"}
    assert old_files == new_files | {"legacy.js"}  # legacy.js still calls getUser
    [definition] = [
        loc for loc in navigator.references(head, "users.ts", 3, 16) if loc.is_definition
    ]
    assert definition.path == "users.ts"


def test_builtin_resolves_to_a_lib_file(nav):
    navigator, _, head = nav
    # reports.ts line 4: "  const users = ids.map((i) => fetchUser(i));"
    [loc] = navigator.definitions(head, "reports.ts", 4, 22)
    assert loc.kind == LIBRARY and loc.path.endswith(".d.ts") and loc.name == "map"
    assert "map<U>" in navigator.library_source(loc.path)


def test_describe_environment(nav):
    navigator, _, _ = nav
    assert navigator.describe_environment("api.ts").startswith("TypeScript ")


def test_missing_file_is_an_error(nav):
    navigator, base, _ = nav
    with pytest.raises(NavigationError):
        navigator.definitions(base, "nope.ts", 1, 0)


def test_working_tree(nav, ts_rename_repo):
    navigator, _, _ = nav
    assert where(navigator.definitions(None, "billing.ts", 4, 16)) == [(REPO, "users.ts", 3)]
