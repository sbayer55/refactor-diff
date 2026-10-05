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
    NAME,
    STRUCTURAL,
    Annotation,
    FileAnalysis,
    LanguageAnalyzer,
    Token,
)
from refactor_diff.model import FORMATTING, RENAME, REPLACE, RETYPE, Signature

MAX_LABEL = 160
_ANNOTATION_PUNCT = {":", "->"}

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
    for cluster in _clusters(a, ops, classified):
        if len(cluster) == 1 or all(classified[k] is not None for k in cluster):
            for k in cluster:
                sig = classified[k] or _replace_sig(a, b, *ops[k][1:])
                if sig.kind == REPLACE:
                    result.generic_tokens += ops[k][2] - ops[k][1] + ops[k][4] - ops[k][3]
                _add(result, sig)
        else:
            i1, j1 = ops[cluster[0]][1], ops[cluster[0]][3]
            i2, j2 = ops[cluster[-1]][2], ops[cluster[-1]][4]
            result.generic_tokens += i2 - i1 + j2 - j1
            _add(result, _replace_sig(a, b, i1, i2, j1, j2))
        for k in cluster:
            _, i1, i2, j1, j2 = ops[k]
            _highlight(result.old_hl, a.tokens[i1:i2], old_range)
            _highlight(result.new_hl, b.tokens[j1:j2], new_range)
    return result


def _add(result: Classification, sig: Signature) -> None:
    if all(s.key != sig.key for s in result.signatures):
        result.signatures.append(sig)


def _clusters(a: _Side, ops, classified) -> list[list[int]]:
    """Join ops separated only by a "." so ``cfg.get("x")`` -> ``settings.x`` reads as one
    replacement instead of a rename plus a fragment."""
    clusters: list[list[int]] = []
    for k, op in enumerate(ops):
        if clusters:
            prev = ops[clusters[-1][-1]]
            gap = a.tokens[prev[2] : op[1]]
            if (
                len(gap) == 1
                and gap[0].value == "."
                and (classified[k] is None or classified[clusters[-1][-1]] is None)
            ):
                clusters[-1].append(k)
                continue
        clusters.append([k])
    return clusters


def _classify_op(analyzer: LanguageAnalyzer, a: _Side, b: _Side, op) -> Signature | None:
    tag, i1, i2, j1, j2 = op
    old_toks, new_toks = a.tokens[i1:i2], b.tokens[j1:j2]
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
