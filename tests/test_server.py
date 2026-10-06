import json
import subprocess
from unittest import mock

from refactor_diff import sources


def test_index_and_static(rename_repo, serve):
    client = serve(rename_repo)
    assert "refactor-diff" in client.get("/").text
    assert client.get("/static/app.js").status_code == 200


def test_sources_lists_branches(rename_repo, serve):
    with mock.patch.object(sources, "gh_available", return_value=False):
        data = serve(rename_repo).get("/api/sources").json()
    assert {"main", "feature"} <= set(data["branches"])
    assert data["default_base"] == "main" and data["current"] == "feature"


def test_analyze_and_fetch_report(rename_repo, serve):
    client = serve(rename_repo)
    data = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    assert data["stats"]["residual_units"] == 1
    assert client.get(f"/api/report/{data['id']}").json()["id"] == data["id"]


def test_analyze_bad_ref_is_400(rename_repo, serve):
    res = serve(rename_repo).post("/api/analyze", json={"base": "nope"})
    assert res.status_code == 400 and "nope" in res.json()["error"]


def test_pr_source_uses_gh(rename_repo, serve):
    git = lambda *a: subprocess.run(  # noqa: E731
        ["git", "-C", str(rename_repo), *a], capture_output=True, text=True
    ).stdout.strip()
    pr_info = {
        "number": 7,
        "title": "Rename get_user",
        "url": "https://example.test/pr/7",
        "baseRefName": "main",
        "headRefName": "feature",
        "baseRefOid": git("rev-parse", "main"),
        "headRefOid": git("rev-parse", "feature"),
    }
    real_run = subprocess.run

    def fake_run(cmd, *args, **kwargs):
        if cmd[0] == "gh":
            return subprocess.CompletedProcess(cmd, 0, json.dumps(pr_info).encode(), b"")
        return real_run(cmd, *args, **kwargs)

    with (
        mock.patch.object(sources, "gh_available", return_value=True),
        mock.patch.object(sources.subprocess, "run", side_effect=fake_run),
    ):
        data = serve(rename_repo).post("/api/analyze", json={"pr": 7}).json()
    assert data["source"]["pr"]["number"] == 7
    assert data["source"]["label"] == "#7 Rename get_user"
    assert data["stats"]["residual_units"] == 1


def test_file_diff_endpoint(rename_repo, serve):
    client = serve(rename_repo)
    report = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    data = client.get(f"/api/report/{report['id']}/file", params={"path": "api.py"}).json()

    old = [ln["text"] for ln in data["lines"] if ln["t"] != "+"]
    new = [ln["text"] for ln in data["lines"] if ln["t"] != "-"]
    main_api = subprocess.run(
        ["git", "-C", str(rename_repo), "show", "main:api.py"], capture_output=True, text=True
    ).stdout
    assert old == main_api.splitlines()
    assert new == (rename_repo / "api.py").read_text().splitlines()
    assert len(old) == data["old_lines"] and len(new) == data["new_lines"]

    # Changed lines point at the report's units and carry their highlights.
    changed = [ln for ln in data["lines"] if ln["t"] != " "]
    assert changed and all(ln["unit"] in report["units"] for ln in changed)
    first_add = next(ln for ln in changed if ln["t"] == "+")
    assert first_add["text"] == "from users import fetch_user" and first_add["hl"] == [[18, 28]]


def test_file_diff_unknown_path_is_404(rename_repo, serve):
    client = serve(rename_repo)
    report = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    res = client.get(f"/api/report/{report['id']}/file", params={"path": "nope.py"})
    assert res.status_code == 404
