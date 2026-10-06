"""Render a report as Markdown: the review summary to paste into a PR or a message."""

from __future__ import annotations

from refactor_diff.model import DOCS, FORMATTING, Hunk, HunkLine, Report


def anchor_line(report: Report, hunk: Hunk) -> HunkLine:
    """The line a comment on this hunk should attach to: the first changed line of a unit that
    still needs review, preferring the new side."""
    changed = [ln for ln in hunk.lines if ln.type != " "]
    residual = [ln for ln in changed if ln.unit and not report.units[ln.unit].explained]
    pool = residual or changed or hunk.lines
    return next((ln for ln in pool if ln.type == "+"), pool[0])


def markdown_summary(report: Report, review: dict | None = None) -> str:
    review = review or {}
    done_groups = set(review.get("groups", []))
    done_hunks = set(review.get("hunks", []))
    src = report.source
    stats = report.stats()
    sha = lambda s: f"`{s[:7]}`" if s else "working tree"  # noqa: E731

    out = [f"# refactor-diff: {src['label']}", ""]
    if src.get("pr") and src["pr"].get("url"):
        out.append(f"{src['pr']['url']} · ")
    out.append(
        f"{sha(src['base_sha'])} → {sha(src['head_sha'])} · "
        f"**{stats['collapsed_pct']}%** of changed lines collapsed · "
        f"**{stats['residual_units']}** {'change' if stats['residual_units'] == 1 else 'changes'}"
        f" to review · {stats['mechanical_groups']} mechanical "
        f"{'pattern' if stats['mechanical_groups'] == 1 else 'patterns'} · "
        f"{stats['files_analyzed']}/{stats['files_changed']} files analyzed"
        + (f" · {stats['verified_units']} verified by AST" if stats["verified_units"] else "")
    )

    mech = [g for g in report.groups if g.mechanical and g.kind not in (FORMATTING, DOCS)]
    trivial = [g for g in report.groups if g.mechanical and g.kind in (FORMATTING, DOCS)]
    if mech or trivial:
        out += [
            "",
            "## Mechanical patterns",
            "",
            "| | Kind | Pattern | Count | Files |",
            "|---|---|---|---|---|",
        ]
        for g in mech + trivial:
            tick = "✓" if g.id in done_groups else ""
            n = len(g.unit_ids)
            out.append(f"| {tick} | {g.kind} | `{_cell(g.label)}` | {n} | {len(g.files)} |")

    residual = [report.hunks[h] for h in report.residual_hunk_ids]
    out += ["", "## Needs review", ""]
    if not residual:
        out.append("Nothing: every change matched a mechanical pattern.")
    for h in residual:
        first = anchor_line(report, h)
        line = first.new_no or first.old_no or h.new_start
        text = first.text.strip()
        near = _near_note(report, h)
        box = "[x]" if h.fingerprint in done_hunks else "[ ]"
        out.append(f"- {box} `{h.path}:{line}` — `{_cell(text)}`{near}")

    if report.warnings:
        out += ["", "## Warnings", ""]
        for w in report.warnings:
            where = ", ".join(f"`{loc.path}:{loc.line}`" for loc in w.locations[:5])
            more = f" … +{w.total - len(w.locations)}" if w.total > len(w.locations) else ""
            out.append(f"- **{w.kind}**: {w.message}{f' ({where}{more})' if where else ''}")
    return "\n".join(out) + "\n"


def _near_note(report: Report, hunk) -> str:
    by_id = {g.id: g for g in report.groups}
    for uid in hunk.unit_ids:
        u = report.units[uid]
        for n in u.near:
            g = by_id.get(n.group_id)
            if g is not None:
                return f" — ≈ almost `{_cell(g.label)}`"
    return ""


def _cell(text: str) -> str:
    return text.replace("|", "\\|").replace("`", "'").replace("\n", " ")
