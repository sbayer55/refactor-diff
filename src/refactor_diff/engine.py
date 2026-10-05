"""Turn a diff source into a Report. UI-agnostic; the web server calls ``analyze``."""

from __future__ import annotations

from pathlib import Path

from refactor_diff import sources
from refactor_diff.categories import categorize
from refactor_diff.grouping import (
    build_groups,
    find_leftovers,
    inconsistent_renames,
    near_misses,
    still_defined,
)
from refactor_diff.hunks import Opcode, candidate_units, diff_hunks
from refactor_diff.languages import analyzer_for
from refactor_diff.languages.base import FileAnalysis, LanguageAnalyzer, split_lines
from refactor_diff.model import (
    RENAME,
    ChangeUnit,
    FileChange,
    FileSummary,
    Group,
    Hunk,
    HunkLine,
    Report,
    Warning,
    short_hash,
)
from refactor_diff.moves import detect_moves, link_imports, resync_hunks
from refactor_diff.patterns import Classification, classify, make_unit
from refactor_diff.verify import verify_units


def analyze(
    repo: Path,
    base: str | None = None,
    head: str | None = None,
    pr: int | None = None,
    min_count: int = 2,
) -> Report:
    src = sources.resolve(repo, base, head, pr)
    changes = sources.load_changes(repo, src)

    files: list[FileSummary] = []
    units: dict[str, ChangeUnit] = {}
    hunks: dict[str, Hunk] = {}
    analyses: dict[str, tuple[FileAnalysis, FileAnalysis]] = {}
    texts: dict[str, tuple[str, str]] = {}

    for change in sorted(changes, key=lambda c: c.path):
        analyzer = analyzer_for(change.path)
        old_lines, new_lines = split_lines(change.old_text), split_lines(change.new_text)
        summary = FileSummary(
            change.path,
            change.old_path,
            change.status,
            analyzed=analyzer is not None,
            additions=0,
            deletions=0,
            category=categorize(change.path, analyzed=analyzer is not None),
        )
        files.append(summary)
        texts[change.path] = (change.old_text, change.new_text)
        if analyzer is None:
            for group in diff_hunks(old_lines, new_lines):
                for tag, i1, i2, j1, j2 in group:
                    if tag != "equal":
                        summary.deletions += i2 - i1
                        summary.additions += j2 - j1
            continue
        old_an, new_an = analyzer.analyze(change.old_text), analyzer.analyze(change.new_text)
        summary.parse_ok = old_an.parsed and new_an.parsed
        analyses[change.path] = (old_an, new_an)
        file_units, file_hunks = _diff_file(change, analyzer, old_an, new_an, summary)
        units.update((u.id, u) for u in file_units)
        hunks.update((h.id, h) for h in file_hunks)

    moves = detect_moves(units, hunks, analyses, analyzer_for)
    link_imports(units, moves, analyses)
    resync_hunks(hunks, units)
    groups = build_groups(list(units.values()), min_count)
    verify_units(units, analyses, groups, analyzer_for)
    by_path: dict[str, list[ChangeUnit]] = {}
    for u in units.values():
        by_path.setdefault(u.path, []).append(u)
    for f in files:
        mine = by_path.get(f.path, [])
        f.units = len(mine)
        f.residual_units = sum(1 for u in mine if not u.explained)

    warnings = inconsistent_renames(groups)
    warnings += near_misses(units, groups)
    warnings += _leftover_warnings(repo, src, groups, {p: a[1] for p, a in analyses.items()})

    residual = [h.id for h in hunks.values() if any(not units[uid].explained for uid in h.unit_ids)]
    return Report(
        id=short_hash(src.base_sha, src.head_sha or "worktree", min_count),
        source=src.to_dict() | {"min_count": min_count},
        files=files,
        groups=groups,
        units=units,
        hunks=hunks,
        residual_hunk_ids=residual,
        warnings=warnings,
        texts=texts,
    )


def _diff_file(
    change: FileChange,
    analyzer: LanguageAnalyzer,
    old_an: FileAnalysis,
    new_an: FileAnalysis,
    summary: FileSummary,
) -> tuple[list[ChangeUnit], list[Hunk]]:
    units: list[ChangeUnit] = []
    hunks: list[Hunk] = []
    old_lines, new_lines = old_an.lines, new_an.lines

    for group in diff_hunks(old_lines, new_lines):
        hunk_id = short_hash(change.path, group[0][1], group[0][3])
        lines: list[HunkLine] = []
        hunk_units: list[str] = []
        for tag, i1, i2, j1, j2 in group:
            if tag == "equal":
                lines += [
                    HunkLine(" ", old_lines[i], i + 1, j1 + (i - i1) + 1) for i in range(i1, i2)
                ]
                continue
            summary.deletions += i2 - i1
            summary.additions += j2 - j1
            op_units = [
                make_unit(change.path, hunk_id, op, cls, old_lines, new_lines)
                for op, cls in _best_split(analyzer, old_an, new_an, Opcode(tag, i1, i2, j1, j2))
            ]
            units += op_units
            hunk_units += [u.id for u in op_units]
            # Unified-diff order: all removed lines of the opcode, then all added lines.
            unit_of_old: dict[int, ChangeUnit] = {}
            unit_of_new: dict[int, ChangeUnit] = {}
            for u in op_units:
                for k in range(len(u.old)):
                    unit_of_old[u.old_start + k] = u
                for k in range(len(u.new)):
                    unit_of_new[u.new_start + k] = u
            for i in range(i1, i2):
                u = unit_of_old[i + 1]
                ln = u.old[i + 1 - u.old_start]
                lines.append(HunkLine("-", ln.text, i + 1, None, u.id, ln.hl))
            for j in range(j1, j2):
                u = unit_of_new[j + 1]
                ln = u.new[j + 1 - u.new_start]
                lines.append(HunkLine("+", ln.text, None, j + 1, u.id, ln.hl))
        changed = [ln.type + ln.text for ln in lines if ln.type != " "]
        hunks.append(
            Hunk(
                hunk_id,
                change.path,
                group[0][1] + 1,
                group[0][3] + 1,
                lines,
                hunk_units,
                fingerprint=short_hash(change.path, *changed),
            )
        )
    return units, hunks


def _best_split(
    analyzer: LanguageAnalyzer, old_an: FileAnalysis, new_an: FileAnalysis, op: Opcode
) -> list[tuple[Opcode, Classification]]:
    def run(ops: list[Opcode]) -> list[tuple[Opcode, Classification]]:
        return [
            (o, classify(analyzer, old_an, new_an, o.old_range, o.new_range, o.i1, o.j1))
            for o in ops
        ]

    block_ops, paired_ops = candidate_units(op)
    block = run(block_ops)
    if paired_ops is None:
        return block
    paired = run(paired_ops)
    # Line pairing is more granular; prefer it unless the lines don't really correspond
    # (e.g. a call re-wrapped across lines), which shows up as extra generic churn.
    if sum(c.generic_tokens for _, c in paired) > sum(c.generic_tokens for _, c in block):
        return block
    return paired


def _leftover_warnings(
    repo: Path,
    src: sources.ResolvedSource,
    groups: list[Group],
    head_analyses: dict[str, FileAnalysis],
) -> list[Warning]:
    """Flag references left behind when a definition (def/class) was renamed.

    Only renamed definitions are checked, repo-wide at head, and only when the old name is
    no longer defined anywhere: then every remaining reference points at nothing. Renamed
    locals, parameters and keyword arguments are skipped, since other variables with the same
    name are usually unrelated.
    """
    warnings: list[Warning] = []
    extra: dict[str, FileAnalysis] = {}
    for g in groups:
        if g.kind != RENAME or not g.mechanical or "definition" not in g.details:
            continue
        analyzers = {p: analyzer_for(p) for p in g.files}
        if any(a is not None and a.is_builtin(g.old) for a in analyzers.values()):
            continue
        hits = sources.grep_files(repo, src, g.old, ["*.py", "*.pyi"])
        missing = [p for p in hits if p not in head_analyses and p not in extra]
        for path, text in sources.read_file_at(repo, src, missing).items():
            analyzer = analyzer_for(path)
            if analyzer is not None:
                extra[path] = analyzer.analyze(text)
        scope = {p: head_analyses.get(p) or extra.get(p) for p in hits}
        scope = {p: an for p, an in scope.items() if an is not None}
        if any(still_defined(g.old, an, analyzer_for(p)) for p, an in scope.items()):
            continue
        warning = find_leftovers(g, scope)
        if warning is not None:
            warnings.append(warning)
    return warnings
