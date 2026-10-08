"""User settings that outlive the server: which AI provider to use and how to reach it.

Stored as ``settings.json`` in the same directory as the review marks (see ``state.config_dir``),
written with mode 0600 because it holds API keys. The web API never returns a key: ``public_view``
masks them, and a masked value sent back on save means "keep the stored key".
"""

from __future__ import annotations

import copy
import json
import os
import threading
from pathlib import Path

from refactor_diff.state import config_dir

PROVIDERS = ("claude", "openai", "ollama")

DEFAULTS: dict = {
    "ai": {
        "provider": "claude",
        "providers": {
            "claude": {"api_key": "", "model": "claude-opus-5-5", "base_url": ""},
            "openai": {"base_url": "", "api_key": "", "model": ""},
            "ollama": {"host": "http://127.0.0.1:11434", "model": "", "num_ctx": 32768},
        },
        "context": {"function": True, "pr": True, "references": False},
    }
}

SECRET_KEYS = ("api_key",)
MASK = "••••"

_lock = threading.Lock()


def settings_path(root: Path | None = None) -> Path:
    return (root or config_dir()) / "settings.json"


def _merge(base: dict, over: dict) -> dict:
    out = copy.deepcopy(base)
    for k, v in over.items():
        if isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = _merge(out[k], v)
        else:
            out[k] = v
    return out


def load(root: Path | None = None) -> dict:
    """The settings with every default filled in (never raises on a missing or broken file)."""
    try:
        data = json.loads(settings_path(root).read_text())
    except (OSError, ValueError):
        data = {}
    if not isinstance(data, dict):
        data = {}
    merged = _merge(DEFAULTS, data)
    if merged["ai"]["provider"] not in PROVIDERS:
        merged["ai"]["provider"] = DEFAULTS["ai"]["provider"]
    return merged


def save(data: dict, root: Path | None = None) -> dict:
    """Write ``data`` (merged over the defaults) atomically with mode 0600; returns it."""
    merged = _merge(DEFAULTS, data)
    path = settings_path(root)
    with _lock:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(merged, indent=1, sort_keys=True))
        os.chmod(tmp, 0o600)
        os.replace(tmp, path)
    return merged


def mask(secret: str) -> str:
    return f"{MASK}{secret[-4:]}" if secret else ""


def is_masked(value: object) -> bool:
    return isinstance(value, str) and value.startswith(MASK)


def public_view(data: dict) -> dict:
    """``data`` with secrets masked and a ``configured`` flag per provider."""
    out = copy.deepcopy(data)
    for name, cfg in out["ai"]["providers"].items():
        for key in SECRET_KEYS:
            if key in cfg:
                cfg[key] = mask(cfg[key])
        cfg["configured"] = configured(data, name)
    ai = out["ai"]
    active = ai["provider"]
    ai["active"] = {
        "provider": active,
        "model": data["ai"]["providers"][active].get("model", ""),
        "configured": configured(data, active),
    }
    return out


def configured(data: dict, provider: str) -> bool:
    cfg = data["ai"]["providers"].get(provider, {})
    if provider == "claude":
        return bool(cfg.get("api_key"))
    if provider == "openai":
        return bool(cfg.get("base_url")) and bool(cfg.get("model"))
    if provider == "ollama":
        return bool(cfg.get("host")) and bool(cfg.get("model"))
    return False


def apply_update(current: dict, update: dict) -> dict:
    """``current`` with the ``ai`` section of ``update`` applied; masked secrets keep the stored
    value, so the UI can round-trip the public view."""
    ai = update.get("ai") if isinstance(update, dict) else None
    if not isinstance(ai, dict):
        raise ValueError("Expected an object with an 'ai' section.")
    merged = copy.deepcopy(current)
    if "provider" in ai:
        if ai["provider"] not in PROVIDERS:
            raise ValueError(f"Unknown provider {ai['provider']!r}.")
        merged["ai"]["provider"] = ai["provider"]
    for name, cfg in (ai.get("providers") or {}).items():
        if name not in PROVIDERS or not isinstance(cfg, dict):
            continue
        target = merged["ai"]["providers"][name]
        for key, value in cfg.items():
            if key == "configured":
                continue
            if key in SECRET_KEYS and is_masked(value):
                continue
            if key == "num_ctx":
                try:
                    value = max(1024, int(value))
                except (TypeError, ValueError) as e:
                    raise ValueError("Context window must be a number.") from e
            elif not isinstance(value, str):
                raise ValueError(f"{name}.{key} must be text.")
            else:
                value = value.strip()
            target[key] = value
    ctx = ai.get("context")
    if isinstance(ctx, dict):
        for key in merged["ai"]["context"]:
            if key in ctx:
                merged["ai"]["context"][key] = bool(ctx[key])
    return merged


def resolve_test_config(current: dict, provider: str, cfg: dict | None) -> dict:
    """Provider config for a connection test: unsaved values from the dialog, with masked
    secrets replaced by the stored ones."""
    if provider not in PROVIDERS:
        raise ValueError(f"Unknown provider {provider!r}.")
    probe = {"ai": {"provider": provider, "providers": {provider: cfg or {}}}}
    return apply_update(current, probe)
