"""Serializable report model shared by the engine and the web API.

IDs are derived from content and positions so they stay stable across re-runs of the
same diff; future actions (PR comments, file edits) reference units by these IDs.
"""

from __future__ import annotations

import hashlib
from dataclasses import asdict, dataclass, field

# Signature kinds
FORMATTING = "formatting"
RENAME = "rename"
RETYPE = "retype"
REPLACE = "replace"
DOCS = "docs"  # comment / docstring only

# Kinds that are never logic changes, so they count as mechanical even when they don't repeat.
ALWAYS_MECHANICAL = {FORMATTING, DOCS}


def short_hash(*parts: object) -> str:
    return hashlib.sha1("\x1f".join(map(str, parts)).encode()).hexdigest()[:12]


@dataclass
class FileChange:
    path: str
    old_path: str | None
    status: str  # "A" added, "M" modified, "D" deleted, "R" renamed
    old_text: str
    new_text: str


@dataclass(frozen=True)
class Signature:
    """A normalized description of one mechanical edit.

    ``key`` decides grouping; ``old``/``new`` are display text from the first occurrence.
    """

    kind: str
    key: str
    old: str
    new: str
    detail: str = ""  # e.g. "call", "attribute", "param user_id"

    @property
    def label(self) -> str:
        if self.kind == FORMATTING:
            return "Whitespace / layout only"
        if self.kind == DOCS:
            return "Comments & docstrings"
        if self.kind == REPLACE and not self.old:
            return f"insert {self.new}"
        if self.kind == REPLACE and not self.new:
            return f"delete {self.old}"
        return f"{self.old} → {self.new}"


@dataclass
class Line:
    text: str
    hl: list[list[int]] = field(default_factory=list)  # [[start_col, end_col], ...]


@dataclass
class ChangeUnit:
    id: str
    path: str
    hunk_id: str
    old_start: int  # first old line (1-based); for insertions, the line before
    new_start: int
    old: list[Line]
    new: list[Line]
    signatures: list[Signature] = field(default_factory=list)
    explained: bool = False

    def to_dict(self) -> dict:
        d = asdict(self)
        d["signatures"] = [s.key for s in self.signatures]
        return d


@dataclass
class HunkLine:
    type: str  # " ", "-", "+"
    text: str
    old_no: int | None
    new_no: int | None
    unit: str | None = None
    hl: list[list[int]] = field(default_factory=list)


@dataclass
class Hunk:
    id: str
    path: str
    old_start: int
    new_start: int
    lines: list[HunkLine]
    unit_ids: list[str]


@dataclass
class Group:
    id: str
    key: str
    kind: str
    label: str
    old: str
    new: str
    unit_ids: list[str]
    files: list[str]
    details: dict[str, int]
    mechanical: bool


@dataclass
class Location:
    path: str
    line: int
    text: str


@dataclass
class Warning:
    kind: str  # "missed-rename" | "inconsistent-rename"
    message: str
    group_id: str | None = None
    locations: list[Location] = field(default_factory=list)
    total: int = 0  # all matches found; ``locations`` may be truncated


@dataclass
class FileSummary:
    path: str
    old_path: str | None
    status: str
    analyzed: bool
    additions: int
    deletions: int
    units: int = 0
    residual_units: int = 0
    parse_ok: bool = True
    category: str = "source"  # see categories.py


@dataclass
class Report:
    id: str
    source: dict
    files: list[FileSummary]
    groups: list[Group]
    units: dict[str, ChangeUnit]
    hunks: dict[str, Hunk]
    residual_hunk_ids: list[str]
    warnings: list[Warning]

    def stats(self) -> dict:
        total = len(self.units)
        explained = sum(1 for u in self.units.values() if u.explained)
        return {
            "files_changed": len(self.files),
            "files_analyzed": sum(1 for f in self.files if f.analyzed),
            "units": total,
            "explained_units": explained,
            "residual_units": total - explained,
            "collapsed_pct": round(100 * explained / total) if total else 0,
            "mechanical_groups": sum(1 for g in self.groups if g.mechanical),
        }

    def to_dict(self) -> dict:
        return {
            "id": self.id,
            "source": self.source,
            "stats": self.stats(),
            "files": [asdict(f) for f in self.files],
            "groups": [asdict(g) for g in self.groups],
            "units": {k: u.to_dict() for k, u in self.units.items()},
            "hunks": {k: asdict(h) for k, h in self.hunks.items()},
            "residual_hunk_ids": self.residual_hunk_ids,
            "warnings": [asdict(w) for w in self.warnings],
        }
