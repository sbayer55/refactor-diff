"""Python analyzer built on the stdlib ``tokenize`` and ``ast`` modules."""

from __future__ import annotations

import ast
import builtins
import copy
import io
import keyword
import re
import textwrap
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
    Arg,
    Binding,
    CallSite,
    DefSite,
    FileAnalysis,
    ImportSite,
    Param,
    Pos,
    StmtSpan,
    Token,
    split_lines,
)

_STRUCTURAL = {
    tokenize.INDENT: "<INDENT>",
    tokenize.DEDENT: "<DEDENT>",
    tokenize.NEWLINE: "<NEWLINE>",
}
_BUILTINS = frozenset(dir(builtins))
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

    def is_builtin(self, value: str) -> bool:
        return value in _BUILTINS

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
            tree = ast.parse(text)
        except (SyntaxError, ValueError):
            return FileAnalysis(lines=lines, tokens=tokens, parsed=False)
        pos = _Positions(lines)
        return FileAnalysis(
            lines=lines,
            tokens=tokens,
            annotations=_annotations(tree, pos),
            docstrings=_docstrings(tree, pos),
            parsed=parsed,
            tree=tree,
            statements=_statements(tree.body, ""),
            calls=_calls(tree, pos),
            defs=_defs(tree, tokens, pos),
            imports=_imports(tree),
        )

    def parse_block(self, text: str) -> ast.AST | None:
        try:
            return ast.parse(textwrap.dedent(text))
        except (SyntaxError, ValueError):
            return None

    def normalized_dump(
        self, node: object, renames: dict[str, str], strip_annotations: bool
    ) -> str:
        assert isinstance(node, ast.AST)
        tree = _Normalize(renames, strip_annotations).visit(copy.deepcopy(node))
        return ast.dump(tree, include_attributes=False)


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


class _Positions:
    """Converts ast node positions (UTF-8 byte columns) to token positions (characters)."""

    def __init__(self, lines: list[str]):
        self.lines = lines

    def start(self, node: ast.AST) -> Pos:
        return (node.lineno, _char_col(self.lines, node.lineno, node.col_offset))

    def end(self, node: ast.AST) -> Pos:
        return (node.end_lineno, _char_col(self.lines, node.end_lineno, node.end_col_offset))

    def located(self, node: ast.AST | None) -> bool:
        return node is not None and getattr(node, "end_lineno", None) is not None


def _annotations(tree: ast.AST, pos: _Positions) -> list[Annotation]:
    found: list[Annotation] = []

    def add(node: ast.expr | None, target: str) -> None:
        if pos.located(node):
            found.append(Annotation(pos.start(node), pos.end(node), ast.unparse(node), target))

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


def _is_docstring(node: ast.AST) -> bool:
    return (
        isinstance(node, ast.Expr)
        and isinstance(node.value, ast.Constant)
        and isinstance(node.value.value, str)
    )


def _docstrings(tree: ast.AST, pos: _Positions) -> list[tuple[Pos, Pos]]:
    """Spans of bare string statements: module/class/function docstrings and the
    attribute docstrings that follow assignments."""
    return [
        (pos.start(node), pos.end(node))
        for node in ast.walk(tree)
        if _is_docstring(node) and pos.located(node)
    ]


def _statements(body: list[ast.stmt], prefix: str) -> list[StmtSpan]:
    """Spans of the statements in ``body``; class bodies recurse one level so that methods
    are spans of their own."""
    spans = []
    for node in body:
        if getattr(node, "end_lineno", None) is None:
            continue
        decorators = getattr(node, "decorator_list", [])
        start = min([node.lineno, *(d.lineno for d in decorators)])
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            spans.append(StmtSpan(start, node.end_lineno, "def", prefix + node.name, node))
        elif isinstance(node, ast.ClassDef):
            qual = prefix + node.name
            children = tuple(_statements(node.body, qual + "."))
            spans.append(StmtSpan(start, node.end_lineno, "class", qual, node, children))
        else:
            spans.append(StmtSpan(start, node.end_lineno, "stmt", "", node))
    return spans


def _calls(tree: ast.AST, pos: _Positions) -> list[CallSite]:
    sites = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not pos.located(node):
            continue
        if not isinstance(node.func, (ast.Name, ast.Attribute)):
            continue
        args: list[Arg] = []
        for a in node.args:
            if isinstance(a, ast.Starred):
                args.append((pos.start(a), pos.end(a), "*"))
            else:
                args.append((pos.start(a), pos.end(a), None))
        for kw in node.keywords:
            if not pos.located(kw):
                continue
            args.append((pos.start(kw), pos.end(kw), kw.arg if kw.arg is not None else "**"))
        args.sort()
        sites.append(
            CallSite(
                name=ast.unparse(node.func),
                start=pos.start(node),
                end=pos.end(node),
                args_start=pos.end(node.func),
                args_end=pos.end(node),
                args=tuple(args),
            )
        )
    sites.sort(key=lambda s: (s.start, s.end))
    return sites


def _defs(tree: ast.AST, tokens: list[Token], pos: _Positions) -> list[DefSite]:
    sites = []
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) or not pos.located(node):
            continue
        span = _params_span(node, tokens)
        if span is None:
            continue
        a = node.args
        params: list[Param] = []
        n_pos = len(a.posonlyargs) + len(a.args)
        defaults = [None] * (n_pos - len(a.defaults)) + list(a.defaults)
        for arg, default, kind in [
            *((x, d, "posonly") for x, d in zip(a.posonlyargs, defaults, strict=False)),
            *((x, d, "pos") for x, d in zip(a.args, defaults[len(a.posonlyargs) :], strict=False)),
            *([(a.vararg, None, "vararg")] if a.vararg else []),
            *((x, d, "kwonly") for x, d in zip(a.kwonlyargs, a.kw_defaults, strict=False)),
            *([(a.kwarg, None, "kwarg")] if a.kwarg else []),
        ]:
            if not pos.located(arg):
                continue
            end = pos.end(default) if default is not None and pos.located(default) else pos.end(arg)
            params.append(Param(arg.arg, kind, default is not None, pos.start(arg), end))
        params.sort(key=lambda p: p.start)
        sites.append(DefSite(node.name, pos.start(node), pos.end(node), *span, tuple(params)))
    sites.sort(key=lambda s: (s.start, s.end))
    return sites


def _params_span(node: ast.AST, tokens: list[Token]) -> tuple[Pos, Pos] | None:
    """Positions of the ``(`` and the matching ``)`` of a def's parameter list (the list has
    no AST node of its own)."""
    k = next(
        (
            i
            for i, t in enumerate(tokens)
            if t.kind == NAME and t.value == node.name and t.start >= (node.lineno, 0)
        ),
        None,
    )
    if k is None or k + 1 >= len(tokens) or tokens[k + 1].value != "(":
        return None
    depth = 0
    for t in tokens[k + 1 :]:
        if t.kind != OP:
            continue
        if t.value in "([{":
            depth += 1
        elif t.value in ")]}":
            depth -= 1
            if depth == 0:
                return tokens[k + 1].start, t.end
    return None


def _imports(tree: ast.AST) -> list[ImportSite]:
    sites = []
    for node in ast.walk(tree):
        if getattr(node, "end_lineno", None) is None:
            continue
        if isinstance(node, ast.Import):
            bindings = tuple(
                Binding(a.name, None, a.asname or a.name.split(".")[0]) for a in node.names
            )
        elif isinstance(node, ast.ImportFrom):
            module = "." * node.level + (node.module or "")
            bindings = tuple(
                Binding(module, a.name, a.asname or a.name, node.level) for a in node.names
            )
        else:
            continue
        sites.append(ImportSite(node.lineno, node.end_lineno, bindings))
    sites.sort(key=lambda s: s.start)
    return sites


class _Normalize(ast.NodeTransformer):
    """Rewrite a tree so that two pieces of code that differ only in docstrings, in names
    listed in ``renames`` and (optionally) in type annotations dump identically."""

    def __init__(self, renames: dict[str, str], strip_annotations: bool):
        self.renames = renames
        self.strip = strip_annotations

    def _name(self, name: str | None) -> str | None:
        return self.renames.get(name, name) if name is not None else None

    def _body(self, body: list[ast.stmt]) -> list[ast.stmt]:
        if body and _is_docstring(body[0]):
            body = body[1:]
        return body or [ast.Pass()]

    def generic_visit(self, node: ast.AST) -> ast.AST:
        if hasattr(node, "type_comment"):
            node.type_comment = None
        node = super().generic_visit(node)
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            node.body = self._body(node.body)
        return node

    def visit_Name(self, node: ast.Name) -> ast.AST:
        node.id = self._name(node.id)
        return self.generic_visit(node)

    def visit_Attribute(self, node: ast.Attribute) -> ast.AST:
        node.attr = self._name(node.attr)
        return self.generic_visit(node)

    def visit_arg(self, node: ast.arg) -> ast.AST:
        node.arg = self._name(node.arg)
        if self.strip:
            node.annotation = None
        return self.generic_visit(node)

    def visit_keyword(self, node: ast.keyword) -> ast.AST:
        node.arg = self._name(node.arg)
        return self.generic_visit(node)

    def _visit_def(self, node):
        node.name = self._name(node.name)
        if self.strip:
            node.returns = None
        return self.generic_visit(node)

    visit_FunctionDef = visit_AsyncFunctionDef = visit_ClassDef = _visit_def

    def visit_alias(self, node: ast.alias) -> ast.AST:
        node.name = ".".join(self._name(p) for p in node.name.split("."))
        node.asname = self._name(node.asname)
        return node

    def visit_Global(self, node: ast.Global) -> ast.AST:
        node.names = [self._name(n) for n in node.names]
        return node

    visit_Nonlocal = visit_Global

    def visit_ExceptHandler(self, node: ast.ExceptHandler) -> ast.AST:
        node.name = self._name(node.name)
        return self.generic_visit(node)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> ast.AST | None:
        if not self.strip:
            return self.generic_visit(node)
        if node.value is None:
            return None
        return self.generic_visit(ast.Assign(targets=[node.target], value=node.value))


def _char_col(lines: list[str], lineno: int, byte_col: int) -> int:
    """ast reports UTF-8 byte offsets; tokens use character offsets."""
    if lineno - 1 >= len(lines):
        return byte_col
    line = lines[lineno - 1]
    return len(line.encode("utf-8")[:byte_col].decode("utf-8", errors="replace"))
