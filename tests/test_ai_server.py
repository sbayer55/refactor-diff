import json
import os
import stat

from refactor_diff import settings
from refactor_diff.ai.providers import ProviderError


class FakeProvider:
    name = "fake"
    model = "fake-1"

    def __init__(self, parts=("Hello ", "`L6` world"), fail=None):
        self.parts = list(parts)
        self.fail = fail
        self.calls = []

    def stream(self, system, messages):
        self.calls.append({"system": system, "messages": messages})
        yield from self.parts
        if self.fail:
            raise ProviderError(self.fail)

    def test(self):
        return {"ok": True, "latency_ms": 1, "models": ["fake-1"], "error": None}


def events(text):
    out = []
    for block in text.strip().split("\n\n"):
        lines = dict(ln.split(": ", 1) for ln in block.splitlines())
        out.append((lines["event"], json.loads(lines["data"])))
    return out


def analyzed(client):
    report = client.post("/api/analyze", json={"base": "main", "head": "feature"}).json()
    hunk = next(h for h in report["hunks"].values() if h["path"] == "api.py")
    return report, hunk


# --- settings ------------------------------------------------------------------------------------


def test_settings_round_trip_masks_keys(rename_repo, serve, isolated_config):
    client = serve(rename_repo)
    view = client.get("/api/settings").json()
    assert view["ai"]["active"] == {
        "provider": "claude",
        "model": "claude-opus-5-5",
        "configured": False,
    }

    res = client.post(
        "/api/settings",
        json={
            "ai": {
                "providers": {
                    "claude": {"api_key": "sk-ant-abcd1234", "model": "claude-sonnet-5-5"}
                }
            }
        },
    )
    view = res.json()
    assert view["ai"]["providers"]["claude"]["api_key"] == "••••1234"
    assert (
        view["ai"]["active"]["configured"] is True
        and view["ai"]["active"]["model"] == "claude-sonnet-5-5"
    )
    path = isolated_config / "refactor-diff" / "settings.json"
    assert stat.S_IMODE(os.stat(path).st_mode) == 0o600
    assert json.loads(path.read_text())["ai"]["providers"]["claude"]["api_key"] == "sk-ant-abcd1234"

    # Sending the masked value back keeps the key; a bad provider is a 400.
    res = client.post("/api/settings", json=view)
    assert res.json()["ai"]["active"]["configured"] is True
    assert client.post("/api/settings", json={"ai": {"provider": "cursor"}}).status_code == 400


def test_settings_test_uses_unsaved_values(rename_repo, serve):
    seen = {}

    def factory(data):
        seen.update(data["ai"])
        return FakeProvider()

    client = serve(rename_repo, provider_factory=factory)
    res = client.post(
        "/api/settings/test",
        json={"provider": "ollama", "config": {"host": "http://h", "model": "m"}},
    ).json()
    assert res["ok"] is True and res["models"] == ["fake-1"]
    assert seen["provider"] == "ollama" and seen["providers"]["ollama"]["model"] == "m"
    # Without a factory override the real build runs and reports the missing key.
    client = serve(rename_repo)
    res = client.post("/api/settings/test", json={"provider": "claude", "config": {}}).json()
    assert res["ok"] is False and "API key" in res["error"]


# --- menu ----------------------------------------------------------------------------------------


def test_ai_menu_describes_the_location(rename_repo, serve):
    client = serve(rename_repo)
    report, hunk = analyzed(client)
    res = client.post(f"/api/report/{report['id']}/ai/menu", json={"hunk_id": hunk["id"]}).json()
    assert res["focus"] == {"path": "api.py", "side": "new", "line": 6, "qualname": "handle"}
    ids = [t["id"] for t in res["tasks"]]
    assert ids[:3] == ["explain", "function", "compare"] and "break" in ids
    assert res["provider"]["provider"] == "claude" and res["provider"]["configured"] is False
    assert res["context"] == {"function": True, "pr": True, "references": False}
    res = client.post(
        f"/api/report/{report['id']}/ai/menu", json={"hunk_id": hunk["id"], "side": "n", "line": 1}
    )
    assert res.json()["focus"]["qualname"] is None
    res = client.post(f"/api/report/{report['id']}/ai/menu", json={"hunk_id": "nope"})
    assert res.status_code == 400
    assert client.post("/api/report/zzz/ai/menu", json={}).status_code == 404


def test_refs_count_needs_a_def(rename_repo, serve):
    client = serve(rename_repo)
    report, hunk = analyzed(client)
    res = client.post(
        f"/api/report/{report['id']}/ai/refs-count",
        json={"hunk_id": hunk["id"], "side": "n", "line": 1},
    )
    assert res.json() == {"count": None}


# --- ask -----------------------------------------------------------------------------------------


def test_ask_streams_events(rename_repo, serve):
    provider = FakeProvider()
    client = serve(rename_repo, provider_factory=lambda s: provider)
    report, hunk = analyzed(client)
    res = client.post(
        f"/api/report/{report['id']}/ai/ask", json={"task": "explain", "hunk_id": hunk["id"]}
    )
    assert res.status_code == 200 and res.headers["content-type"].startswith("text/event-stream")
    evs = events(res.text)
    assert evs[0][0] == "meta"
    assert evs[0][1]["provider"] == "fake" and evs[0][1]["model"] == "fake-1"
    assert evs[0][1]["focus"] == {"path": "api.py", "side": "new", "line": 6}
    assert evs[0][1]["pieces"] == ["function", "hunk", "patterns"]  # no PR on this comparison
    assert [e[1]["text"] for e in evs[1:-1]] == ["Hello ", "`L6` world"]
    assert evs[-1][0] == "done" and evs[-1][1]["elapsed_ms"] >= 0

    call = provider.calls[0]
    assert "Task: Explain what this hunk changes" in call["system"]
    assert call["messages"][0]["role"] == "user"
    user = call["messages"][0]["content"]
    assert "## Hunk (api.py" in user and "## Enclosing def handle — after" in user
    assert "## Commits" not in user  # explain doesn't ask for history


def test_ask_custom_prompt_and_follow_ups(rename_repo, serve):
    provider = FakeProvider()
    client = serve(rename_repo, provider_factory=lambda s: provider)
    report, hunk = analyzed(client)
    body = {
        "task": "custom",
        "hunk_id": hunk["id"],
        "side": "n",
        "line": 6,
        "prompt": "Is the 404 right?",
        "pieces": {"function": False, "pr": True},
        "history": [
            {"role": "assistant", "content": "Earlier answer"},
            {"role": "user", "content": "And now?"},
        ],
    }
    res = client.post(f"/api/report/{report['id']}/ai/ask", json=body)
    assert res.status_code == 200
    msgs = provider.calls[0]["messages"]
    assert msgs[0]["content"].endswith("## Question\nIs the 404 right?")
    assert "## Enclosing" not in msgs[0]["content"] and "## Pull request" in msgs[0]["content"]
    assert msgs[1:] == body["history"]

    bad = dict(body, prompt="   ")
    assert client.post(f"/api/report/{report['id']}/ai/ask", json=bad).status_code == 400
    bad = dict(body, history=[{"role": "user", "content": "x"}])
    assert client.post(f"/api/report/{report['id']}/ai/ask", json=bad).status_code == 400


def test_ask_rejects_what_it_cannot_do(rename_repo, serve, isolated_config):
    client = serve(rename_repo)
    report, hunk = analyzed(client)
    url = f"/api/report/{report['id']}/ai/ask"
    # Not configured: the error tells the UI to open Settings.
    res = client.post(url, json={"task": "explain", "hunk_id": hunk["id"]})
    assert res.status_code == 400 and res.json()["settings"] is True
    settings.save(
        {"ai": {"providers": {"claude": {"api_key": "k"}}}}, isolated_config / "refactor-diff"
    )
    assert client.post(url, json={"task": "nope", "hunk_id": hunk["id"]}).status_code == 400
    # A task that needs a def, asked on the import line.
    res = client.post(url, json={"task": "function", "hunk_id": hunk["id"], "side": "n", "line": 1})
    assert res.status_code == 400 and "isn't inside a function" in res.json()["error"]
    # References need navigation, which this app has (Jedi), so `uses` is allowed here.
    res = client.post(url, json={"task": "uses", "hunk_id": hunk["id"], "side": "n", "line": 99})
    assert res.status_code == 400 and "isn't part of that hunk" in res.json()["error"]


def test_ask_reports_provider_errors_in_the_stream(rename_repo, serve):
    provider = FakeProvider(parts=["partial"], fail="Ollama fell over")
    client = serve(rename_repo, provider_factory=lambda s: provider)
    report, hunk = analyzed(client)
    res = client.post(
        f"/api/report/{report['id']}/ai/ask", json={"task": "review", "hunk_id": hunk["id"]}
    )
    evs = events(res.text)
    assert [e[0] for e in evs] == ["meta", "delta", "error"]
    assert evs[-1][1]["message"] == "Ollama fell over"
    assert "Verdict:" in provider.calls[0]["system"]
