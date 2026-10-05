"""Whole-file views of one changed file, for showing context and the old/new versions."""

from __future__ import annotations

from difflib import SequenceMatcher

from refactor_diff.languages.base import split_lines
from refactor_diff.model import Report


def file_diff(report: Report, path: str) -> dict | None:
    """Every line of the file as a unified diff (no context cut-off).

    Uses the same line matcher as the engine, so changed lines carry the ids of the units
    they belong to plus their token highlights. Rows: ``t`` (" ", "-", "+"), ``o``/``n``
    (old/new line numbers), ``text``, ``unit``, ``hl``. The old file is the rows without "+",
    the new file the rows without "-".
    """
    summary = next((f for f in report.files if f.path == path), None)
    if summary is None or path not in report.texts:
        return None
    old_text, new_text = report.texts[path]
    old, new = split_lines(old_text), split_lines(new_text)

    by_old: dict[int, tuple[str, list]] = {}
    by_new: dict[int, tuple[str, list]] = {}
    for u in report.units.values():
        if u.path != path:
            continue
        for k, ln in enumerate(u.old):
            by_old[u.old_start + k] = (u.id, ln.hl)
        for k, ln in enumerate(u.new):
            by_new[u.new_start + k] = (u.id, ln.hl)

    lines: list[dict] = []
    for tag, i1, i2, j1, j2 in SequenceMatcher(None, old, new, autojunk=False).get_opcodes():
        if tag == "equal":
            lines += [
                {"t": " ", "o": i + 1, "n": j1 + (i - i1) + 1, "text": old[i]}
                for i in range(i1, i2)
            ]
            continue
        for i in range(i1, i2):
            unit, hl = by_old.get(i + 1, (None, []))
            lines.append({"t": "-", "o": i + 1, "n": None, "text": old[i], "unit": unit, "hl": hl})
        for j in range(j1, j2):
            unit, hl = by_new.get(j + 1, (None, []))
            lines.append({"t": "+", "o": None, "n": j + 1, "text": new[j], "unit": unit, "hl": hl})

    return {
        "path": path,
        "old_path": summary.old_path,
        "status": summary.status,
        "category": summary.category,
        "analyzed": summary.analyzed,
        "old_lines": len(old),
        "new_lines": len(new),
        "lines": lines,
    }
