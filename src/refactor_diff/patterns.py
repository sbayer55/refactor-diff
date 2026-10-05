"""Classify one change unit into mechanical edit signatures.

The old and new token streams of the unit are aligned with ``difflib``. Each differing
span becomes a signature:

* rename   - a single identifier swapped for another (``get_user`` -> ``fetch_user``)
* retype   - every changed token sits inside a type annotation
* replace  - any other token-level substitution, insertion or deletion
* formatting - token streams are identical (only whitespace/layout changed)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from difflib import SequenceMatcher

from refactor_diff.languages.base import (
    COMMENT,
    NAME,
    NUMBER,
    OP,
    OTHER,
    STRING,
    STRUCTURAL,
    Annotation,
    FileAnalysis,
    LanguageAnalyzer,
    Token,
)
from refactor_diff.model import DOCS, FORMATTING, RENAME, REPLACE, RETYPE, Signature

MAX_LABEL = 160
MAX_WRAP_GAP = 12  # unchanged tokens a bracket wrap may enclose
PLACEHOLDER = "…"
_ANNOTATION_PUNCT = {":", "->"}
_BRACKETS = {"(": 1, "[": 1, "{": 1, ")": -1, "]": -1, "}": -1}
_JOIN_GAPS = {".", "="}
_HOLE_KINDS = {NAME, STRING, NUMBER}

LineRange = tuple[int, int] | None  # inclusive (first, last), None when empty


@dataclass
class Classification:
    signatures: list[Signature]
    old_hl: dict[int, list[list[int]]] = field(default_factory=dict)  # line -> ranges
    new_hl: dict[int, list[list[int]]] = field(default_factory=dict)
    generic_tokens: int = 0  # tokens covered by "replace" signatures; lower = cleaner


@dataclass
class _Side:
    analysis: FileAnalysis
    tokens: list[Token]  # tokens overlapping the unit's lines
    offset: int  # index of tokens[0] in analysis.tokens


def tokens_in_range(analysis: FileAnalysis, rng: LineRange) -> _Side:
    if rng is None:
        return _Side(analysis, [], 0)
    first, last = rng
    idx = [i for i, t in enumerate(analysis.tokens) if t.start[0] <= last and t.end[0] >= first]
    if not idx:
        return _Side(analysis, [], 0)
    return _Side(analysis, analysis.tokens[idx[0] : idx[-1] + 1], idx[0])


def classify(
    analyzer: LanguageAnalyzer,
    old: FileAnalysis,
    new: FileAnalysis,
    old_range: LineRange,
    new_range: LineRange,
) -> Classification:
    a = tokens_in_range(old, old_range)
    b = tokens_in_range(new, new_range)
    a_vals = [t.value for t in a.tokens]
    b_vals = [t.value for t in b.tokens]
    if a_vals == b_vals:
        return Classification([Signature(FORMATTING, FORMATTING, "", "")])

    ops = [
        op
        for op in SequenceMatcher(None, a_vals, b_vals, autojunk=False).get_opcodes()
        if op[0] != "equal"
    ]
    classified = [_classify_op(analyzer, a, b, op) for op in ops]

    result = Classification([])
    for cluster in _clusters(a, b, ops, classified):
        if all(classified[k] is not None for k in cluster):
            for k in cluster:
                _add(result, classified[k])
        elif len(cluster) == 1:
            _, i1, i2, j1, j2 = ops[cluster[0]]
            result.generic_tokens += i2 - i1 + j2 - j1
            _add(result, _replace_sig(a, b, i1, i2, j1, j2))
        else:
            cluster_ops = [ops[k] for k in cluster]
            result.generic_tokens += sum(i2 - i1 + j2 - j1 for _, i1, i2, j1, j2 in cluster_ops)
            _add(result, _template_sig(a, b, cluster_ops))
        for k in cluster:
            _, i1, i2, j1, j2 = ops[k]
            _highlight(result.old_hl, a.tokens[i1:i2], old_range)
            _highlight(result.new_hl, b.tokens[j1:j2], new_range)
    return result


def _add(result: Classification, sig: Signature) -> None:
    if all(s.key != sig.key for s in result.signatures):
        result.signatures.append(sig)


def _depth(tokens: list[Token]) -> int:
    return sum(_BRACKETS.get(t.value, 0) for t in tokens if t.kind == OP)


def _clusters(a: _Side, b: _Side, ops, classified) -> list[list[int]]:
    """Group nearby ops that form one edit, so it gets one signature instead of fragments:

    * wraps - an op leaves a bracket open and a later op closes it around unchanged code:
      ``actor`` -> ``str(actor.user_id)``, ``role="x"`` -> ``roles=("x",)``
    * ops separated by a single ``.`` or ``=``: ``cfg.get("x")`` -> ``settings.x``

    Retypes never join a cluster; a cluster made only of renames is reported as renames.
    """
    clusters: list[list[int]] = []
    open_old = open_new = 0
    for k, (_, i1, i2, j1, j2) in enumerate(ops):
        if clusters:
            last = ops[clusters[-1][-1]]
            gap = a.tokens[last[2] : i1]
            joinable = (
                _joinable(classified[k])
                and _joinable(classified[clusters[-1][-1]])
                and not any(t.kind == STRUCTURAL for t in gap)
            )
            wrap = (open_old > 0 or open_new > 0) and len(gap) <= MAX_WRAP_GAP
            short = len(gap) == 1 and gap[0].value in _JOIN_GAPS
            if joinable and (wrap or short):
                clusters[-1].append(k)
                open_old += _depth(gap) + _depth(a.tokens[i1:i2])
                open_new += _depth(gap) + _depth(b.tokens[j1:j2])
                continue
        clusters.append([k])
        open_old, open_new = _depth(a.tokens[i1:i2]), _depth(b.tokens[j1:j2])
    return clusters


def _joinable(sig: Signature | None) -> bool:
    return sig is None or sig.kind == RENAME


def _classify_op(analyzer: LanguageAnalyzer, a: _Side, b: _Side, op) -> Signature | None:
    tag, i1, i2, j1, j2 = op
    old_toks, new_toks = a.tokens[i1:i2], b.tokens[j1:j2]
    if _is_docs(old_toks, a.analysis) and _is_docs(new_toks, b.analysis):
        if any(t.kind != STRUCTURAL for t in old_toks + new_toks):
            return Signature(DOCS, DOCS, "", "")
    retype = _retype_sig(a, b, i1, i2, j1, j2)
    if retype is not None:
        return retype
    if (
        tag == "replace"
        and len(old_toks) == 1
        and len(new_toks) == 1
        and old_toks[0].kind == NAME
        and new_toks[0].kind == NAME
        and not analyzer.is_keyword(old_toks[0].value)
        and not analyzer.is_keyword(new_toks[0].value)
    ):
        old_name, new_name = old_toks[0].value, new_toks[0].value
        detail = rename_context(analyzer, a.analysis.tokens, a.offset + i1)
        return Signature(
            RENAME, f"{RENAME}\x00{old_name}\x00{new_name}", old_name, new_name, detail
        )
    return None


def _is_docs(tokens: list[Token], analysis: FileAnalysis) -> bool:
    """Every token is a comment, a docstring, or layout."""
    return all(
        t.kind in (COMMENT, STRUCTURAL)
        or t.kind == STRING
        and any(s <= t.start and t.end <= e for s, e in analysis.docstrings)
        for t in tokens
    )


def rename_context(analyzer: LanguageAnalyzer, tokens: list[Token], i: int) -> str:
    prev = tokens[i - 1].value if i > 0 else ""
    nxt = tokens[i + 1].value if i + 1 < len(tokens) else ""
    if prev in analyzer.definition_keywords():
        return "definition"
    if prev == ".":
        return "attribute"
    k = i
    while k > 0 and tokens[k - 1].kind != STRUCTURAL:
        k -= 1
    if tokens[k].value in analyzer.import_keywords():
        return "import"
    if nxt == "(":
        return "call"
    if nxt == "=" and prev in ("(", ","):
        return "keyword"
    return "name"


def _retype_sig(a: _Side, b: _Side, i1: int, i2: int, j1: int, j2: int) -> Signature | None:
    old_sig = [t for t in a.tokens[i1:i2] if t.value not in _ANNOTATION_PUNCT]
    new_sig = [t for t in b.tokens[j1:j2] if t.value not in _ANNOTATION_PUNCT]
    if not old_sig and not new_sig:
        return None
    old_ann = _enclosing(a.analysis.annotations, old_sig, a.tokens, i1, i2)
    new_ann = _enclosing(b.analysis.annotations, new_sig, b.tokens, j1, j2)
    if old_sig and old_ann is None or new_sig and new_ann is None:
        return None
    if old_ann is None and new_ann is None:
        return None
    old_text = old_ann.text if old_ann else "(untyped)"
    new_text = new_ann.text if new_ann else "(untyped)"
    target = (new_ann or old_ann).target
    return Signature(RETYPE, f"{RETYPE}\x00{old_text}\x00{new_text}", old_text, new_text, target)


def _enclosing(
    annotations: list[Annotation], changed: list[Token], unit_tokens: list[Token], i1: int, i2: int
) -> Annotation | None:
    """The annotation containing every changed token, or - for a pure insertion on this side -
    the annotation around the insertion point."""
    if changed:
        for ann in annotations:
            if all(ann.contains(t) for t in changed):
                return ann
        return None
    neighbors = unit_tokens[max(i1 - 1, 0) : i2 + 1]
    for ann in annotations:
        if any(ann.contains(t) for t in neighbors):
            return ann
    return None


def _replace_sig(a: _Side, b: _Side, i1: int, i2: int, j1: int, j2: int) -> Signature:
    old_toks, new_toks = a.tokens[i1:i2], b.tokens[j1:j2]
    key = "\x00".join([REPLACE, *(t.value for t in old_toks), "\x01", *(t.value for t in new_toks)])
    return Signature(
        REPLACE, key, render(old_toks, a.analysis.lines), render(new_toks, b.analysis.lines)
    )


def _template_sig(a: _Side, b: _Side, cluster_ops) -> Signature:
    """One signature for a cluster of ops. Unchanged code between the ops becomes a "…"
    placeholder when it holds names or literals, so ``role="admin"`` and ``role="faculty"``
    share the template ``role=… -> roles=(…,)``."""
    old: list[Token] = []
    new: list[Token] = []
    for n, (_, i1, i2, j1, j2) in enumerate(cluster_ops):
        if n:
            prev = cluster_ops[n - 1]
            gap_a, gap_b = a.tokens[prev[2] : i1], b.tokens[prev[4] : j1]
            if any(t.kind in _HOLE_KINDS for t in gap_a):
                old.append(_hole(gap_a))
                new.append(_hole(gap_b))
            else:
                old += gap_a
                new += gap_b
        old += a.tokens[i1:i2]
        new += b.tokens[j1:j2]
    key = "\x00".join([REPLACE, *(t.value for t in old), "\x01", *(t.value for t in new)])
    return Signature(REPLACE, key, render(old, a.analysis.lines), render(new, b.analysis.lines))


def _hole(tokens: list[Token]) -> Token:
    return Token(OTHER, "\x02", PLACEHOLDER, tokens[0].start, tokens[-1].end)


def render(tokens: list[Token], lines: list[str]) -> str:
    """Reconstruct readable source text for a token run (single line, truncated)."""
    out: list[str] = []
    prev: Token | None = None
    for t in tokens:
        if t.kind == STRUCTURAL:
            continue
        if prev is not None:
            if prev.end[0] == t.start[0] and t.start[0] - 1 < len(lines):
                out.append(lines[t.start[0] - 1][prev.end[1] : t.start[1]])
            else:
                out.append(" ")
        out.append(" ".join(t.text.split()) if "\n" in t.text else t.text)
        prev = t
    text = "".join(out).strip()
    if not text and tokens:
        text = "(indentation)"
    return text if len(text) <= MAX_LABEL else text[: MAX_LABEL - 1] + "…"


def _highlight(target: dict[int, list[list[int]]], tokens: list[Token], rng: LineRange) -> None:
    if rng is None:
        return
    first, last = rng
    for t in tokens:
        if t.kind == STRUCTURAL:
            continue
        for line in range(max(t.start[0], first), min(t.end[0], last) + 1):
            start = t.start[1] if line == t.start[0] else 0
            end = t.end[1] if line == t.end[0] else 10**6
            if end > start:
                target.setdefault(line, []).append([start, end])
