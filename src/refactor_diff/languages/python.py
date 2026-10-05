"""Python analyzer built on the stdlib ``tokenize`` and ``ast`` modules."""

from __future__ import annotations

import ast
import io
import keyword
import re
import tokenize

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
    Token,
    split_lines,
)

_STRUCTURAL = {
    tokenize.INDENT: "<INDENT>",
    tokenize.DEDENT: "<DEDENT>",
    tokenize.NEWLINE: "<NEWLINE>",
}
_SKIP = {tokenize.NL, tokenize.ENCODING, tokenize.ENDMARKER}

# Rough tokenizer for files that do not tokenize cleanly (merge markers, syntax errors).
_FALLBACK_RE = re.compile(
    r"""
    (?P<comment>\#[^\n]*)
  | (?P<string>[rbfuRBFU]{0,2}
        (?:'''[\s\S]*?'''|\"\"\"[\s\S]*?\"\"\"|'(?:\\.|[^'\\\n])*'|"(?:\\.|[^"\\\n])*"))
  | (?P<name>[^\W\d]\w*)
  | (?P<number>\d[\w.]*)
  | (?P<op>\*\*=?|//=?|->|:=|<<=?|>>=?|[-+*/%@&|^=<>!]=|[^\s\w])
  | (?P<ws>\s+)
    """,
    re.VERBOSE,
)


class PythonAnalyzer:
    name = "python"

    def handles(self, path: str) -> bool:
        return path.endswith((".py", ".pyi"))

    def is_keyword(self, value: str) -> bool:
        return keyword.iskeyword(value) or keyword.issoftkeyword(value)

    def import_keywords(self) -> frozenset[str]:
        return frozenset({"import", "from"})

    def definition_keywords(self) -> frozenset[str]:
        return frozenset({"def", "class"})

    def analyze(self, text: str) -> FileAnalysis:
        lines = split_lines(text)
        try:
            tokens = _tokenize(text)
            parsed = True
        except (tokenize.TokenError, IndentationError, SyntaxError):
            tokens = _fallback_tokenize(text)
            parsed = False
        try:
            annotations = _annotations(ast.parse(text), lines)
        except (SyntaxError, ValueError):
            annotations = []
            parsed = False
        return FileAnalysis(lines=lines, tokens=tokens, annotations=annotations, parsed=parsed)


def _tokenize(text: str) -> list[Token]:
    out: list[Token] = []
    for tok in tokenize.generate_tokens(io.StringIO(text).readline):
        if tok.type in _SKIP:
            continue
        if tok.type in _STRUCTURAL:
            out.append(Token(STRUCTURAL, _STRUCTURAL[tok.type], "", tok.start, tok.start))
            continue
        out.append(
            Token(
                _kind(tok.type, tok.string),
                _normalize(tok.type, tok.string),
                tok.string,
                tok.start,
                tok.end,
            )
        )
    return out


def _kind(tok_type: int, string: str) -> str:
    if tok_type == tokenize.NAME:
        return NAME
    if tok_type == tokenize.OP:
        return OP
    if tok_type == tokenize.STRING:
        return STRING
    if tok_type == tokenize.NUMBER:
        return NUMBER
    if tok_type == tokenize.COMMENT:
        return COMMENT
    return OTHER


def _normalize(tok_type: int, string: str) -> str:
    if tok_type == tokenize.STRING:
        # Treat 'x' and "x" as the same token so quote-style churn reads as formatting.
        try:
            return repr(ast.literal_eval(string))
        except (ValueError, SyntaxError):
            return string
    if tok_type == tokenize.COMMENT:
        return string.rstrip()
    return string


def _fallback_tokenize(text: str) -> list[Token]:
    out: list[Token] = []
    line_starts = [0]
    for i, ch in enumerate(text):
        if ch == "\n":
            line_starts.append(i + 1)

    def pos(offset: int) -> tuple[int, int]:
        lo, hi = 0, len(line_starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if line_starts[mid] <= offset:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1, offset - line_starts[lo]

    for m in _FALLBACK_RE.finditer(text):
        kind = m.lastgroup
        if kind == "ws" or kind is None:
            continue
        s = m.group()
        mapped = {"comment": COMMENT, "string": STRING, "name": NAME, "number": NUMBER}.get(
            kind, OP
        )
        out.append(
            Token(mapped, s.rstrip() if mapped == COMMENT else s, s, pos(m.start()), pos(m.end()))
        )
    return out


def _annotations(tree: ast.AST, lines: list[str]) -> list[Annotation]:
    found: list[Annotation] = []

    def add(node: ast.expr | None, target: str) -> None:
        if node is None or getattr(node, "end_lineno", None) is None:
            return
        start = (node.lineno, _char_col(lines, node.lineno, node.col_offset))
        end = (node.end_lineno, _char_col(lines, node.end_lineno, node.end_col_offset))
        found.append(Annotation(start, end, ast.unparse(node), target))

    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            a = node.args
            for arg in [*a.posonlyargs, *a.args, *a.kwonlyargs, a.vararg, a.kwarg]:
                if arg is not None:
                    add(arg.annotation, f"param {arg.arg}")
            add(node.returns, f"return of {node.name}")
        elif isinstance(node, ast.AnnAssign):
            add(node.annotation, f"variable {ast.unparse(node.target)}")
    return found


def _char_col(lines: list[str], lineno: int, byte_col: int) -> int:
    """ast reports UTF-8 byte offsets; tokens use character offsets."""
    if lineno - 1 >= len(lines):
        return byte_col
    line = lines[lineno - 1]
    return len(line.encode("utf-8")[:byte_col].decode("utf-8", errors="replace"))
