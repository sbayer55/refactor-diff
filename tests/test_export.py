import json
import subprocess
from unittest import mock

from conftest import commit, git
from starlette.testclient import TestClient

from refactor_diff import sources
from refactor_diff.engine import analyze
from refactor_diff.export import markdown_summary
from refactor_diff.web.server import create_app


def test_markdown_summary(rename_repo):
    report = analyze(rename_repo, "main", "feature")
    [hunk] = report.residual_hunk_ids
    rename = next(g for g in report.groups if g.label == "get_user → fetch_user")
    md = markdown_summary(
        report, {"groups": [rename.id], "hunks": [report.hunks[hunk].fingerprint]}
    )
    assert md.startswith("# refactor-diff: main...feature\n")
    assert "of changed lines collapsed · **1** change to review" in md
    assert "| ✓ | rename | `get_user → fetch_user` | 8 | 4 |" in md
    assert "|  | retype | `int → str` | 4 | 3 |" in md
    assert '- [x] `api.py:6` — `if user is None or not user.get("active"):`' in md
    assert "- **missed-rename**: get_user was renamed to fetch_user" in md
    assert "(`legacy.py:1`, `legacy.py:5`)" in md


def test_summary_route_and_commits(tmp_path):
    repo = tmp_path / "repo"
    commit(repo, {"a.py": "x = 1\n"}, "before")
    git(repo, "checkout", "-qb", "feature")
    commit(repo, {"a.py": "x = 2\n"}, "first change")
    commit(repo, {"b.py": "y = 1\ny2 = 2\n"}, "second change")
    client = TestClient(create_app(repo))
    data = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()

    md = client.get(f"/api/report/{data['id']}/summary.md")
    assert md.headers["content-type"].startswith("text/markdown")
    assert "- [ ] `a.py:1` — `x = 2`" in md.text

    commits = client.get(f"/api/report/{data['id']}/commits").json()["commits"]
    assert [c["subject"] for c in commits] == ["first change", "second change"]
    assert (
        commits[1]["files"] == 1 and commits[1]["insertions"] == 2 and commits[1]["deletions"] == 0
    )
    assert commits[0]["sha"] == git(repo, "rev-parse", "feature~1")

    # One commit on its own analyzes against its parent.
    one = client.post(
        "/api/analyze", json={"base": commits[1]["sha"] + "^", "head": commits[1]["sha"]}
    ).json()
    assert [f["path"] for f in one["files"]] == ["b.py"]
    assert one["source"]["identity"] == f"refs:{commits[1]['sha']}^:{commits[1]['sha']}"

    # The working tree has no commit list.
    wt = client.post("/api/analyze", json={"base": "main", "head": ":worktree:"}).json()
    assert client.get(f"/api/report/{wt['id']}/commits").json() == {"commits": []}


def test_pr_comment_routes(rename_repo):
    sha = lambda ref: git(rename_repo, "rev-parse", ref)  # noqa: E731
    pr_info = {
        "number": 7,
        "title": "Rename get_user",
        "url": "https://example.test/pr/7",
        "baseRefName": "main",
        "headRefName": "feature",
        "baseRefOid": sha("main"),
        "headRefOid": sha("feature"),
    }
    calls = []
    real_run = subprocess.run

    def fake_run(cmd, *args, **kwargs):
        if cmd[0] != "gh":
            return real_run(cmd, *args, **kwargs)
        calls.append((cmd, kwargs.get("input")))
        if cmd[1] == "pr" and cmd[2] == "view":
            return subprocess.CompletedProcess(cmd, 0, json.dumps(pr_info).encode(), b"")
        if cmd[1] == "pr" and cmd[2] == "comment":
            return subprocess.CompletedProcess(cmd, 0, b"https://example.test/pr/7#c1\n", b"")
        if cmd[1] == "api":
            return subprocess.CompletedProcess(
                cmd, 0, json.dumps({"html_url": "https://example.test/pr/7#r1"}).encode(), b""
            )
        raise AssertionError(cmd)

    with (
        mock.patch.object(sources, "gh_available", return_value=True),
        mock.patch.object(sources.subprocess, "run", side_effect=fake_run),
    ):
        client = TestClient(create_app(rename_repo))
        data = client.post("/api/analyze", json={"pr": 7}).json()
        rid = data["id"]
        res = client.post(f"/api/report/{rid}/pr/comment", json={"body": "## Summary\nhello"})
        assert res.json() == {"url": "https://example.test/pr/7#c1"}
        [hunk_id] = data["residual_hunk_ids"]
        res = client.post(
            f"/api/report/{rid}/pr/review-comment", json={"body": "why?", "hunk_id": hunk_id}
        )
        assert res.json() == {
            "url": "https://example.test/pr/7#r1",
            "path": "api.py",
            "line": 6,
            "side": "RIGHT",
        }
        assert client.post(f"/api/report/{rid}/pr/comment", json={"body": "  "}).status_code == 400

    comment_cmd, comment_body = calls[1]
    assert (
        comment_cmd[:5] == ["gh", "pr", "comment", "7", "--body-file"]
        and comment_body == b"## Summary\nhello"
    )
    api_cmd, api_body = calls[2]
    assert api_cmd[1:5] == ["api", "--method", "POST", "repos/{owner}/{repo}/pulls/7/comments"]
    assert json.loads(api_body) == {
        "body": "why?",
        "commit_id": sha("feature"),
        "path": "api.py",
        "line": 6,
        "side": "RIGHT",
    }


def test_pr_routes_refuse_non_pr_reports(rename_repo):
    client = TestClient(create_app(rename_repo))
    data = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    res = client.post(f"/api/report/{data['id']}/pr/comment", json={"body": "x"})
    assert res.status_code == 400 and "pull request" in res.json()["error"]
