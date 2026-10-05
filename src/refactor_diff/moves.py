"""Detect code moved between files (or within one) and the import churn a move causes.

A move shows up in the line diff as a deletion in one place and an insertion in another. After
every file has been diffed, deletion-only and insertion-only units are compared by their token
streams (layout and comments ignored): exact matches pair first, then near matches. A paired
unit gets a ``move`` signature, which is always mechanical, so an exact move disappears from
review; a move with edits inside leaves only those edits, classified like any other change.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from difflib import SequenceMatcher

from refactor_diff.hunks import Opcode
from refactor_diff.languages.base import COMMENT, NAME, STRUCTURAL, FileAnalysis, LanguageAnalyzer
from refactor_diff.model import FORMATTING, IMPORT, MOVE, ChangeUnit, Hunk, Signature, short_hash
from refactor_diff.patterns import classify, make_unit, merge_ranges, tokens_in_range

MIN_LINES = 3  # non-blank lines a block needs before it can count as moved
MIN_TOKENS = 12
FUZZY_MIN = 0.75  # token-stream similarity for a "moved with edits" pair
MAX_FUZZY_PAIRS = 5000  # comparisons before giving up on near matches (large diffs)

Analyses = dict[str, tuple[FileAnalysis, FileAnalysis]]


@dataclass
class Move:
    id: str
    from_unit: str
    to_unit: str
    old_path: str
    old_range: tuple[int, int]  # 1-based inclusive
    new_path: str
    new_range: tuple[int, int]
    ratio: float
    names: tuple[str, ...]  # defs/classes in the block
    linked_units: list[str] = field(default_factory=list)  # import edits explained by it

    @property
    def signature(self) -> Signature:
        s, e = self.old_range
        s2, e2 = self.new_range
        return Signature(
            MOVE,
            f"{MOVE}\0{self.old_path}\0{s}-{e}\0{self.new_path}\0{s2}-{e2}",
            self.old_path,
            self.new_path,
            ", ".join(self.names) or "block",
        )


@dataclass
class _Block:
    unit_id: str
    path: str
    side: str  # "old" | "new"
    start: int  # 1-based inclusive line range within the unit
    end: int
    stream: tuple[str, ...]
    names: tuple[str, ...]
    whole: bool  # the entire unit, as opposed to one statement out of it

    @property
    def size(self) -> int:
        return len(self.stream)

    def overlaps(self, other: _Block) -> bool:
        return self.unit_id == other.unit_id and self.start <= other.end and other.start <= self.end


def detect_moves(
    units: dict[str, ChangeUnit],
    hunks: dict[str, Hunk],
    analyses: Analyses,
    analyzer_for: Callable[[str], LanguageAnalyzer | None],
) -> list[Move]:
    """Pair deleted blocks with inserted ones, splitting units where only part of one moved,
    and tag both halves with a move signature. Mutates ``units`` and ``hunks``."""
    dels: list[_Block] = []
    ins: list[_Block] = []
    for u in units.values():
        if u.path not in analyses:
            continue
        old_an, new_an = analyses[u.path]
        if u.old and not u.new and old_an.parsed:
            dels += _blocks(u, old_an, "old")
        elif u.new and not u.old and new_an.parsed:
            ins += _blocks(u, new_an, "new")
    if not dels or not ins:
        return []

    pairs = _pair(dels, ins)
    if not pairs:
        return []

    # Units with a partial match are cut at the matched statements' boundaries first.
    cuts: dict[str, list[tuple[int, int]]] = {}
    for d, i, _ in pairs:
        for b in (d, i):
            if not b.whole:
                cuts.setdefault(b.unit_id, []).append((b.start, b.end))
    pieces: dict[tuple[str, int, int], ChangeUnit] = {}
    for uid, ranges in cuts.items():
        pieces.update(_split_unit(units, hunks, units[uid], sorted(ranges), analyses, analyzer_for))

    def resolve(b: _Block) -> ChangeUnit:
        return units[b.unit_id] if b.whole else pieces[(b.unit_id, b.start, b.end)]

    moves = []
    for d, i, ratio in pairs:
        analyzer = analyzer_for(i.path)
        if analyzer is None:
            continue
        moves.append(
            _apply_move(
                resolve(d), resolve(i), analyzer, analyses[d.path][0], analyses[i.path][1], ratio
            )
        )
    return moves


def _blocks(unit: ChangeUnit, analysis: FileAnalysis, side: str) -> list[_Block]:
    lines = unit.old if side == "old" else unit.new
    start = unit.old_start if side == "old" else unit.new_start
    end = start + len(lines) - 1
    out = []
    whole = _block(unit, analysis, side, start, end, whole=True)
    if whole is not None:
        out.append(whole)
    # Statements of their own inside the block, so one function can be matched out of a
    # deleted file (or a class body).
    spans = [s for s in analysis.statements if start <= s.start and s.end <= end]
    for s in list(spans):
        spans += [c for c in s.children if c.kind != "stmt"]
    if len(spans) >= 2:
        for s in spans:
            b = _block(unit, analysis, side, s.start, s.end, whole=False)
            if b is not None:
                out.append(b)
    return out


def _block(unit, analysis, side, start, end, whole) -> _Block | None:
    stream = _stream(analysis, (start, end))
    non_blank = sum(1 for n in range(start, end + 1) if analysis.lines[n - 1].strip())
    if len(stream) < MIN_TOKENS or non_blank < MIN_LINES:
        return None
    names = tuple(
        s.qualname
        for s in _flat(analysis.statements)
        if s.qualname and start <= s.start and s.end <= end
    )
    return _Block(unit.id, unit.path, side, start, end, stream, names, whole)


def _flat(spans):
    for s in spans:
        yield s
        yield from _flat(s.children)


def _stream(analysis: FileAnalysis, rng: tuple[int, int]) -> tuple[str, ...]:
    """Token values of a line range, without layout or comments."""
    side = tokens_in_range(analysis, rng)
    return tuple(t.value for t in side.tokens if t.kind not in (STRUCTURAL, COMMENT))


def _pair(dels: list[_Block], ins: list[_Block]) -> list[tuple[_Block, _Block, float]]:
    used: list[_Block] = []

    def free(b: _Block) -> bool:
        return not any(b.overlaps(u) for u in used)

    def take(d: _Block, i: _Block, ratio: float) -> None:
        used.extend((d, i))
        pairs.append((d, i, ratio))

    pairs: list[tuple[_Block, _Block, float]] = []
    by_stream: dict[tuple[str, ...], list[_Block]] = {}
    for b in ins:
        by_stream.setdefault(b.stream, []).append(b)
    # Largest first, so a whole unit wins over the statements inside it.
    order = sorted(dels, key=lambda b: (-b.size, b.path, b.start))
    for d in order:
        if not free(d):
            continue
        candidates = [i for i in by_stream.get(d.stream, []) if free(i)]
        if candidates:
            candidates.sort(key=lambda i: (i.path != d.path, i.path, i.start))
            take(d, candidates[0], 1.0)

    scored: list[tuple[float, int, _Block, _Block]] = []
    comparisons = 0
    for d in order:
        if not free(d):
            continue
        for i in ins:
            if not free(i) or not 0.6 <= i.size / d.size <= 1.6:
                continue
            comparisons += 1
            if comparisons > MAX_FUZZY_PAIRS:
                break
            sm = SequenceMatcher(None, d.stream, i.stream, autojunk=False)
            if sm.quick_ratio() < FUZZY_MIN:
                continue
            ratio = sm.ratio()
            if ratio >= FUZZY_MIN:
                scored.append((ratio, d.size, d, i))
    scored.sort(key=lambda x: (-x[0], -x[1], x[2].path, x[2].start))
    for ratio, _, d, i in scored:
        if free(d) and free(i):
            take(d, i, ratio)
    return pairs


def _split_unit(
    units: dict[str, ChangeUnit],
    hunks: dict[str, Hunk],
    unit: ChangeUnit,
    ranges: list[tuple[int, int]],
    analyses: Analyses,
    analyzer_for,
) -> dict[tuple[str, int, int], ChangeUnit]:
    """Replace a one-sided unit by consecutive pieces cut at ``ranges`` (the matched blocks).
    Returns the piece for each range, keyed like the blocks."""
    analyzer = analyzer_for(unit.path)
    old_an, new_an = analyses[unit.path]
    deleting = bool(unit.old)
    start = unit.old_start if deleting else unit.new_start
    end = start + (len(unit.old) if deleting else len(unit.new)) - 1
    bounds = []
    cursor = start
    for s, e in ranges:
        if s > cursor:
            bounds.append((cursor, s - 1, False))
        bounds.append((s, e, True))
        cursor = e + 1
    if cursor <= end:
        bounds.append((cursor, end, False))

    out = {}
    new_units = []
    for s, e, matched in bounds:
        if deleting:
            op = Opcode("delete", s - 1, e, unit.new_start - 1, unit.new_start - 1)
        else:
            op = Opcode("insert", unit.old_start - 1, unit.old_start - 1, s - 1, e)
        cls = classify(analyzer, old_an, new_an, op.old_range, op.new_range)
        piece = make_unit(unit.path, unit.hunk_id, op, cls, old_an.lines, new_an.lines)
        new_units.append(piece)
        if matched:
            out[(unit.id, s, e)] = piece

    del units[unit.id]
    units.update((p.id, p) for p in new_units)
    hunk = hunks[unit.hunk_id]
    k = hunk.unit_ids.index(unit.id)
    hunk.unit_ids[k : k + 1] = [p.id for p in new_units]
    return out


def _apply_move(
    d_unit: ChangeUnit,
    i_unit: ChangeUnit,
    analyzer: LanguageAnalyzer,
    from_old_an: FileAnalysis,
    to_new_an: FileAnalysis,
    ratio: float,
) -> Move:
    old_range = (d_unit.old_start, d_unit.old_start + len(d_unit.old) - 1)
    new_range = (i_unit.new_start, i_unit.new_start + len(i_unit.new) - 1)
    # classify() only reads tokens inside the ranges, so it diffs across files just as well.
    cls = classify(analyzer, from_old_an, to_new_an, old_range, new_range)
    for k, ln in enumerate(d_unit.old):
        ln.hl = merge_ranges(cls.old_hl.get(old_range[0] + k, []))
    for k, ln in enumerate(i_unit.new):
        ln.hl = merge_ranges(cls.new_hl.get(new_range[0] + k, []))
    names = _names(to_new_an, new_range) or _names(from_old_an, old_range)
    move = Move(
        id=short_hash(MOVE, d_unit.id, i_unit.id),
        from_unit=d_unit.id,
        to_unit=i_unit.id,
        old_path=d_unit.path,
        old_range=old_range,
        new_path=i_unit.path,
        new_range=new_range,
        ratio=ratio,
        names=names,
    )
    # The edits made inside the moved block live on the new side only, so that a one-off edit
    # isn't counted twice and mistaken for a repeated pattern.
    d_unit.signatures = [move.signature]
    i_unit.signatures = [move.signature, *(s for s in cls.signatures if s.kind != FORMATTING)]
    d_unit.partner, i_unit.partner = i_unit.id, d_unit.id
    return move


def _names(analysis: FileAnalysis, rng: tuple[int, int]) -> tuple[str, ...]:
    return tuple(
        s.qualname
        for s in analysis.statements
        if s.qualname and rng[0] <= s.start and s.end <= rng[1]
    )


def resync_hunks(hunks: dict[str, Hunk], units: dict[str, ChangeUnit]) -> None:
    """Point hunk lines at the units (and highlights) they belong to after units changed."""
    by_old: dict[tuple[str, int], tuple[str, list[list[int]]]] = {}
    by_new: dict[tuple[str, int], tuple[str, list[list[int]]]] = {}
    for u in units.values():
        for k, ln in enumerate(u.old):
            by_old[(u.path, u.old_start + k)] = (u.id, ln.hl)
        for k, ln in enumerate(u.new):
            by_new[(u.path, u.new_start + k)] = (u.id, ln.hl)
    for h in hunks.values():
        for ln in h.lines:
            if ln.type == "-":
                ln.unit, ln.hl = by_old.get((h.path, ln.old_no), (ln.unit, ln.hl))
            elif ln.type == "+":
                ln.unit, ln.hl = by_new.get((h.path, ln.new_no), (ln.unit, ln.hl))


# --- import churn caused by a move -----------------------------------------------------------


def link_imports(units: dict[str, ChangeUnit], moves: list[Move], analyses: Analyses) -> None:
    """An import edit that only follows a move (``from a import f`` -> ``from b import f``
    after ``f`` moved from a.py to b.py, an import the moved block needs at its destination, or
    one it no longer needs at its origin) is explained by that move: its ``import`` signature
    is replaced by the move's."""
    if not moves:
        return
    for u in units.values():
        for k, sig in enumerate(u.signatures):
            if sig.kind != IMPORT:
                continue
            move = _explaining_move(u, sig, moves, analyses)
            if move is not None:
                u.signatures[k] = move.signature
                move.linked_units.append(u.id)
                break


def _explaining_move(u: ChangeUnit, sig: Signature, moves: list[Move], analyses) -> Move | None:
    _, old_mod, new_mod = sig.key.split("\0", 2)
    alias = sig.detail
    for m in moves:
        if old_mod and new_mod:
            if (
                alias in {n.split(".")[-1] for n in m.names}
                and module_matches(old_mod, m.old_path, u.path)
                and module_matches(new_mod, m.new_path, u.path)
            ):
                return m
        elif new_mod and u.path == m.new_path:
            if alias in _names_in(analyses[m.new_path][1], m.new_range):
                return m
        elif old_mod and u.path == m.old_path:
            if alias not in _names_in(analyses[m.old_path][1], None):
                return m
    return None


def _names_in(analysis: FileAnalysis, rng: tuple[int, int] | None) -> set[str]:
    toks = analysis.tokens if rng is None else tokens_in_range(analysis, rng).tokens
    return {t.value for t in toks if t.kind == NAME}


def module_matches(module: str, path: str, importer: str) -> bool:
    """Whether dotted ``module`` (possibly relative to ``importer``'s package) names ``path``."""
    if not path.endswith((".py", ".pyi")):
        return False
    parts = path.rsplit(".", 1)[0].split("/")
    if parts[-1] == "__init__":
        parts.pop()
    if module.startswith("."):
        level = len(module) - len(module.lstrip("."))
        pkg = importer.split("/")[:-1]
        base = pkg[: len(pkg) - (level - 1)] if level > 1 else pkg
        target = base + [p for p in module.lstrip(".").split(".") if p]
        return parts == target
    mod_parts = module.split(".")
    return parts[-len(mod_parts) :] == mod_parts
