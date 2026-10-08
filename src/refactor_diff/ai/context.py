"""What the model gets to see: the pieces of context around one location in a report.

Everything refactor-diff already knows about a hunk is turned into text here: the hunk with
its pattern annotations, the enclosing function on both sides (from the language analyzers'
statement spans), references from the navigator, the commits that touched the line, the pull
request, and the diff's mechanical patterns. Tasks pick which pieces they need; ``render``
lays them out in a fixed order so prompts stay stable from one request to the next.
"""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

from refactor_diff import sources
from refactor_diff.export import anchor_line
from refactor_diff.languages import analyzer_for
from refactor_diff.languages.base import FileAnalysis, StmtSpan, split_lines
from refactor_diff.model import RENAME, Hunk, Report
from refactor_diff.navigation import NavigationError, Navigator

FUNCTION_LINE_CAP = 400  # lines per side of the enclosing function
REFERENCES_CAP = 50
PATTERNS_CAP = 20

PIECES = ("hunk", "function", "references", "history", "pr", "patterns")

SIDES = {"o": "old", "old": "old", "n": "new", "new": "new"}


class ContextError(ValueError):
    pass


@dataclass(frozen=True)
class Focus:
    hunk: Hunk
    path: str  # the new path (how the UI names files)
    side: str  # "old" | "new"
    line: int


@dataclass
class Span:
    path: str
    start: int
    end: int
    text: str
    truncated: bool = False


@dataclass
class Function:
    qualname: str
    kind: str  # "def" | "class"
    old: Span | None
    new: Span | None

    @property
    def name(self) -> str:
        return self.qualname.rsplit(".", 1)[-1]


class ContextBuilder:
    def __init__(self, report: Report, repo: Path, navigator: Navigator | None = None):
        self.report = report
        self.repo = repo
        self.navigator = navigator
        self._groups_by_key = {g.key: g for g in report.groups}
        self._groups_by_id = {g.id: g for g in report.groups}

    # --- focus -------------------------------------------------------------------------------

    def focus(self, hunk_id: str, side: str | None = None, line: int | None = None) -> Focus:
        hunk = self.report.hunks.get(hunk_id or "")
        if hunk is None:
            raise ContextError("Unknown hunk; run the analysis again.")
        if line is None:
            at = anchor_line(self.report, hunk)
            return Focus(
                hunk, hunk.path, "new" if at.new_no else "old", at.new_no or at.old_no or 1
            )
        side_name = SIDES.get(side or "")
        if side_name is None:
            raise ContextError("side must be old or new.")
        nums = {ln.new_no if side_name == "new" else ln.old_no for ln in hunk.lines}
        if line not in nums:
            raise ContextError(f"Line {line} isn't part of that hunk.")
        return Focus(hunk, hunk.path, side_name, line)

    # --- pieces ------------------------------------------------------------------------------

    def hunk_piece(self, focus: Focus) -> str:
        """The hunk as a numbered unified diff with annotation lines for pattern-explained
        units, verified units and near misses."""
        r = self.report
        out = []
        noted: set[str] = set()
        for ln in focus.hunk.lines:
            unit = r.units.get(ln.unit) if ln.unit else None
            if unit and unit.id not in noted:
                noted.add(unit.id)
                for note in self._unit_notes(unit):
                    out.append(f"[[{note}]]")
            o = str(ln.old_no) if ln.old_no else ""
            n = str(ln.new_no) if ln.new_no else ""
            out.append(f"{o:>5} {n:>5} {ln.type if ln.type != ' ' else ' '} {ln.text}")
        return "\n".join(out)

    def unexplained_lines(self, focus: Focus) -> int:
        r = self.report
        return sum(
            1
            for ln in focus.hunk.lines
            if ln.type != " " and (not ln.unit or not r.units[ln.unit].explained)
        )

    def _unit_notes(self, unit) -> list[str]:
        notes = []
        for sig in unit.signatures:
            g = self._groups_by_key.get(sig.key)
            if g is None:
                continue
            if g.mechanical:
                notes.append(
                    f"pattern: {g.kind} {g.label} (×{len(g.unit_ids)} in {len(g.files)} files)"
                )
            else:
                notes.append(f"unique {g.kind}: {g.label}")
        for near in unit.near:
            g = self._groups_by_id.get(near.group_id)
            if g:
                notes.append(f"near miss: almost {g.kind} {g.label} — {near.hint}")
        if unit.verified:
            notes.append("verified: the enclosing statement is the same program on both sides")
        return notes

    def function_piece(self, focus: Focus) -> Function | None:
        """The innermost def/class around the focus line, on both sides when it exists."""
        this_side = self._span_at(focus.path, focus.side, focus.line)
        if this_side is None:
            return None
        other_name = "old" if focus.side == "new" else "new"
        other = self._counterpart(focus, this_side, other_name)
        spans = {focus.side: this_side, other_name: other}
        kind = this_side.kind
        return Function(
            qualname=this_side.qualname,
            kind=kind,
            old=self._span_text(focus.path, "old", spans["old"]),
            new=self._span_text(focus.path, "new", spans["new"]),
        )

    def _analysis(self, path: str, side: str) -> FileAnalysis | None:
        return _analyze(self.report, path, side)

    def _span_at(self, path: str, side: str, line: int) -> StmtSpan | None:
        an = self._analysis(path, side)
        if an is None:
            return None
        return _innermost(an.statements, line)

    def _counterpart(self, focus: Focus, span: StmtSpan, other: str) -> StmtSpan | None:
        """The same def on the other side: by (renamed) qualified name, else by the hunk's
        line numbers on that side."""
        an = self._analysis(focus.path, other)
        if an is None:
            return None
        renames = self._rename_map(to_old=(other == "old"))
        wanted = ".".join(renames.get(p, p) for p in span.qualname.split("."))
        for s in _flat(an.statements):
            if s.kind == span.kind and s.qualname and s.qualname == wanted:
                return s
        nums = [
            (ln.old_no if other == "old" else ln.new_no)
            for ln in focus.hunk.lines
            if (ln.old_no if other == "old" else ln.new_no)
        ]
        for n in nums:
            s = _innermost(an.statements, n)
            if s is not None and s.kind == span.kind:
                return s
        return None

    def _rename_map(self, to_old: bool) -> dict[str, str]:
        out: dict[str, str] = {}
        for g in self.report.groups:
            if g.kind == RENAME and g.mechanical:
                a, b = (g.new, g.old) if to_old else (g.old, g.new)
                out[a] = b
        return out

    def _span_text(self, path: str, side: str, span: StmtSpan | None) -> Span | None:
        if span is None:
            return None
        an = self._analysis(path, side)
        if an is None:
            return None
        lines = an.lines[span.start - 1 : span.end]
        truncated = len(lines) > FUNCTION_LINE_CAP
        if truncated:
            lines = lines[:FUNCTION_LINE_CAP]
        side_path = _side_path(self.report, side, path)
        return Span(side_path, span.start, span.start + len(lines) - 1, "\n".join(lines), truncated)

    def references_piece(self, focus: Focus, fn: Function | None) -> dict | None:
        """Where the enclosing def is used, at head (or at base when it only exists there)."""
        if self.navigator is None or fn is None:
            return None
        side = "new" if fn.new else "old"
        span = fn.new or fn.old
        assert span is not None
        pos = self._name_position(focus.path, side, span, fn.name)
        if pos is None:
            return None
        sha = self.report.source["base_sha"] if side == "old" else self.report.source["head_sha"]
        try:
            locs = self.navigator.references(sha, span.path, pos[0], pos[1])
        except NavigationError as e:
            return {"error": str(e), "side": side, "name": fn.name}
        locs = [loc for loc in locs if loc.kind == "repo"]
        shown = locs[:REFERENCES_CAP]
        return {
            "name": fn.name,
            "side": side,
            "total": len(locs),
            "files": len({loc.path for loc in locs}),
            "locations": [
                {
                    "path": loc.path,
                    "line": loc.line,
                    "text": loc.text.strip(),
                    "def": loc.is_definition,
                }
                for loc in shown
            ],
        }

    def references_available(self, focus: Focus) -> str | None:
        """None when references can be looked up, else why not."""
        if self.navigator is None:
            return "code navigation is off"
        if analyzer_for(focus.path) is None:
            return "no code navigation for this file type"
        return None

    def _name_position(self, path: str, side: str, span: Span, name: str) -> tuple[int, int] | None:
        an = self._analysis(path, side)
        if an is None:
            return None
        pattern = re.compile(rf"(?<![\w$]){re.escape(name)}(?![\w$])")
        for offset, text in enumerate(an.lines[span.start - 1 : span.end]):
            m = pattern.search(text)
            if m:
                return span.start + offset, m.start() + 1
        return None

    def history_piece(self, focus: Focus) -> dict:
        """The commits in the range that touched the focus line, plus the whole range."""
        src = self.report.source
        base, head = src["base_sha"], src["head_sha"]
        commits = sources.list_commits(self.repo, base, head) if head else []
        touching: list[dict] = []
        if head and commits:
            touching = self._line_commits(focus, base, head)
        return {"range": commits, "touching": touching}

    def _line_commits(self, focus: Focus, base: str, head: str) -> list[dict]:
        fmt = "--format=%x1e%H%x1f%h%x1f%s%x1f%an%x1f%aI%x1f%b"
        if focus.side == "new":
            args = ["log", fmt, f"{base}..{head}", f"-L{focus.line},{focus.line}:{focus.path}"]
        else:
            text = next((ln.text for ln in focus.hunk.lines if ln.old_no == focus.line), "").strip()
            if len(text) < 6:
                return []
            path = _side_path(self.report, "old", focus.path)
            args = ["log", fmt, f"{base}..{head}", "-S", text, "--", path, focus.path]
        try:
            out = sources.git_text(self.repo, *args)
        except (sources.SourceError, subprocess.CalledProcessError, OSError):
            return []
        found = []
        for record in out.split("\x1e"):
            if not record.strip():
                continue
            record = record.split("\ndiff --git", 1)[0]
            sha, short, subject, author, date, body = (record.strip("\n").split("\x1f") + [""] * 6)[
                :6
            ]
            found.append(
                {
                    "sha": sha,
                    "short": short,
                    "subject": subject,
                    "author": author,
                    "date": date,
                    "body": body.strip(),
                }
            )
        return found

    def pr_piece(self) -> dict | None:
        pr = self.report.source.get("pr")
        if not pr:
            return None
        return {
            "number": pr.get("number"),
            "title": pr.get("title", ""),
            "url": pr.get("url", ""),
            "body": (pr.get("body") or "").strip(),
        }

    def patterns_piece(self) -> list[str]:
        groups = sorted(
            (g for g in self.report.groups if g.mechanical),
            key=lambda g: -len(g.unit_ids),
        )
        return [
            f"{g.kind} {g.label} ×{len(g.unit_ids)} in {len(g.files)} files"
            for g in groups[:PATTERNS_CAP]
        ]

    # --- assembling --------------------------------------------------------------------------

    def collect(self, focus: Focus, pieces: set[str]) -> dict:
        """The requested pieces (plus the hunk and the patterns, always) as plain data."""
        out: dict = {
            "focus": focus,
            "hunk": self.hunk_piece(focus),
            "patterns": self.patterns_piece(),
        }
        fn = self.function_piece(focus) if {"function", "references"} & pieces else None
        if "function" in pieces:
            out["function"] = fn
        if "references" in pieces:
            out["references"] = self.references_piece(focus, fn)
        if "history" in pieces:
            out["history"] = self.history_piece(focus)
        if "pr" in pieces:
            out["pr"] = self.pr_piece()
        return out


def render(ctx: dict) -> str:
    """The user message: every collected piece as a titled block, in a fixed order."""
    focus: Focus = ctx["focus"]
    fn: Function | None = ctx.get("function")
    where = f"{focus.path}, {focus.side} side, line {focus.line}"
    if fn:
        where += f" (inside {fn.kind} {fn.qualname})"
    blocks = [f"## Location\n{where}"]
    h = focus.hunk
    blocks.append(
        f"## Hunk ({h.path} @@ old line {h.old_start}, new line {h.new_start})\n{ctx['hunk']}"
    )
    if fn:
        lang = _fence_lang(focus.path)
        for label, span in (("before", fn.old), ("after", fn.new)):
            if span is None:
                blocks.append(
                    f"## Enclosing {fn.kind} {fn.qualname} — {label}\n(does not exist on this side)"
                )
                continue
            cut = f" (cut after {FUNCTION_LINE_CAP} lines)" if span.truncated else ""
            at = f"{span.path}:{span.start}–{span.end}{cut}"
            blocks.append(
                f"## Enclosing {fn.kind} {fn.qualname} — {label} ({at})\n"
                f"```{lang}\n{span.text}\n```"
            )
    elif "function" in ctx:
        blocks.append("## Enclosing function\n(none: the line is not inside a def or class)")
    refs = ctx.get("references")
    if refs:
        if refs.get("error"):
            blocks.append(f"## References to {refs['name']}\n(unavailable: {refs['error']})")
        else:
            side = "head" if refs["side"] == "new" else "base"
            lines = [
                f"{loc['path']}:{loc['line']}  {loc['text']}"
                + ("   [definition]" if loc["def"] else "")
                for loc in refs["locations"]
            ]
            more = refs["total"] - len(refs["locations"])
            if more > 0:
                lines.append(f"… and {more} more")
            count = f"{refs['total']} in {refs['files']} files, at {side}"
            blocks.append(f"## References to {refs['name']} ({count})\n" + "\n".join(lines))
    elif "references" in ctx:
        blocks.append("## References\n(unavailable)")
    hist = ctx.get("history")
    if hist is not None:
        if not hist["range"]:
            blocks.append("## Commits\n(none: this is the working tree)")
        else:
            lines = []
            for c in hist["touching"]:
                lines.append(f"{c['short']} {c['subject']} ({c['author']}, {c['date'][:10]})")
                if c["body"]:
                    lines.append("    " + c["body"].replace("\n", "\n    "))
            touching = (
                "\n".join(lines) if lines else "(no commit in the range touches this line directly)"
            )
            rng = "\n".join(f"{c['short']} {c['subject']}" for c in hist["range"])
            blocks.append(f"## Commits that touched this line\n{touching}")
            blocks.append(f"## All commits in the range (oldest first)\n{rng}")
    pr = ctx.get("pr")
    if pr:
        body = pr["body"] or "(no description)"
        blocks.append(f"## Pull request #{pr['number']}: {pr['title']}\n{body}")
    elif "pr" in ctx:
        blocks.append("## Pull request\n(this comparison is not a pull request)")
    pats = ctx.get("patterns") or []
    blocks.append("## Mechanical patterns in this diff\n" + ("\n".join(pats) if pats else "(none)"))
    return "\n\n".join(blocks)


# --- helpers ------------------------------------------------------------------------------------


def _side_path(report: Report, side: str, path: str) -> str:
    if side == "old":
        for f in report.files:
            if f.path == path and f.old_path:
                return f.old_path
    return path


def _fence_lang(path: str) -> str:
    an = analyzer_for(path)
    if an is None:
        return ""
    return {"python": "python", "typescript": "ts", "tsx": "tsx", "javascript": "js"}.get(
        an.name, ""
    )


def _innermost(statements: list[StmtSpan], line: int) -> StmtSpan | None:
    for s in statements:
        if s.start <= line <= s.end:
            for c in s.children:
                if c.start <= line <= c.end and c.kind in ("def", "class"):
                    return c
            return s if s.kind in ("def", "class") else None
    return None


def _flat(statements: list[StmtSpan]):
    for s in statements:
        yield s
        yield from _flat(list(s.children))


@lru_cache(maxsize=64)
def _analyze_text(path: str, text: str) -> FileAnalysis | None:
    analyzer = analyzer_for(path)
    if analyzer is None:
        return None
    try:
        return analyzer.analyze(text)
    except Exception:  # a file the parser can't handle: no structure, no function piece
        return FileAnalysis(lines=split_lines(text), tokens=[], parsed=False)


def _analyze(report: Report, path: str, side: str) -> FileAnalysis | None:
    texts = report.texts.get(path)
    if texts is None:
        return None
    text = texts[0] if side == "old" else texts[1]
    if not text:
        return None
    return _analyze_text(path, text)
