"""Cluster classified units into groups and run sanity checks on the result."""

from __future__ import annotations

from collections import Counter, defaultdict
from difflib import SequenceMatcher

from refactor_diff.languages.base import NAME, FileAnalysis, LanguageAnalyzer
from refactor_diff.model import (
    ALWAYS_MECHANICAL,
    ARGS,
    RENAME,
    ChangeUnit,
    Group,
    Location,
    NearMiss,
    Signature,
    Warning,
    short_hash,
)

MAX_LOCATIONS = 50
SYMBOL_CONTEXTS = {"definition", "import"}
NEAR_GROUPS_PER_KIND = 200  # largest mechanical groups considered per signature kind
NEAR_PER_UNIT = 2


def build_groups(units: list[ChangeUnit], min_count: int) -> list[Group]:
    """One group per signature key. A group is *mechanical* when it repeats at least
    ``min_count`` times (formatting always is); a unit is *explained* when every signature it
    carries belongs to a mechanical group."""
    by_key: dict[str, list[tuple[ChangeUnit, Signature]]] = defaultdict(list)
    for unit in units:
        for sig in unit.signatures:
            by_key[sig.key].append((unit, sig))

    groups: list[Group] = []
    for key, members in by_key.items():
        first = members[0][1]
        unit_ids = list(dict.fromkeys(u.id for u, _ in members))
        details = Counter(s.detail for _, s in members if s.detail)
        groups.append(
            Group(
                id=short_hash(key),
                key=key,
                kind=first.kind,
                label=first.label,
                old=first.old,
                new=first.new,
                unit_ids=unit_ids,
                files=sorted({u.path for u, _ in members}),
                details=dict(details.most_common()),
                mechanical=first.kind in ALWAYS_MECHANICAL or len(unit_ids) >= min_count,
            )
        )

    mechanical = {g.key for g in groups if g.mechanical}
    for unit in units:
        unit.explained = bool(unit.signatures) and all(s.key in mechanical for s in unit.signatures)

    groups.sort(
        key=lambda g: (not g.mechanical, g.kind in ALWAYS_MECHANICAL, -len(g.unit_ids), g.label)
    )
    return groups


def inconsistent_renames(groups: list[Group]) -> list[Warning]:
    """A symbol (definition or import) renamed to different names in different places.

    Locals and keyword arguments are skipped: renaming ``user_id`` differently in unrelated
    functions is normal."""
    targets: dict[str, list[Group]] = defaultdict(list)
    for g in groups:
        if g.kind == RENAME:
            targets[g.old].append(g)
    warnings = []
    for old, gs in targets.items():
        if len(gs) > 1 and any(SYMBOL_CONTEXTS & g.details.keys() for g in gs):
            news = ", ".join(f"{g.new} (×{len(g.unit_ids)})" for g in gs)
            warnings.append(
                Warning(
                    kind="inconsistent-rename",
                    message=f"{old} was renamed to different names: {news}",
                    group_id=max(gs, key=lambda g: len(g.unit_ids)).id,
                )
            )
    return warnings


def near_misses(units: dict[str, ChangeUnit], groups: list[Group]) -> list[Warning]:
    """Flag leftover changes that almost match a mechanical pattern: ``get_user → fetch_users``
    next to forty ``get_user → fetch_user``, or a template differing in one token. Those are
    where typos hide. Fills ``unit.near`` and returns one warning per pattern with near misses."""
    by_kind: dict[str, list[tuple[Group, _Parts]]] = defaultdict(list)
    for g in sorted(groups, key=lambda g: -len(g.unit_ids)):
        if g.mechanical and g.kind not in ALWAYS_MECHANICAL:
            if len(by_kind[g.kind]) < NEAR_GROUPS_PER_KIND:
                by_kind[g.kind].append((g, _sig_parts(g.kind, g.key, g.old, g.new)))

    hits: dict[str, list[ChangeUnit]] = defaultdict(list)
    for u in units.values():
        if u.explained:
            continue
        found: dict[str, NearMiss] = {}
        for sig in u.signatures:
            if sig.kind in ALWAYS_MECHANICAL:
                continue
            parts = _sig_parts(sig.kind, sig.key, sig.old, sig.new)
            for g, g_parts in by_kind.get(sig.kind, []):
                if g.key == sig.key:
                    continue
                score = _score(parts, g_parts, sig.kind)
                if score is None:
                    continue
                side = "new" if parts[0] == g_parts[0] else "old"
                differs = sig.new if side == "new" else sig.old
                hint = f"looks like `{g.label}` ({side} side differs: {differs})"
                best = found.get(g.id)
                if best is None or score > best.score:
                    found[g.id] = NearMiss(g.id, round(score, 3), hint)
        u.near = sorted(found.values(), key=lambda n: -n.score)[:NEAR_PER_UNIT]
        for n in u.near:
            hits[n.group_id].append(u)

    warnings = []
    by_id = {g.id: g for g in groups}
    for gid, near_units in hits.items():
        g = by_id[gid]
        locations = []
        for u in near_units[:MAX_LOCATIONS]:
            line = u.new_start if u.new else u.old_start
            text = (u.new or u.old)[0].text
            locations.append(Location(u.path, line, text))
        n = len(near_units)
        warnings.append(
            Warning(
                kind="near-miss",
                message=f"{n} change{'s' if n != 1 else ''} look{'' if n != 1 else 's'} like "
                f"a near miss of {g.label}",
                group_id=gid,
                locations=locations,
                total=n,
            )
        )
    return warnings


_Parts = tuple[str, str]


def _sig_parts(kind: str, key: str, old: str, new: str) -> _Parts:
    """The two strings to compare per side: the callee and the shape delta for argument
    changes, the displayed old/new text otherwise."""
    if kind == ARGS:
        _, name, delta = (key.split("\0", 2) + ["", ""])[:3]
        return name, delta
    return old, new


def _sim(a, b) -> float:
    if a == b:
        return 1.0
    la, lb = len(a), len(b)
    if abs(la - lb) > max(3, 0.3 * max(la, lb)):
        return 0.0
    sm = SequenceMatcher(None, a, b, autojunk=False)
    return sm.ratio() if sm.quick_ratio() >= 0.7 else 0.0


def _score(a: _Parts, b: _Parts, kind: str) -> float | None:
    so, sn = _sim(a[0], b[0]), _sim(a[1], b[1])
    if kind == ARGS:
        ok = (so == 1 and sn >= 0.75) or (sn == 1 and so >= 0.85)
    else:
        ok = (so == 1 and sn >= 0.8) or (sn == 1 and so >= 0.8) or (so >= 0.9 and sn >= 0.9)
    return (so + sn) / 2 if ok else None


def still_defined(name: str, analysis: FileAnalysis, analyzer: LanguageAnalyzer) -> bool:
    """Whether ``name`` is still defined here (def/class, or a top-level assignment)."""
    toks = analysis.tokens
    for i, tok in enumerate(toks):
        if tok.kind != NAME or tok.value != name:
            continue
        if i > 0 and toks[i - 1].value in analyzer.definition_keywords():
            return True
        if tok.start[1] == 0 and i + 1 < len(toks) and toks[i + 1].value in ("=", ":"):
            return True
    return False


def find_leftovers(group: Group, files: dict[str, FileAnalysis]) -> Warning | None:
    """One warning listing every identifier still spelled with the old name after a rename
    (comments and strings don't count)."""
    attribute_only = set(group.details) == {"attribute"}
    locations: list[Location] = []
    total = 0
    for path in sorted(files):
        analysis = files[path]
        toks = analysis.tokens
        for i, tok in enumerate(toks):
            if tok.kind != NAME or tok.value != group.old:
                continue
            if attribute_only and not (i > 0 and toks[i - 1].value == "."):
                continue
            total += 1
            line = tok.start[0]
            if len(locations) < MAX_LOCATIONS and all(
                (loc.path, loc.line) != (path, line) for loc in locations[-1:]
            ):
                text = analysis.lines[line - 1] if line - 1 < len(analysis.lines) else ""
                locations.append(Location(path, line, text))
    if not total:
        return None
    files_hit = len({loc.path for loc in locations})
    return Warning(
        kind="missed-rename",
        message=(
            f"{group.old} was renamed to {group.new} and is no longer defined, but "
            f"{total} reference{'s' if total != 1 else ''} remain"
            f"{f' in {files_hit} files' if files_hit > 1 else ''}"
        ),
        group_id=group.id,
        locations=locations,
        total=total,
    )
