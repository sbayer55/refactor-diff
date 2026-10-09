"""Review UI preferences (layout, highlighting, panes, per-repository filters).

The UI used to keep these in the browser's localStorage, which is scoped to the page's origin:
``http://127.0.0.1:<port>``. The port is random for the CLI and only usually the same for the
desktop app, so preferences kept disappearing. They now live in ``ui.json`` next to the review
marks (see ``state.config_dir``) as a flat ``{key: string}`` map, with localStorage as a cache.
"""

from __future__ import annotations

import json
import os
import threading
from pathlib import Path

from refactor_diff.state import config_dir

PREFIX = "refactor-diff:"
MAX_BYTES = 256 * 1024

_lock = threading.Lock()


def prefs_path(root: Path | None = None) -> Path:
    return (root or config_dir()) / "ui.json"


def load(root: Path | None = None) -> dict[str, str]:
    """The stored preferences (never raises on a missing or broken file)."""
    try:
        data = json.loads(prefs_path(root).read_text())
    except (OSError, ValueError):
        return {}
    if not isinstance(data, dict):
        return {}
    return {k: v for k, v in data.items() if _valid(k, v)}


def update(changes: dict, root: Path | None = None) -> dict[str, str]:
    """Apply ``changes`` (a ``None`` value deletes the key) and return the result.

    Raises ``ValueError`` for keys outside the UI's namespace, non-string values, or a result
    that would grow past ``MAX_BYTES``.
    """
    if not isinstance(changes, dict):
        raise ValueError("Expected an object of preference changes.")
    for key, value in changes.items():
        if not isinstance(key, str) or not key.startswith(PREFIX):
            raise ValueError(f"Preference keys must start with {PREFIX!r}.")
        if value is not None and not isinstance(value, str):
            raise ValueError(f"{key} must be a string or null.")
    path = prefs_path(root)
    with _lock:
        data = load(root)
        for key, value in changes.items():
            if value is None:
                data.pop(key, None)
            else:
                data[key] = value
        text = json.dumps(data, indent=1, sort_keys=True)
        if len(text.encode()) > MAX_BYTES:
            raise ValueError("Too many preferences stored.")
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(text)
        os.replace(tmp, path)
    return data


def _valid(key: object, value: object) -> bool:
    return isinstance(key, str) and key.startswith(PREFIX) and isinstance(value, str)
