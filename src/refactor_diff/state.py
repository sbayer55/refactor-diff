"""Review state that outlives the server: which patterns and hunks were marked reviewed, and
what the diff looked like last time, so that re-running after new commits can say what is new.

Stored per repository under ``$XDG_CONFIG_HOME/refactor-diff`` (``~/.config/refactor-diff``),
keyed by the *identity* of a comparison - the PR number or the ref names - rather than by
commit, so marks survive a push. Hunks are tracked by content fingerprint (see
``Hunk.fingerprint``), so a reviewed hunk stays reviewed when lines above it shift and stops
being reviewed when its content changes.
"""

from __future__ import annotations

import json
import os
import threading
from datetime import datetime, timezone
from pathlib import Path

from refactor_diff.model import short_hash


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"
    return Path(base) / "refactor-diff"


def _empty() -> dict:
    return {"head_sha": None, "updated": None, "groups": [], "hunks": [], "seen": {}}


class ReviewStore:
    def __init__(self, repo: Path, root: Path | None = None):
        self.repo = repo
        self.path = (root or config_dir()) / "reviews" / f"{short_hash(str(repo.resolve()))}.json"
        self._lock = threading.Lock()

    # --- file access ---

    def _read(self) -> dict:
        try:
            data = json.loads(self.path.read_text())
        except (OSError, ValueError):
            return {"repo": str(self.repo), "comparisons": {}}
        data.setdefault("comparisons", {})
        return data

    def _write(self, data: dict) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(data, indent=1, sort_keys=True))
        os.replace(tmp, self.path)

    # --- api ---

    def load(self, identity: str) -> dict:
        with self._lock:
            return self._read()["comparisons"].get(identity) or _empty()

    def save(self, identity: str, entry: dict) -> None:
        with self._lock:
            data = self._read()
            entry["updated"] = datetime.now(timezone.utc).isoformat(timespec="seconds")
            data["comparisons"][identity] = entry
            self._write(data)

    def mark(self, identity: str, groups: dict | None = None, hunks: dict | None = None) -> dict:
        """Add/remove reviewed marks; each change is ``{"add": [...], "remove": [...]}``."""
        with self._lock:
            data = self._read()
            entry = data["comparisons"].get(identity) or _empty()
            for field, change in (("groups", groups), ("hunks", hunks)):
                if not change:
                    continue
                current = dict.fromkeys(entry.get(field, []))
                for x in change.get("remove", []):
                    current.pop(x, None)
                for x in change.get("add", []):
                    current[x] = None
                entry[field] = list(current)
            entry["updated"] = datetime.now(timezone.utc).isoformat(timespec="seconds")
            data["comparisons"][identity] = entry
            self._write(data)
            return entry

    def record_analysis(self, identity: str, head_sha: str | None, hunks: dict[str, str]) -> dict:
        """Remember the hunks of this analysis (``fingerprint -> path``), prune reviewed marks
        of hunks that no longer exist, and return the delta against the previous head:
        ``{"prev_head", "new": [fingerprints], "changed_reviewed": [fingerprints]}``."""
        head = head_sha or "worktree"
        with self._lock:
            data = self._read()
            entry = data["comparisons"].get(identity) or _empty()
            moved_on = entry["head_sha"] is not None and (
                entry["head_sha"] != head
                or (head == "worktree" and set(entry["seen"]) != set(hunks))
            )
            if moved_on:
                # Keep the last distinct state so re-analyzing the same head still reports it.
                entry["previous"] = {
                    "head_sha": entry["head_sha"],
                    "seen": entry["seen"],
                    "hunks": entry["hunks"],
                }
            entry["head_sha"] = head
            entry["seen"] = hunks
            entry["hunks"] = [fp for fp in entry["hunks"] if fp in hunks]
            entry["updated"] = datetime.now(timezone.utc).isoformat(timespec="seconds")
            data["comparisons"][identity] = entry
            self._write(data)
            return self._delta(entry, hunks)

    @staticmethod
    def _delta(entry: dict, hunks: dict[str, str]) -> dict:
        prev = entry.get("previous")
        if not prev:
            return {"prev_head": None, "new": [], "changed_reviewed": []}
        return {
            "prev_head": prev["head_sha"],
            "new": [fp for fp in hunks if fp not in prev["seen"]],
            "changed_reviewed": [
                {"fingerprint": fp, "path": prev["seen"].get(fp, "")}
                for fp in prev["hunks"]
                if fp not in hunks
            ],
        }

    def review(self, identity: str, hunks: dict[str, str]) -> dict:
        """The state the UI needs for a report whose hunks are ``hunks``."""
        entry = self.load(identity)
        return {
            "identity": identity,
            "groups": entry["groups"],
            "hunks": [fp for fp in entry["hunks"] if fp in hunks],
            "delta": self._delta(entry, hunks),
        }
