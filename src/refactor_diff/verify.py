"""Prove that a mechanical-looking change really is one.

For every statement (function, class method, top-level statement) that contains changes
classified only as formatting, docs, rename or retype, the old and new versions are parsed
and compared after normalization: docstrings dropped, the diff's mechanical renames applied
to the old side, annotations dropped when the span contains a retype. Equal trees mean the
statement is the same program, and every unit inside it is marked ``verified``.

Verification is a property of the whole statement: a function that has one rename and one
logic change verifies neither. It never changes whether a unit is *explained*; it only tells
the reviewer which collapsed changes are safe beyond doubt.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Callable

from refactor_diff.languages.base import FileAnalysis, LanguageAnalyzer, StmtSpan
from refactor_diff.model import (
    DOCS,
    FORMATTING,
    MOVE,
    RENAME,
    RETYPE,
    ChangeUnit,
    Group,
)

VERIFIABLE = {FORMATTING, DOCS, RENAME, RETYPE}
TRIVIAL = {FORMATTING, DOCS}


def verify_units(
    units: dict[str, ChangeUnit],
    analyses: dict[str, tuple[FileAnalysis, FileAnalysis]],
    groups: list[Group],
    analyzer_for: Callable[[str], LanguageAnalyzer | None],
) -> None:
    mechanical = {g.key for g in groups if g.mechanical}
    by_path: dict[str, list[ChangeUnit]] = defaultdict(list)
    for u in units.values():
        by_path[u.path].append(u)

    for path, mine in by_path.items():
        analyzer = analyzer_for(path)
        if analyzer is None or path not in analyses or not hasattr(analyzer, "normalized_dump"):
            continue
        old_an, new_an = analyses[path]
        if old_an.tree is None or new_an.tree is None:
            continue
        # Group the file's units by the old-side statement they sit in.
        by_span: dict[StmtSpan | None, list[ChangeUnit]] = defaultdict(list)
        for u in mine:
            if u.partner is not None:
                continue  # moves are verified against their partner below
            rng = (u.old_start, u.old_start + len(u.old) - 1) if u.old else None
            by_span[_span_for(old_an.statements, rng, u.old_start)].append(u)
        for span, span_units in by_span.items():
            kinds = {s.kind for u in span_units for s in u.signatures}
            if not kinds or not kinds <= VERIFIABLE:
                continue
            if span is None:
                # Blank or comment lines between statements.
                for u in span_units:
                    u.verified = kinds <= TRIVIAL
                continue
            renames = _rename_map(span_units, mechanical)
            new_rng = _new_range(span_units)
            other = _counterpart(span, new_an.statements, renames, new_rng)
            if other is None:
                continue
            strip = RETYPE in kinds
            ok = analyzer.normalized_dump(span.node, renames, strip) == analyzer.normalized_dump(
                other.node, {}, strip
            )
            for u in span_units:
                u.verified = ok

    _verify_moves(units, analyses, analyzer_for, mechanical)


def _verify_moves(units, analyses, analyzer_for, mechanical) -> None:
    for u in units.values():
        if u.partner is None or not u.new:
            continue  # handle each move once, from its new side
        d = units.get(u.partner)
        if d is None:
            continue
        kinds = {s.kind for s in u.signatures} - {MOVE}
        if not kinds <= VERIFIABLE:
            continue
        analyzer = analyzer_for(u.path)
        if analyzer is None or not hasattr(analyzer, "parse_block"):
            continue  # the language can't verify blocks
        old_text = "\n".join(ln.text for ln in d.old)
        new_text = "\n".join(ln.text for ln in u.new)
        old_tree, new_tree = analyzer.parse_block(old_text), analyzer.parse_block(new_text)
        if old_tree is None or new_tree is None:
            continue
        renames = _rename_map([u], mechanical)
        strip = RETYPE in kinds
        ok = analyzer.normalized_dump(old_tree, renames, strip) == analyzer.normalized_dump(
            new_tree, {}, strip
        )
        u.verified = d.verified = ok


def _span_for(
    statements: list[StmtSpan], rng: tuple[int, int] | None, anchor: int
) -> StmtSpan | None:
    """The smallest statement containing the lines (a class when they cross its methods)."""
    first, last = rng if rng else (anchor, anchor)
    for s in statements:
        if s.start <= first and last <= s.end:
            for c in s.children:
                if c.start <= first and last <= c.end:
                    return c
            return s
    return None


def _counterpart(
    span: StmtSpan,
    statements: list[StmtSpan],
    renames: dict[str, str],
    new_rng: tuple[int, int] | None,
) -> StmtSpan | None:
    """The new-side statement matching ``span``: by (renamed) name for defs and classes,
    otherwise the statement at the units' new position."""
    if span.qualname:
        wanted = ".".join(renames.get(p, p) for p in span.qualname.split("."))
        for s in _flat(statements):
            if s.qualname == wanted and s.kind == span.kind:
                return s
        return None
    if new_rng is None:
        return None
    return _span_for(statements, new_rng, new_rng[0])


def _new_range(span_units: list[ChangeUnit]) -> tuple[int, int] | None:
    starts = [u.new_start for u in span_units if u.new]
    ends = [u.new_start + len(u.new) - 1 for u in span_units if u.new]
    if not starts:
        anchors = [u.new_start for u in span_units]
        return (min(anchors), min(anchors)) if anchors else None
    return min(starts), max(ends)


def _rename_map(span_units: list[ChangeUnit], mechanical: set[str]) -> dict[str, str]:
    targets: dict[str, set[str]] = defaultdict(set)
    for u in span_units:
        for s in u.signatures:
            if s.kind == RENAME and s.key in mechanical:
                targets[s.old].add(s.new)
    return {old: next(iter(news)) for old, news in targets.items() if len(news) == 1}


def _flat(spans):
    for s in spans:
        yield s
        yield from _flat(s.children)
