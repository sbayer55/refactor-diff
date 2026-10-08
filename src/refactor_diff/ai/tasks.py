"""The predefined Ask tasks: what each one sends and asks for.

A task names the context pieces it needs (see ``context.PIECES``), says when it is unavailable
for a location (so the menu can grey it out with a reason), gives the small hint shown next to
its menu row, and adds a few sentences of instruction to the shared system prompt.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field

from refactor_diff.ai.context import ContextBuilder, Focus, Function

SYSTEM_PROMPT = """You are helping a developer review a code change in refactor-diff, a tool that
collapses repeated mechanical edits (renames, type changes, identical substitutions) so the
reviewer only reads what is left.

The user message contains context blocks, each under a `## heading`:
- The hunk is a unified diff with old and new line numbers in the first two columns and `-`/`+`
  in the third. Lines in [[double brackets]] are annotations from refactor-diff, not code:
  `pattern:` means a repeated mechanical edit already explains that unit, `verified:` means the
  enclosing statement was proven equivalent on both sides, `near miss:` means the change almost
  matches a pattern (often a typo), `unique` means a one-off edit.
- The enclosing function or class may be given before and after the change.
- References list where a name is used, as `path:line  code`.
- Commits and the pull request describe why the change was made, when available.

Rules for your answer:
- Lead with the answer. Short paragraphs or a numbered list; no headings; no restating code.
- Focus on the lines that are not explained by a pattern unless the question is about the
  pattern itself; say when a pattern already covers something.
- Cite lines as `L<n>` for new-side line numbers and `O<n>` for old-side line numbers, and
  ranges as `L17–18`. Name other files as `path:line`.
- Be concrete about behaviour: inputs, outputs, errors, side effects. Say "I can't tell from
  the context" rather than guessing when the context is insufficient.
- Keep it under 250 words unless the question needs more.
"""


@dataclass(frozen=True)
class Task:
    id: str
    label: str
    group: str  # "Understand" | "Review and risk" | "Custom"
    pieces: frozenset[str]
    instruction: str
    unavailable: Callable[[ContextBuilder, Focus, Function | None], str | None] = lambda b, f, fn: (
        None
    )
    hint: Callable[[ContextBuilder, Focus, Function | None], str] = lambda b, f, fn: ""
    judgement: bool = False  # end with a one-line verdict
    needs_function: bool = False
    needs_both_sides: bool = False
    extra: dict = field(default_factory=dict)


def _no_def(b: ContextBuilder, f: Focus, fn: Function | None) -> str | None:
    if fn is None:
        return "the line isn't inside a function or class"
    return None


def _no_both_sides(b: ContextBuilder, f: Focus, fn: Function | None) -> str | None:
    if fn is None:
        return "the line isn't inside a function or class"
    if fn.old is None:
        return f"{fn.name} is new in this diff"
    if fn.new is None:
        return f"{fn.name} was removed in this diff"
    return None


def _no_refs(b: ContextBuilder, f: Focus, fn: Function | None) -> str | None:
    why = b.references_available(f)
    if why:
        return why
    return _no_def(b, f, fn)


def _no_history(b: ContextBuilder, f: Focus, fn: Function | None) -> str | None:
    src = b.report.source
    if not src.get("head_sha") and not src.get("pr"):
        return "no commits or pull request for the working tree"
    return None


def _hint_unexplained(b: ContextBuilder, f: Focus, fn: Function | None) -> str:
    n = b.unexplained_lines(f)
    return f"{n} unexplained line{'s' if n != 1 else ''}"


def _hint_fn(b: ContextBuilder, f: Focus, fn: Function | None) -> str:
    return f"{fn.name}()" if fn and fn.kind == "def" else (fn.qualname if fn else "")


def _hint_history(b: ContextBuilder, f: Focus, fn: Function | None) -> str:
    src = b.report.source
    parts = []
    if src.get("pr"):
        parts.append(f"PR #{src['pr']['number']}")
    commits = b.history_piece(f)["range"] if src.get("head_sha") else []
    if commits:
        parts.append(f"{len(commits)} commit{'s' if len(commits) != 1 else ''}")
    return " · ".join(parts)


def _hint_verified(b: ContextBuilder, f: Focus, fn: Function | None) -> str:
    units = [b.report.units[ln.unit] for ln in f.hunk.lines if ln.unit]
    if units and all(u.verified for u in units):
        return "verified"
    return "not verified"


TASKS: list[Task] = [
    Task(
        "explain",
        "Explain this change",
        "Understand",
        frozenset({"hunk", "function", "pr", "patterns"}),
        "Explain what this hunk changes and the intended effect. Separate the mechanical part "
        "(covered by patterns) from the substantive part in one sentence, then explain the "
        "substantive part: what behaves differently and for whom. End with anything a reviewer "
        "should double-check, if there is something.",
        hint=_hint_unexplained,
    ),
    Task(
        "function",
        "How does this function work",
        "Understand",
        frozenset({"hunk", "function"}),
        "Explain how the enclosing function works in its current (after) form: its inputs, what it "
        "returns, the main steps, error paths and side effects. Mention the change only where it "
        "matters to the explanation.",
        unavailable=_no_def,
        hint=_hint_fn,
        needs_function=True,
    ),
    Task(
        "compare",
        "Compare old vs new",
        "Understand",
        frozenset({"hunk", "function"}),
        "Compare the before and after versions of the enclosing function semantically: what it "
        "used to do, what it does now, and every observable difference (return values, errors, "
        "side effects, performance). Ignore differences that are only renames or formatting.",
        unavailable=_no_both_sides,
        hint=_hint_fn,
        needs_both_sides=True,
    ),
    Task(
        "uses",
        "Who uses this",
        "Understand",
        frozenset({"hunk", "function", "references"}),
        "Summarise how the enclosing function is used, from the references: group callers by "
        "purpose, say what each group relies on (return shape, null handling, exceptions), and "
        "point out any caller that this change affects.",
        unavailable=_no_refs,
        hint=_hint_fn,
        needs_function=True,
    ),
    Task(
        "why",
        "Why was this changed",
        "Understand",
        frozenset({"hunk", "history", "pr"}),
        "Explain why this line was changed, using the commit messages and the pull request "
        "description. Say which commit introduced it. If the stated reason doesn't match what the "
        "code does, say so.",
        unavailable=_no_history,
        hint=_hint_history,
    ),
    Task(
        "review",
        "Review this change",
        "Review and risk",
        frozenset({"hunk", "function", "pr", "patterns"}),
        "Review the substantive part of this hunk as a careful colleague: bugs, unhandled cases, "
        "behaviour changes that callers may not expect, and anything inconsistent with the "
        "rest of the diff. Number the findings, most important first; skip style nits.",
        hint=_hint_unexplained,
        judgement=True,
    ),
    Task(
        "preserving",
        "Is this behavior-preserving",
        "Review and risk",
        frozenset({"hunk", "function"}),
        "Decide whether the change to the enclosing function preserves behaviour for every input. "
        "Reason from the before and after versions; list each difference you can find with the "
        "input that exposes it. Be strict: a renamed function with one extra guard is not "
        "behaviour-preserving.",
        unavailable=_no_both_sides,
        hint=_hint_verified,
        judgement=True,
        needs_both_sides=True,
    ),
    Task(
        "break",
        "What could break",
        "Review and risk",
        frozenset({"hunk", "function", "references"}),
        "Assess the blast radius of this change: which callers and code paths are affected, "
        "which of them could now fail or behave differently, and what should be tested. Use the "
        "references list; name the files and lines.",
        unavailable=_no_refs,
        hint=_hint_fn,
        judgement=True,
        needs_function=True,
    ),
]

CUSTOM = Task(
    "custom",
    "Custom prompt",
    "Custom",
    frozenset({"hunk", "patterns"}),
    "Answer the developer's question about this location using the context provided.",
)

BY_ID = {t.id: t for t in TASKS} | {CUSTOM.id: CUSTOM}

JUDGEMENT_SUFFIX = (
    " End with one line that starts with `Verdict:` saying whether anything needs the "
    "reviewer's attention."
)


def system_prompt(task: Task) -> str:
    instruction = task.instruction + (JUDGEMENT_SUFFIX if task.judgement else "")
    return f"{SYSTEM_PROMPT}\nTask: {instruction}"


def menu(builder: ContextBuilder, focus: Focus) -> list[dict]:
    """The menu rows for a location: label, group, hint and why a task is unavailable."""
    fn = builder.function_piece(focus)
    rows = []
    for t in TASKS:
        rows.append(
            {
                "id": t.id,
                "label": t.label,
                "group": t.group,
                "hint": t.hint(builder, focus, fn),
                "unavailable": t.unavailable(builder, focus, fn),
            }
        )
    return rows
