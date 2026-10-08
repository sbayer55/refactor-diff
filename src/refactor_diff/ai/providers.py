"""The model providers behind the Ask menu, all with one tiny interface: stream text deltas
for a system prompt and a list of chat turns, and test the connection.

Claude goes through the official SDK; Ollama and any OpenAI-compatible endpoint (a Cursor
proxy, OpenRouter, LM Studio, vLLM, ...) speak JSON over httpx2.
"""

from __future__ import annotations

import json
import time
from collections.abc import Iterator
from typing import Protocol

import httpx2 as httpx

CONNECT_TIMEOUT = 10.0
READ_TIMEOUT = 120.0
MAX_TOKENS = 16000

CLAUDE_MODELS = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
]


class ProviderError(Exception):
    """A message fit to show the user."""


class Provider(Protocol):
    name: str
    model: str

    def stream(self, system: str, messages: list[dict]) -> Iterator[str]: ...

    def test(self) -> dict: ...


def build(settings: dict) -> Provider:
    """The active provider from the settings, or ``ProviderError`` when it isn't set up."""
    ai = settings["ai"]
    name = ai["provider"]
    cfg = ai["providers"][name]
    if name == "claude":
        if not cfg.get("api_key"):
            raise ProviderError("Claude needs an API key. Open Settings to add one.")
        return ClaudeProvider(
            cfg["api_key"], cfg.get("model") or "claude-opus-5-5", cfg.get("base_url") or None
        )
    if name == "openai":
        if not cfg.get("base_url") or not cfg.get("model"):
            raise ProviderError(
                "The OpenAI-compatible endpoint needs a base URL and a model. Open Settings."
            )
        return OpenAIProvider(cfg["base_url"], cfg.get("api_key", ""), cfg["model"])
    if name == "ollama":
        if not cfg.get("host") or not cfg.get("model"):
            raise ProviderError("Ollama needs a host and a model. Open Settings.")
        return OllamaProvider(cfg["host"], cfg["model"], int(cfg.get("num_ctx") or 32768))
    raise ProviderError(f"Unknown provider {name!r}.")


def _timed(fn) -> dict:
    t0 = time.monotonic()
    try:
        extra = fn() or {}
    except ProviderError as e:
        return {"ok": False, "error": str(e), "latency_ms": None, "models": []}
    return (
        {"ok": True, "error": None, "latency_ms": int((time.monotonic() - t0) * 1000)}
        | {"models": []}
        | extra
    )


# --- Claude -----------------------------------------------------------------------------------


class ClaudeProvider:
    name = "claude"

    def __init__(self, api_key: str, model: str, base_url: str | None = None, client=None):
        import anthropic

        self.model = model
        self._anthropic = anthropic
        self.client = client or anthropic.Anthropic(
            api_key=api_key, base_url=base_url, timeout=READ_TIMEOUT, max_retries=1
        )

    def _translate(self, e: Exception) -> ProviderError:
        a = self._anthropic
        if isinstance(e, a.AuthenticationError):
            return ProviderError("Claude rejected the API key. Check it in Settings.")
        if isinstance(e, a.PermissionDeniedError):
            return ProviderError("The Claude API key doesn't have access to this model.")
        if isinstance(e, a.NotFoundError):
            return ProviderError(
                f"Claude doesn't know the model {self.model!r}. Check it in Settings."
            )
        if isinstance(e, a.RateLimitError):
            return ProviderError("Claude is rate limiting requests; try again in a moment.")
        if isinstance(e, a.APIStatusError):
            return ProviderError(f"Claude returned an error ({e.status_code}): {e.message}")
        if isinstance(e, a.APIConnectionError):
            return ProviderError("Couldn't reach the Claude API. Check your network or base URL.")
        return ProviderError(f"Claude request failed: {e}")

    def stream(self, system: str, messages: list[dict]) -> Iterator[str]:
        try:
            with self.client.messages.stream(
                model=self.model,
                max_tokens=MAX_TOKENS,
                system=system,
                messages=messages,
                output_config={"effort": "medium"},
            ) as stream:
                yield from stream.text_stream
                final = stream.get_final_message()
        except Exception as e:  # the SDK's typed errors are translated below
            raise self._translate(e) from e
        if final.stop_reason == "refusal":
            raise ProviderError("Claude declined to answer this request.")
        if final.stop_reason == "max_tokens":
            yield "\n\n*(answer cut off: the response hit the length limit)*"

    def test(self) -> dict:
        def probe():
            try:
                self.client.messages.create(
                    model=self.model,
                    max_tokens=8,
                    messages=[{"role": "user", "content": "Reply with the single word ok."}],
                )
            except Exception as e:
                raise self._translate(e) from e
            return {"models": CLAUDE_MODELS}

        return _timed(probe)


# --- JSON over HTTP ---------------------------------------------------------------------------


def _http(headers: dict | None = None) -> httpx.Client:
    return httpx.Client(
        timeout=httpx.Timeout(READ_TIMEOUT, connect=CONNECT_TIMEOUT), headers=headers or {}
    )


def _describe(e: Exception, what: str) -> ProviderError:
    if isinstance(e, httpx.ConnectError):
        return ProviderError(f"Couldn't connect to {what}. Is it running?")
    if isinstance(e, httpx.TimeoutException):
        return ProviderError(f"{what} didn't respond in time.")
    if isinstance(e, httpx.HTTPStatusError):
        body = e.response.text[:200].strip()
        return ProviderError(
            f"{what} returned HTTP {e.response.status_code}: {body or 'no detail'}"
        )
    if isinstance(e, httpx.HTTPError):
        return ProviderError(f"Request to {what} failed: {e}")
    return ProviderError(f"Request to {what} failed: {e}")


class OllamaProvider:
    name = "ollama"

    def __init__(
        self, host: str, model: str, num_ctx: int = 32768, client: httpx.Client | None = None
    ):
        self.host = host.rstrip("/")
        self.model = model
        self.num_ctx = num_ctx
        self.client = client or _http()

    def stream(self, system: str, messages: list[dict]) -> Iterator[str]:
        body = {
            "model": self.model,
            "messages": [{"role": "system", "content": system}, *messages],
            "stream": True,
            "options": {"num_ctx": self.num_ctx},
        }
        try:
            with self.client.stream("POST", f"{self.host}/api/chat", json=body) as res:
                if res.status_code >= 400:
                    res.read()
                    raise httpx.HTTPStatusError("error", request=res.request, response=res)
                for line in res.iter_lines():
                    if not line.strip():
                        continue
                    chunk = json.loads(line)
                    if chunk.get("error"):
                        raise ProviderError(f"Ollama: {chunk['error']}")
                    text = (chunk.get("message") or {}).get("content") or ""
                    if text:
                        yield text
                    if chunk.get("done"):
                        break
        except ProviderError:
            raise
        except Exception as e:
            raise _describe(e, "Ollama") from e

    def test(self) -> dict:
        def probe():
            try:
                res = self.client.get(f"{self.host}/api/tags")
                res.raise_for_status()
                models = [m.get("name", "") for m in res.json().get("models", [])]
            except Exception as e:
                raise _describe(e, "Ollama") from e
            if self.model and self.model not in models:
                raise ProviderError(
                    f"Ollama is running but has no model {self.model!r}. "
                    f"Pull it with `ollama pull {self.model}`."
                )
            return {"models": models}

        return _timed(probe)


class OpenAIProvider:
    name = "openai"

    def __init__(self, base_url: str, api_key: str, model: str, client: httpx.Client | None = None):
        self.base_url = base_url.rstrip("/")
        self.model = model
        headers = {"Authorization": f"Bearer {api_key}"} if api_key else {}
        self.client = client or _http(headers)

    def stream(self, system: str, messages: list[dict]) -> Iterator[str]:
        body = {
            "model": self.model,
            "messages": [{"role": "system", "content": system}, *messages],
            "stream": True,
        }
        try:
            with self.client.stream("POST", f"{self.base_url}/chat/completions", json=body) as res:
                if res.status_code >= 400:
                    res.read()
                    raise httpx.HTTPStatusError("error", request=res.request, response=res)
                for line in res.iter_lines():
                    if not line.startswith("data:"):
                        continue
                    payload = line[5:].strip()
                    if payload == "[DONE]":
                        break
                    chunk = json.loads(payload)
                    if chunk.get("error"):
                        raise ProviderError(f"Endpoint error: {chunk['error']}")
                    choices = chunk.get("choices") or []
                    if not choices:
                        continue
                    text = (choices[0].get("delta") or {}).get("content") or ""
                    if text:
                        yield text
        except ProviderError:
            raise
        except Exception as e:
            raise _describe(e, "the endpoint") from e

    def test(self) -> dict:
        def probe():
            try:
                res = self.client.get(f"{self.base_url}/models")
                if res.status_code == 200:
                    data = res.json().get("data", [])
                    return {"models": [m.get("id", "") for m in data if isinstance(m, dict)]}
                res = self.client.post(
                    f"{self.base_url}/chat/completions",
                    json={
                        "model": self.model,
                        "max_tokens": 8,
                        "messages": [{"role": "user", "content": "Reply with the single word ok."}],
                    },
                )
                res.raise_for_status()
            except Exception as e:
                raise _describe(e, "the endpoint") from e
            return {"models": []}

        return _timed(probe)
