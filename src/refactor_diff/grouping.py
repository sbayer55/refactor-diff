"""Cluster classified units into groups and run sanity checks on the result."""

from __future__ import annotations

from collections import Counter, defaultdict

from refactor_diff.languages.base import NAME, FileAnalysis, LanguageAnalyzer
from refactor_diff.model import (
    FORMATTING,
    RENAME,
    ChangeUnit,
    Group,
    Location,
    Signature,
    Warning,
    short_hash,
)

MAX_LOCATIONS = 50
SYMBOL_CONTEXTS = {"definition", "import"}


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
                mechanical=first.kind == FORMATTING or len(unit_ids) >= min_count,
            )
        )

    mechanical = {g.key for g in groups if g.mechanical}
    for unit in units:
        unit.explained = bool(unit.signatures) and all(s.key in mechanical for s in unit.signatures)

    groups.sort(key=lambda g: (not g.mechanical, g.kind == FORMATTING, -len(g.unit_ids), g.label))
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
