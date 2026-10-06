import json

from conftest import commit, git

from refactor_diff.state import ReviewStore, config_dir


def test_config_dir_honours_xdg(isolated_config):
    assert config_dir() == isolated_config / "refactor-diff"


def test_marks_round_trip_and_are_pruned(tmp_path):
    store = ReviewStore(tmp_path / "repo", tmp_path / "cfg")
    store.mark("refs:main:f", groups={"add": ["g1", "g2"]}, hunks={"add": ["h1", "h2"]})
    store.mark("refs:main:f", groups={"remove": ["g1"]})
    entry = store.load("refs:main:f")
    assert entry["groups"] == ["g2"] and entry["hunks"] == ["h1", "h2"]
    # Written atomically as JSON; a second store on the same repo sees it.
    assert json.loads(store.path.read_text())["comparisons"]["refs:main:f"]["groups"] == ["g2"]
    assert ReviewStore(tmp_path / "repo", tmp_path / "cfg").load("refs:main:f")["hunks"] == [
        "h1",
        "h2",
    ]
    # A hunk that no longer exists loses its mark; a group mark is kept.
    store.record_analysis("refs:main:f", "sha1", {"h1": "a.py"})
    assert store.review("refs:main:f", {"h1": "a.py"}) == {
        "identity": "refs:main:f",
        "groups": ["g2"],
        "hunks": ["h1"],
        "delta": {"prev_head": None, "new": [], "changed_reviewed": []},
    }


def test_delta_against_the_previous_head(tmp_path):
    store = ReviewStore(tmp_path / "repo", tmp_path / "cfg")
    ident = "pr:7"
    store.record_analysis(ident, "aaa", {"h1": "a.py", "h2": "a.py", "h3": "b.py"})
    store.mark(ident, hunks={"add": ["h1", "h2"]})
    # New commit: h2 changed (new fingerprint h2b), h4 appeared, h1/h3 unchanged.
    delta = store.record_analysis(
        ident, "bbb", {"h1": "a.py", "h2b": "a.py", "h3": "b.py", "h4": "c.py"}
    )
    assert delta == {
        "prev_head": "aaa",
        "new": ["h2b", "h4"],
        "changed_reviewed": [{"fingerprint": "h2", "path": "a.py"}],
    }
    assert store.load(ident)["hunks"] == ["h1"]
    # Re-analyzing the same head keeps reporting the same delta.
    again = store.record_analysis(
        ident, "bbb", {"h1": "a.py", "h2b": "a.py", "h3": "b.py", "h4": "c.py"}
    )
    assert again == delta


def test_corrupt_state_file_is_ignored(tmp_path):
    store = ReviewStore(tmp_path / "repo", tmp_path / "cfg")
    store.path.parent.mkdir(parents=True)
    store.path.write_text("{not json")
    assert store.load("x")["groups"] == []
    store.mark("x", groups={"add": ["g"]})
    assert store.load("x")["groups"] == ["g"]


def test_review_marks_survive_new_commits_via_the_api(tmp_path, isolated_config, serve):
    repo = tmp_path / "repo"
    pad = "\n".join(f"line_{i} = {i}" for i in range(10)) + "\n"
    base = pad + "\n\ndef f():\n    return 1\n\n\ndef g():\n    return 2\n\n\n" + pad
    commit(repo, {"a.py": base}, "before")
    git(repo, "checkout", "-qb", "feature")
    commit(repo, {"a.py": base.replace("return 2", "return 3")}, "after")

    client = serve(repo)
    body = {"base": "main", "head": "feature"}
    data = client.post("/api/analyze", json=body).json()
    [hunk] = data["hunks"].values()
    assert data["review"] == {
        "identity": "refs:main:feature",
        "groups": [],
        "hunks": [],
        "delta": {"prev_head": None, "new": [], "changed_reviewed": []},
    }
    res = client.post(
        f"/api/report/{data['id']}/review", json={"hunks": {"add": [hunk["fingerprint"]]}}
    )
    assert res.json()["hunks"] == [hunk["fingerprint"]]

    # A new commit that only shifts the hunk down keeps the mark; its new sibling is "new".
    commit(repo, {"a.py": "import os\n\n" + base.replace("return 2", "return 3") + "\n\nX = 1\n"})
    data2 = client.post("/api/analyze", json=body).json()
    fps = {h["fingerprint"] for h in data2["hunks"].values()}
    assert hunk["fingerprint"] in fps and len(fps) == 3
    assert data2["review"]["hunks"] == [hunk["fingerprint"]]
    assert set(data2["review"]["delta"]["new"]) == fps - {hunk["fingerprint"]}
    assert data2["review"]["delta"]["prev_head"] == data["source"]["head_sha"]

    # Editing the reviewed hunk itself drops the mark and reports it as changed.
    commit(repo, {"a.py": "import os\n\n" + base.replace("return 2", "return 4") + "\n\nX = 1\n"})
    data3 = client.post("/api/analyze", json=body).json()
    assert data3["review"]["hunks"] == []
    assert [c["fingerprint"] for c in data3["review"]["delta"]["changed_reviewed"]] == [
        hunk["fingerprint"]
    ]
    # Another server process reads the same state.
    assert serve(repo).get(f"/api/report/{data3['id']}/review").status_code == 404
    assert (isolated_config / "refactor-diff" / "reviews").exists()


def test_bad_review_body_is_400(rename_repo, serve):
    client = serve(rename_repo)
    data = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    res = client.post(f"/api/report/{data['id']}/review", json={"hunks": ["x"]})
    assert res.status_code == 400
