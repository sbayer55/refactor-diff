import json
from types import SimpleNamespace

import httpx2 as httpx
import pytest

from refactor_diff import settings
from refactor_diff.ai import providers
from refactor_diff.ai.providers import (
    ClaudeProvider,
    OllamaProvider,
    OpenAIProvider,
    ProviderError,
    build,
)


def mock_client(handler, headers=None):
    return httpx.Client(transport=httpx.MockTransport(handler), headers=headers or {})


# --- build ---------------------------------------------------------------------------------------


def test_build_requires_configuration(tmp_path):
    data = settings.load(tmp_path)
    with pytest.raises(ProviderError, match="API key"):
        build(data)
    data["ai"]["provider"] = "ollama"
    with pytest.raises(ProviderError, match="Ollama needs"):
        build(data)
    data["ai"]["providers"]["ollama"]["model"] = "qwen3-coder:30b"
    p = build(data)
    assert isinstance(p, OllamaProvider) and p.model == "qwen3-coder:30b" and p.num_ctx == 32768
    data["ai"]["provider"] = "openai"
    with pytest.raises(ProviderError, match="base URL"):
        build(data)
    data["ai"]["providers"]["openai"] |= {"base_url": "http://x/v1/", "model": "m"}
    p = build(data)
    assert isinstance(p, OpenAIProvider) and p.base_url == "http://x/v1"
    data["ai"]["provider"] = "claude"
    data["ai"]["providers"]["claude"]["api_key"] = "k"
    assert isinstance(build(data), ClaudeProvider)


# --- Ollama --------------------------------------------------------------------------------------


def test_ollama_streams_ndjson():
    seen = {}

    def handler(request):
        seen["body"] = json.loads(request.content)
        seen["url"] = str(request.url)
        lines = [
            {"message": {"role": "assistant", "content": "Hel"}, "done": False},
            {"message": {"role": "assistant", "content": "lo"}, "done": False},
            {"message": {"role": "assistant", "content": ""}, "done": True, "done_reason": "stop"},
        ]
        return httpx.Response(200, content="\n".join(json.dumps(x) for x in lines) + "\n")

    p = OllamaProvider("http://ollama.test/", "m", 4096, client=mock_client(handler))
    assert "".join(p.stream("sys", [{"role": "user", "content": "hi"}])) == "Hello"
    assert seen["url"] == "http://ollama.test/api/chat"
    assert seen["body"]["messages"][0] == {"role": "system", "content": "sys"}
    assert seen["body"]["stream"] is True and seen["body"]["options"] == {"num_ctx": 4096}


def test_ollama_errors_are_readable():
    def refuse(request):
        raise httpx.ConnectError("refused", request=request)

    p = OllamaProvider("http://ollama.test", "m", client=mock_client(refuse))
    with pytest.raises(ProviderError, match="Couldn't connect to Ollama"):
        list(p.stream("s", []))
    assert p.test()["ok"] is False and "Couldn't connect" in p.test()["error"]

    def missing(request):
        return httpx.Response(404, text="model 'm' not found")

    p = OllamaProvider("http://ollama.test", "m", client=mock_client(missing))
    with pytest.raises(ProviderError, match="HTTP 404"):
        list(p.stream("s", []))


def test_ollama_test_lists_models():
    def tags(request):
        return httpx.Response(
            200, json={"models": [{"name": "qwen3-coder:30b"}, {"name": "llama4"}]}
        )

    p = OllamaProvider("http://ollama.test", "qwen3-coder:30b", client=mock_client(tags))
    res = p.test()
    assert res["ok"] and res["models"] == ["qwen3-coder:30b", "llama4"] and res["latency_ms"] >= 0
    p = OllamaProvider("http://ollama.test", "mistral", client=mock_client(tags))
    assert "ollama pull mistral" in p.test()["error"]


# --- OpenAI-compatible ---------------------------------------------------------------------------


def test_openai_streams_sse():
    seen = {}

    def handler(request):
        seen["auth"] = request.headers.get("authorization")
        seen["body"] = json.loads(request.content)
        chunks = [
            {"choices": [{"delta": {"role": "assistant"}}]},
            {"choices": [{"delta": {"content": "Hel"}}]},
            {"choices": [{"delta": {"content": "lo"}}]},
            {"choices": []},
        ]
        body = "".join(f"data: {json.dumps(c)}\n\n" for c in chunks) + "data: [DONE]\n\n"
        return httpx.Response(200, content=body)

    p = OpenAIProvider(
        "http://proxy.test/v1",
        "key",
        "m",
        client=mock_client(handler, {"Authorization": "Bearer key"}),
    )
    assert "".join(p.stream("sys", [{"role": "user", "content": "hi"}])) == "Hello"
    assert seen["auth"] == "Bearer key"
    assert seen["body"]["model"] == "m" and seen["body"]["stream"] is True


def test_openai_test_falls_back_to_a_completion():
    calls = []

    def handler(request):
        calls.append(request.url.path)
        if request.url.path.endswith("/models"):
            return httpx.Response(404)
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}]})

    p = OpenAIProvider("http://proxy.test/v1", "", "m", client=mock_client(handler))
    assert p.test()["ok"] is True
    assert calls == ["/v1/models", "/v1/chat/completions"]

    def unauthorized(request):
        return httpx.Response(401, text='{"error":"bad key"}')

    p = OpenAIProvider("http://proxy.test/v1", "", "m", client=mock_client(unauthorized))
    assert "HTTP 401" in p.test()["error"]


# --- Claude --------------------------------------------------------------------------------------


class FakeStream:
    def __init__(self, parts, stop_reason="end_turn"):
        self.text_stream = iter(parts)
        self.stop_reason = stop_reason

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def get_final_message(self):
        return SimpleNamespace(stop_reason=self.stop_reason)


def fake_client(parts, stop_reason="end_turn", calls=None):
    def stream(**kwargs):
        if calls is not None:
            calls.append(kwargs)
        return FakeStream(parts, stop_reason)

    return SimpleNamespace(messages=SimpleNamespace(stream=stream, create=lambda **kw: None))


def test_claude_streams_and_flags_cut_off_answers():
    calls = []
    p = ClaudeProvider("k", "claude-opus-5-5", client=fake_client(["a", "b"], calls=calls))
    assert "".join(p.stream("sys", [{"role": "user", "content": "q"}])) == "ab"
    assert calls[0]["system"] == "sys" and calls[0]["model"] == "claude-opus-5-5"
    assert calls[0]["max_tokens"] == providers.MAX_TOKENS
    p = ClaudeProvider("k", "m", client=fake_client(["a"], "max_tokens"))
    assert "".join(p.stream("s", [])).endswith("length limit)*")
    p = ClaudeProvider("k", "m", client=fake_client([], "refusal"))
    with pytest.raises(ProviderError, match="declined"):
        list(p.stream("s", []))


def test_claude_translates_sdk_errors():
    import anthropic

    def boom(**kwargs):
        raise anthropic.AuthenticationError(
            "bad",
            response=httpx.Response(401, request=httpx.Request("POST", "http://x")),
            body=None,
        )

    client = SimpleNamespace(messages=SimpleNamespace(stream=boom, create=boom))
    p = ClaudeProvider("k", "m", client=client)
    with pytest.raises(ProviderError, match="rejected the API key"):
        list(p.stream("s", []))
    res = p.test()
    assert res["ok"] is False and "rejected the API key" in res["error"]
