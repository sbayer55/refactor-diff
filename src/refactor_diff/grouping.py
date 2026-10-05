"""Cluster classified units into groups and run sanity checks on the result."""

from __future__ import annotations

from collections import Counter, defaultdict

from refactor_diff.languages.base import NAME, FileAnalysis
from refactor_diff.model import (
    FORMATTING,
    RENAME,
    ChangeUnit,
    Group,
    Signature,
    Warning,
    short_hash,
)

MAX_LEFTOVERS_PER_GROUP = 25


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
    targets: dict[str, list[Group]] = defaultdict(list)
    for g in groups:
        if g.kind == RENAME:
            targets[g.old].append(g)
    warnings = []
    for old, gs in targets.items():
        if len(gs) > 1:
            news = ", ".join(f"{g.new} (×{len(g.unit_ids)})" for g in gs)
            warnings.append(
                Warning(
                    kind="inconsistent-rename",
                    message=f"{old} was renamed to different names: {news}",
                    group_id=max(gs, key=lambda g: len(g.unit_ids)).id,
                )
            )
    return warnings


def find_leftovers(group: Group, files: dict[str, FileAnalysis]) -> list[Warning]:
    """Identifier tokens still spelled with the old name after a rename."""
    attribute_only = set(group.details) == {"attribute"}
    found: list[Warning] = []
    for path in sorted(files):
        analysis = files[path]
        toks = analysis.tokens
        for i, tok in enumerate(toks):
            if tok.kind != NAME or tok.value != group.old:
                continue
            is_attr = i > 0 and toks[i - 1].value == "."
            if attribute_only and not is_attr:
                continue
            line = tok.start[0]
            found.append(
                Warning(
                    kind="missed-rename",
                    message=f"{group.old} still appears after renaming it to {group.new}",
                    group_id=group.id,
                    path=path,
                    line=line,
                    text=analysis.lines[line - 1] if line - 1 < len(analysis.lines) else None,
                )
            )
            if len(found) >= MAX_LEFTOVERS_PER_GROUP:
                return found
    return found
