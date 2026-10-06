"""Classify one change unit into mechanical edit signatures.

The old and new token streams of the unit are aligned with ``difflib``. Each differing
span becomes a signature:

* rename   - a single identifier swapped for another (``get_user`` -> ``fetch_user``)
* retype   - every changed token sits inside a type annotation
* replace  - any other token-level substitution, insertion or deletion
* formatting - token streams are identical (only whitespace/layout changed)
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from difflib import SequenceMatcher

from refactor_diff.hunks import Opcode
from refactor_diff.languages.base import (
    COMMENT,
    NAME,
    NUMBER,
    OP,
    OTHER,
    STRING,
    STRUCTURAL,
    Annotation,
    DefSite,
    FileAnalysis,
    LanguageAnalyzer,
    Token,
)
from refactor_diff.model import (
    ARGS,
    DOCS,
    FORMATTING,
    IMPORT,
    RENAME,
    REPLACE,
    RETYPE,
    ChangeUnit,
    Line,
    Signature,
    short_hash,
)

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
    old_anchor: int = 0,
    new_anchor: int = 0,
) -> Classification:
    """Classify the change between ``old_range`` and ``new_range``. For a one-sided unit the
    ``*_anchor`` is the 1-based line before the insertion/deletion point on the empty side."""
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
    imports = _import_classification(a, b, ops, old_range, new_range)
    if imports is not None:
        return imports
    classified = [_classify_op(analyzer, a, b, op) for op in ops]
    _classify_args(a, b, ops, classified, old_anchor, new_anchor)

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


def make_unit(
    path: str,
    hunk_id: str,
    op: Opcode,
    cls: Classification,
    old_lines: list[str],
    new_lines: list[str],
) -> ChangeUnit:
    old = [Line(old_lines[i], merge_ranges(cls.old_hl.get(i + 1, []))) for i in range(op.i1, op.i2)]
    new = [Line(new_lines[j], merge_ranges(cls.new_hl.get(j + 1, []))) for j in range(op.j1, op.j2)]
    return ChangeUnit(
        id=short_hash(path, op.i1, op.i2, op.j1, op.j2),
        path=path,
        hunk_id=hunk_id,
        old_start=op.i1 + 1,
        new_start=op.j1 + 1,
        old=old,
        new=new,
        signatures=cls.signatures,
    )


def merge_ranges(ranges: list[list[int]]) -> list[list[int]]:
    merged: list[list[int]] = []
    for start, end in sorted(ranges):
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    return merged


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
        # Only indentation / logical-line structure changed (e.g. a block moved into a class).
        return Signature(FORMATTING, FORMATTING, "", "")
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


# --- imports -------------------------------------------------------------------------------


def _import_classification(a: _Side, b: _Side, ops, old_range, new_range) -> Classification | None:
    """A unit made only of import statements: classify it by what the imports bind.

    * the same names from a different module -> ``import`` (``a`` -> ``b``), keyed on the
      module change so the same move/rename of a module groups across files;
    * a name added or removed -> ``import`` keyed on the binding;
    * the same module importing a differently named thing -> a ``rename`` with the ``import``
      context, the same signature the token classifier would produce.
    """
    old_sites = _import_sites(a, old_range)
    new_sites = _import_sites(b, new_range)
    if old_sites is None or new_sites is None:
        return None
    old_b = {x for s in old_sites for x in s.bindings}
    new_b = {x for s in new_sites for x in s.bindings}
    removed, added = old_b - new_b, new_b - old_b
    if not removed and not added:
        return None
    sigs: list[Signature] = []
    unpaired = []
    for r in sorted(removed, key=lambda x: (x.alias, x.module)):
        same = [x for x in added if (x.name, x.alias) == (r.name, r.alias)]
        if same:
            n = same[0]
            added.discard(n)
            sigs.append(
                Signature(IMPORT, f"{IMPORT}\0{r.module}\0{n.module}", r.module, n.module, r.alias)
            )
            continue
        renamed = [
            x
            for x in added
            if x.module == r.module
            and x.name
            and r.name
            and x.alias == x.name
            and r.alias == r.name
        ]
        if renamed:
            n = renamed[0]
            added.discard(n)
            key = f"{RENAME}\0{r.name}\0{n.name}"
            sigs.append(Signature(RENAME, key, r.name, n.name, "import"))
            continue
        unpaired.append(r)
    if unpaired and added:
        return None  # something else was rewritten: leave it to the token classifier
    for r in unpaired:
        sigs.append(Signature(IMPORT, f"{IMPORT}\0{_dotted(r)}\0", _import_text(r), "", r.alias))
    for n in sorted(added, key=lambda x: (x.alias, x.module)):
        sigs.append(Signature(IMPORT, f"{IMPORT}\0\0{_dotted(n)}", "", _import_text(n), n.alias))
    result = Classification([])
    for s in sigs:
        _add(result, s)
    for _, i1, i2, j1, j2 in ops:
        _highlight(result.old_hl, a.tokens[i1:i2], old_range)
        _highlight(result.new_hl, b.tokens[j1:j2], new_range)
    return result


def _import_sites(side: _Side, rng: LineRange) -> list | None:
    """The import statements overlapping the range, or None if the range holds anything
    else (an empty side counts as all imports)."""
    if rng is None:
        return []
    first, last = rng
    sites = [s for s in side.analysis.imports if s.start <= last and s.end >= first]
    for t in side.tokens:
        if t.kind in (STRUCTURAL, COMMENT):
            continue
        if not any(s.start <= t.start[0] <= s.end for s in sites):
            return None
    return sites


def _dotted(binding) -> str:
    return f"{binding.module}.{binding.name}" if binding.name else binding.module


def _import_text(binding) -> str:
    if binding.name is None:
        text = f"import {binding.module}"
    else:
        text = f"from {binding.module} import {binding.name}"
    alias = binding.name or binding.module.split(".")[0]
    return text if binding.alias == alias else f"{text} as {binding.alias}"


# --- argument lists -------------------------------------------------------------------------

_STAR = {"*": "+*", "**": "+**"}


@dataclass(frozen=True)
class _ArgItem:
    start: tuple[int, int]
    end: tuple[int, int]
    name: str | None  # keyword / parameter name
    kind: str  # "pos" | "kw" | "*" | "**"


def _site_args(site) -> list[_ArgItem]:
    if isinstance(site, DefSite):
        return [
            _ArgItem(
                p.start,
                p.end,
                p.name,
                "*"
                if p.kind == "vararg"
                else "**"
                if p.kind == "kwarg"
                else "kw"
                if p.has_default or p.kind == "kwonly"
                else "pos",
            )
            for p in site.params
        ]
    return [
        _ArgItem(
            s,
            e,
            kw if kw not in (None, "*", "**") else None,
            "pos" if kw is None else "kw" if kw not in ("*", "**") else kw,
        )
        for s, e, kw in site.args
    ]


def _arg_span(site) -> tuple[tuple[int, int], tuple[int, int]]:
    if isinstance(site, DefSite):
        return site.params_start, site.params_end
    return site.args_start, site.args_end


def _classify_args(a: _Side, b: _Side, ops, classified, old_anchor: int, new_anchor: int) -> None:
    """Explain the still-unclassified ops that only add, remove or convert arguments of one
    call (or parameters of one def) with an ``args`` signature keyed on the callee and the
    shape change, so ``fetch(x, y)`` -> ``fetch(x, y, timeout=5)`` groups with every other
    call that gained ``timeout`` whatever its value, and with the def that gained the
    parameter."""
    pending = [k for k, c in enumerate(classified) if c is None]
    if not pending:
        return
    by_pair: dict[tuple, list[int]] = {}
    for k in pending:
        _, i1, i2, j1, j2 = ops[k]
        new_site = _site_for(b, j1, j2, None, new_anchor)
        old_site = _site_for(a, i1, i2, new_site, old_anchor)
        if new_site is None and old_site is not None:
            new_site = _site_for(b, j1, j2, old_site, new_anchor)
        if old_site is None or new_site is None:
            continue
        if old_site.short_name != new_site.short_name or type(old_site) is not type(new_site):
            continue
        by_pair.setdefault((old_site, new_site), []).append(k)
    for (old_site, new_site), ks in by_pair.items():
        sig = _args_sig(a, b, ops, ks, old_site, new_site)
        if sig is not None:
            for k in ks:
                classified[k] = sig


def _site_for(side: _Side, lo: int, hi: int, other, anchor: int):
    """The innermost call/def whose argument list holds the unit tokens ``lo:hi``. For an
    empty token run, the one around the insertion point; with no tokens at all on this side,
    the one on the anchor line named like ``other``."""
    sites = [*side.analysis.calls, *side.analysis.defs]
    toks = side.tokens[lo:hi]
    if toks:
        here = [
            s
            for s in sites
            if all(_arg_span(s)[0] <= t.start and t.end <= _arg_span(s)[1] for t in toks)
        ]
    elif side.tokens:
        p = side.tokens[lo - 1].end if lo > 0 else side.tokens[min(lo, len(side.tokens) - 1)].start
        here = [s for s in sites if _arg_span(s)[0] <= p <= _arg_span(s)[1]]
        if other is not None:
            here = [s for s in here if s.short_name == other.short_name]
    else:
        if other is None:
            return None
        here = [
            s
            for s in sites
            if s.short_name == other.short_name and s.start[0] <= anchor + 1 and s.end[0] >= anchor
        ]
    if not here:
        return None
    return min(here, key=lambda s: (s.end[0] - s.start[0], s.end[1] - s.start[1]))


def _args_sig(a: _Side, b: _Side, ops, ks, old_site, new_site) -> Signature | None:
    cov_old: set[int] = set()
    cov_new: set[int] = set()
    for k in ks:
        _, i1, i2, j1, j2 = ops[k]
        cov_old.update(range(a.offset + i1, a.offset + i2))
        cov_new.update(range(b.offset + j1, b.offset + j2))

    delta: list[str] = []
    conversions: list[str] = []

    def scan(side: _Side, site, cov: set[int], sign: str) -> set[int] | None:
        allowed: set[int] = set()
        toks = side.analysis.tokens
        lo, hi = _arg_span(site)
        for item in _site_args(site):
            idx = [
                i
                for i, t in enumerate(toks)
                if t.kind != STRUCTURAL and item.start <= t.start and t.end <= item.end
            ]
            hit = [i for i in idx if i in cov]
            if not hit:
                continue
            if len(hit) == len(idx):
                allowed.update(idx)
                if item.kind in _STAR:
                    delta.append(sign + item.kind)
                elif item.kind == "kw":
                    delta.append(f"{sign}kw:{item.name}")
                else:
                    delta.append(f"{sign}pos")
            elif (
                sign == "+"
                and item.kind == "kw"
                and len(hit) == 2
                and hit == idx[:2]
                and toks[idx[1]].value == "="
            ):
                allowed.update(hit)
                conversions.append(item.name)
            else:
                return None
        for i in cov:
            if i in allowed:
                continue
            t = toks[i]
            if t.kind == OP and t.value == "," and lo <= t.start and t.end <= hi:
                allowed.add(i)
            elif t.kind == STRUCTURAL:
                allowed.add(i)
            else:
                return None
        return allowed

    if scan(a, old_site, cov_old, "-") is None or scan(b, new_site, cov_new, "+") is None:
        return None
    old_pos = sum(1 for x in _site_args(old_site) if x.kind == "pos")
    new_pos = sum(1 for x in _site_args(new_site) if x.kind == "pos")
    if conversions:
        if len(conversions) != 1 or old_pos - new_pos != 1 or "-pos" in delta:
            return None
        delta.append(f"pos>kw:{conversions[0]}")
    if not delta or ("+pos" in delta and "-pos" in delta):
        return None  # a positional argument replaced by another is a value edit

    counts = Counter(delta)
    parts = sorted(f"{d}:{n}" if d in ("+pos", "-pos") and n > 1 else d for d, n in counts.items())
    name = old_site.short_name
    is_def = isinstance(old_site, DefSite)
    key = "\0".join([ARGS, name, ",".join(parts)])

    def render_side(sign: str) -> str:
        extra = []
        for d in parts:
            if d.startswith(sign + "kw:"):
                extra.append(f"{d[4:]}=…")
            elif d.startswith(sign + "pos"):
                extra += ["…"] * (int(d.split(":")[1]) if ":" in d else 1)
            elif d in (sign + "*", sign + "**"):
                extra.append(d[1:] + "…")
            elif d.startswith("pos>kw:"):
                extra.append("…" if sign == "-" else f"{d[7:]}=…")
        inner = ", ".join(["…", *extra])
        return f"{'def ' if is_def else ''}{name}({inner})"

    return Signature(
        ARGS, key, render_side("-"), render_side("+"), "definition" if is_def else "call"
    )
