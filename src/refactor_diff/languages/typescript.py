"""TypeScript and JavaScript analyzers built on tree-sitter.

One analyzer per grammar (TypeScript, TSX, JavaScript); they share keywords, builtins and the
pathspecs used to look for missed renames, so a renamed TS export is also searched for in JS.
"""

from __future__ import annotations

import re

import tree_sitter_javascript
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser

from refactor_diff.languages.base import (
    COMMENT,
    NAME,
    NUMBER,
    OP,
    STRING,
    STRUCTURAL,
    Annotation,
    Binding,
    FileAnalysis,
    ImportSite,
    Pos,
    StmtSpan,
    Token,
    char_col,
    split_lines,
)

TS_SUFFIXES = (".ts", ".mts", ".cts")
TSX_SUFFIXES = (".tsx",)
JS_SUFFIXES = (".js", ".jsx", ".mjs", ".cjs")
GLOBS = tuple(f"*{s}" for s in (*TS_SUFFIXES, *TSX_SUFFIXES, *JS_SUFFIXES))

KEYWORDS = frozenset(
    """
    abstract any as asserts async await bigint boolean break case catch class const constructor
    continue debugger declare default delete do else enum export extends false finally for from
    function get if implements import in infer instanceof interface is keyof let module namespace
    never new null number object of override private protected public readonly require return
    satisfies set static string super switch symbol this throw true try type typeof undefined
    unique unknown var void while with yield
    """.split()
)

_BUILTINS = frozenset(
    """
    Array ArrayBuffer AsyncGenerator BigInt Boolean DataView Date Error EvalError Float32Array
    Float64Array Function Generator Infinity Int8Array Int16Array Int32Array Intl JSON Map Math
    NaN Number Object Promise Proxy RangeError ReferenceError Reflect RegExp Set String Symbol
    SyntaxError TypeError URIError Uint8Array Uint8ClampedArray Uint16Array Uint32Array WeakMap
    WeakRef WeakSet globalThis console decodeURI decodeURIComponent encodeURI encodeURIComponent
    eval isFinite isNaN parseFloat parseInt queueMicrotask setInterval setTimeout clearInterval
    clearTimeout structuredClone fetch URL URLSearchParams window document process module exports
    require __dirname __filename
    Awaited Capitalize ConstructorParameters Exclude Extract InstanceType Lowercase NonNullable
    Omit OmitThisParameter Parameters Partial Pick Readonly ReadonlyArray Record Required
    ReturnType ThisParameterType ThisType Uncapitalize Uppercase PromiseLike Iterable Iterator
    AsyncIterable IterableIterator
    """.split()
)

# Nodes tokenized whole: their insides are text, not code.
_LEAVES = {"string", "regex", "comment", "html_comment", "hash_bang_line", "jsx_text"}
# Nodes whose children are statements or members: each child ends a logical line.
_BODIES = {
    "program",
    "statement_block",
    "class_body",
    "interface_body",
    "object_type",
    "enum_body",
    "switch_body",
    "switch_case",
    "switch_default",
}
_FUNCTIONS = {
    "function_declaration",
    "function_expression",
    "function_signature",
    "generator_function_declaration",
    "method_definition",
    "method_signature",
    "abstract_method_signature",
}
_FUNCTION_DECLS = {"function_declaration", "generator_function_declaration"}
_FUNCTION_VALUES = {"arrow_function", "function_expression", "function", "generator_function"}
_CLASS_DECLS = {"class_declaration", "abstract_class_declaration"}
_TYPE_DECLS = {"interface_declaration", "type_alias_declaration", "enum_declaration"}
_IDENT_RE = re.compile(r"[A-Za-z_$][\w$]*\Z")
_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "b": "\b", "f": "\f", "v": "\v"}
_ESCAPE_RE = re.compile(r"\\(u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|x[0-9a-fA-F]{2}|.)", re.S)
_NEWLINE = "<NEWLINE>"


class TypeScriptAnalyzer:
    globs = GLOBS

    def __init__(self, name: str, language: object, suffixes: tuple[str, ...]):
        self.name = name
        self._parser = Parser(Language(language))
        self._suffixes = suffixes

    def handles(self, path: str) -> bool:
        return path.endswith(self._suffixes)

    def is_keyword(self, value: str) -> bool:
        return value in KEYWORDS

    def is_builtin(self, value: str) -> bool:
        return value in _BUILTINS

    def import_keywords(self) -> frozenset[str]:
        return frozenset({"import"})

    def definition_keywords(self) -> frozenset[str]:
        return frozenset(
            {"function", "class", "interface", "type", "enum", "namespace", "const", "let", "var"}
        )

    def analyze(self, text: str) -> FileAnalysis:
        lines = split_lines(text)
        tree = self._parser.parse(text.encode("utf-8"))
        walker = _Walker(lines)
        walker.visit(tree.root_node)
        return FileAnalysis(
            lines=lines,
            tokens=walker.tokens,
            annotations=walker.annotations,
            parsed=not tree.root_node.has_error,
            statements=_statements(tree.root_node),
            imports=_imports(tree.root_node),
        )


def analyzers() -> list[TypeScriptAnalyzer]:
    return [
        TypeScriptAnalyzer("typescript", tree_sitter_typescript.language_typescript(), TS_SUFFIXES),
        TypeScriptAnalyzer("tsx", tree_sitter_typescript.language_tsx(), TSX_SUFFIXES),
        TypeScriptAnalyzer("javascript", tree_sitter_javascript.language(), JS_SUFFIXES),
    ]


class _Walker:
    def __init__(self, lines: list[str]):
        self.lines = lines
        self.tokens: list[Token] = []
        self.annotations: list[Annotation] = []

    def pos(self, point) -> Pos:
        row, col = point
        return row + 1, char_col(self.lines, row + 1, col)

    def visit(self, node: Node) -> None:
        if node.type == "type_annotation":
            self._annotation(node)
        if node.child_count == 0 or node.type in _LEAVES or _plain_template(node):
            self._leaf(node)
            return
        body = node.type in _BODIES
        for child in node.children:
            self.visit(child)
            if body and child.is_named:
                self._end_statement(child)

    def _end_statement(self, child: Node) -> None:
        """Mark a logical line end. A terminating ``;`` folds into it, so adding or dropping
        optional semicolons reads as formatting."""
        end = self.pos(child.end_point)
        last = self.tokens[-1] if self.tokens else None
        if last is not None and last.kind == OP and last.value == ";" and last.end == end:
            self.tokens.pop()
            last = self.tokens[-1] if self.tokens else None
        if last is not None and last.kind != STRUCTURAL:
            self.tokens.append(Token(STRUCTURAL, _NEWLINE, "", end, end))

    def _leaf(self, node: Node) -> None:
        text = node.text.decode("utf-8", errors="replace") if node.text else ""
        if not text or node.is_missing:
            return
        if node.type == ";" and node.parent is not None and node.parent.type in _BODIES:
            return  # an empty statement or a class member separator
        kind = _kind(node, text)
        if kind == COMMENT:
            value = text.rstrip()
        elif kind == STRING:
            value = _normalize_string(node.type, text)
        else:
            value = text
        self.tokens.append(
            Token(kind, value, text, self.pos(node.start_point), self.pos(node.end_point))
        )

    def _annotation(self, node: Node) -> None:
        types = [c for c in node.named_children if c.type != "comment"]
        target = _annotation_target(node)
        if not types or target is None:
            return
        start, end = types[0], types[-1]
        source = node.text[start.start_byte - node.start_byte : end.end_byte - node.start_byte]
        text = " ".join(source.decode("utf-8", errors="replace").split())
        self.annotations.append(
            Annotation(self.pos(start.start_point), self.pos(end.end_point), text, target)
        )


def _kind(node: Node, text: str) -> str:
    t = node.type
    if t in ("comment", "html_comment", "hash_bang_line"):
        return COMMENT
    if t in ("string", "template_string", "regex", "string_fragment", "jsx_text"):
        return STRING
    if t == "number" and node.is_named:
        return NUMBER
    if _IDENT_RE.match(text):
        return NAME
    return OP


def _plain_template(node: Node) -> bool:
    """A template literal without ``${}`` is just a string."""
    return node.type == "template_string" and not any(
        c.type == "template_substitution" for c in node.children
    )


def _normalize_string(node_type: str, text: str) -> str:
    """Treat 'x', "x" and `x` as the same token so quote-style churn reads as formatting."""
    if node_type not in ("string", "template_string") or len(text) < 2:
        return text
    body = text[1:-1]
    if node_type == "template_string":
        body = body.replace("\\`", "`")

    def unescape(m: re.Match) -> str:
        esc = m.group(1)
        if esc.startswith("u{"):
            return chr(int(esc[2:-1], 16))
        if esc[0] in "ux" and len(esc) > 1:
            return chr(int(esc[1:], 16))
        return _ESCAPES.get(esc, "" if esc == "\n" else esc)

    try:
        return repr(_ESCAPE_RE.sub(unescape, body))
    except (ValueError, OverflowError):
        return text


def _annotation_target(node: Node) -> str | None:
    parent = node.parent
    if parent is None:
        return None
    name = parent.child_by_field_name("name")
    label = name.text.decode("utf-8", errors="replace") if name is not None and name.text else "?"
    if parent.type in ("required_parameter", "optional_parameter"):
        pattern = parent.child_by_field_name("pattern")
        if pattern is not None and pattern.text:
            label = pattern.text.decode("utf-8", errors="replace")
        return f"param {label}"
    if parent.child_by_field_name("return_type") == node:
        if parent.type == "arrow_function":
            holder = parent.parent
            if holder is not None and holder.type == "variable_declarator":
                held = holder.child_by_field_name("name")
                if held is not None and held.text:
                    return f"return of {held.text.decode('utf-8', errors='replace')}"
            return "return of arrow function"
        if parent.type in _FUNCTIONS:
            return f"return of {label}"
        return None
    if parent.type == "variable_declarator":
        return f"variable {label}"
    if parent.type in ("public_field_definition", "property_signature"):
        return f"property {label}"
    return None


# --- statements and imports ------------------------------------------------------------------


def _text(node: Node | None) -> str:
    return node.text.decode("utf-8", errors="replace") if node is not None and node.text else ""


def _statements(root: Node) -> list[StmtSpan]:
    """Top-level statements; class bodies recurse one level so that methods are spans of
    their own. A statement's span includes its ``export`` and decorators."""
    return [_span(c, c, "") for c in root.named_children if c.type != "comment"]


def _span(outer: Node, node: Node, prefix: str, start: int | None = None) -> StmtSpan:
    first = outer.start_point[0] + 1 if start is None else start
    last = outer.end_point[0] + 1
    if node.type == "export_statement":
        inner = node.child_by_field_name("declaration")
        if inner is not None:
            return _span(outer, inner, prefix, first)
        return StmtSpan(first, last, "stmt", "", outer)
    name = _text(node.child_by_field_name("name"))
    if node.type in _FUNCTION_DECLS and name:
        return StmtSpan(first, last, "def", prefix + name, outer)
    if node.type in _CLASS_DECLS and name:
        qual = prefix + name
        body = node.child_by_field_name("body")
        children = tuple(_members(body, qual + ".")) if body is not None else ()
        return StmtSpan(first, last, "class", qual, outer, children)
    if node.type in _TYPE_DECLS and name:
        return StmtSpan(first, last, "class", prefix + name, outer)
    if node.type in ("lexical_declaration", "variable_declaration"):
        decls = [c for c in node.named_children if c.type == "variable_declarator"]
        if len(decls) == 1:
            value = decls[0].child_by_field_name("value")
            held = decls[0].child_by_field_name("name")
            if value is not None and value.type in _FUNCTION_VALUES and held.type == "identifier":
                return StmtSpan(first, last, "def", prefix + _text(held), outer)
    return StmtSpan(first, last, "stmt", "", outer)


def _members(body: Node, prefix: str) -> list[StmtSpan]:
    """Class members; a member's decorators (siblings before it) belong to its span."""
    spans = []
    decorated: int | None = None
    for c in body.named_children:
        if c.type == "comment":
            continue
        if c.type == "decorator":
            if decorated is None:
                decorated = c.start_point[0] + 1
            continue
        start = decorated if decorated is not None else c.start_point[0] + 1
        decorated = None
        name = _text(c.child_by_field_name("name"))
        if c.type == "method_definition" and name:
            spans.append(StmtSpan(start, c.end_point[0] + 1, "def", prefix + name, c))
        else:
            spans.append(StmtSpan(start, c.end_point[0] + 1, "stmt", "", c))
    return spans


def _imports(root: Node) -> list[ImportSite]:
    sites = []
    for node in root.named_children:
        if node.type != "import_statement":
            continue
        source = node.child_by_field_name("source")
        if source is None:
            continue
        module = "".join(_text(c) for c in source.named_children) if source.named_children else ""
        bindings: list[Binding] = []
        quoted = f'"{module}"'
        for clause in (c for c in node.named_children if c.type == "import_clause"):
            for part in clause.named_children:
                if part.type == "identifier":
                    alias = _text(part)
                    bindings.append(
                        Binding(module, "default", alias, text=f"import {alias} from {quoted}")
                    )
                elif part.type == "namespace_import":
                    ids = [c for c in part.named_children if c.type == "identifier"]
                    alias = _text(ids[-1]) if ids else ""
                    bindings.append(
                        Binding(module, None, alias, text=f"import * as {alias} from {quoted}")
                    )
                elif part.type == "named_imports":
                    for spec in part.named_children:
                        if spec.type != "import_specifier":
                            continue
                        name = _text(spec.child_by_field_name("name"))
                        alias = _text(spec.child_by_field_name("alias")) or name
                        shown = name if alias == name else f"{name} as {alias}"
                        bindings.append(
                            Binding(module, name, alias, text=f"import {{ {shown} }} from {quoted}")
                        )
        if not bindings:
            bindings.append(Binding(module, None, "", text=f"import {quoted}"))
        sites.append(ImportSite(node.start_point[0] + 1, node.end_point[0] + 1, tuple(bindings)))
    return sites
