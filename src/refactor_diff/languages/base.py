"""Language-neutral types every analyzer produces."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Protocol

# Token kinds. Structural tokens carry layout meaning (Python indentation, logical
# line ends) but have no visible text worth highlighting.
NAME = "name"
OP = "op"
STRING = "string"
NUMBER = "number"
COMMENT = "comment"
STRUCTURAL = "structural"
OTHER = "other"

Pos = tuple[int, int]  # (1-based line, 0-based character column)


@dataclass(frozen=True)
class Token:
    kind: str
    value: str  # normalized value used for comparison
    text: str  # original source text
    start: Pos
    end: Pos


@dataclass(frozen=True)
class Annotation:
    """A type annotation span and what it annotates (e.g. "param user_id")."""

    start: Pos
    end: Pos
    text: str
    target: str

    def contains(self, tok: Token) -> bool:
        return self.start <= tok.start and tok.end <= self.end


@dataclass(frozen=True)
class StmtSpan:
    """A statement's line span (1-based, inclusive; decorators included). Top-level
    statements recurse into class bodies to method level via ``children``."""

    start: int
    end: int
    kind: str  # "def", "class", "stmt"
    qualname: str  # "Cls.method" for methods, the def/class name, or "" for other statements
    node: object = field(repr=False, compare=False)  # the language's AST node
    children: tuple[StmtSpan, ...] = ()

    def contains(self, first: int, last: int) -> bool:
        return self.start <= first and last <= self.end


Arg = tuple[Pos, Pos, str | None]  # (start, end, keyword); keyword None = positional, "*"/"**"


@dataclass(frozen=True)
class CallSite:
    """A call expression: ``name`` is the callee text, ``args_*`` the span from the opening
    parenthesis through the closing one."""

    name: str
    start: Pos
    end: Pos
    args_start: Pos
    args_end: Pos
    args: tuple[Arg, ...]

    @property
    def short_name(self) -> str:
        return self.name.rsplit(".", 1)[-1]


@dataclass(frozen=True)
class Param:
    name: str
    kind: str  # "posonly" | "pos" | "vararg" | "kwonly" | "kwarg"
    has_default: bool
    start: Pos
    end: Pos  # span of the whole parameter including annotation and default


@dataclass(frozen=True)
class DefSite:
    name: str
    start: Pos
    end: Pos
    params_start: Pos  # the parenthesised parameter list, parens included
    params_end: Pos
    params: tuple[Param, ...]

    @property
    def short_name(self) -> str:
        return self.name


@dataclass(frozen=True)
class Binding:
    """One name an import statement binds. ``name`` is None for ``import a.b``."""

    module: str  # dotted module, prefixed with "." per relative level
    name: str | None
    alias: str  # the name bound in the importing module
    level: int = 0
    text: str = field(default="", compare=False)  # display form, when not Python's syntax


@dataclass(frozen=True)
class ImportSite:
    start: int  # 1-based inclusive lines
    end: int
    bindings: tuple[Binding, ...]


@dataclass
class FileAnalysis:
    lines: list[str]
    tokens: list[Token]
    annotations: list[Annotation] = field(default_factory=list)
    docstrings: list[tuple[Pos, Pos]] = field(default_factory=list)  # (start, end) spans
    parsed: bool = True  # False when the analyzer had to fall back to a rough tokenizer
    # Structure from the parser; empty when the file didn't parse.
    tree: object | None = field(default=None, repr=False, compare=False)
    statements: list[StmtSpan] = field(default_factory=list)
    calls: list[CallSite] = field(default_factory=list)
    defs: list[DefSite] = field(default_factory=list)
    imports: list[ImportSite] = field(default_factory=list)


class LanguageAnalyzer(Protocol):
    name: str
    globs: tuple[str, ...]  # git pathspecs for every file this language family analyzes

    def handles(self, path: str) -> bool: ...

    def analyze(self, text: str) -> FileAnalysis: ...

    def is_keyword(self, value: str) -> bool: ...

    def is_builtin(self, value: str) -> bool:
        """Names always available without a definition (renaming them away is never "missed")."""
        ...

    def import_keywords(self) -> frozenset[str]:
        """Tokens that start an import statement."""
        ...

    def definition_keywords(self) -> frozenset[str]:
        """Tokens that introduce a named definition (def, class, ...)."""
        ...

    # Optional: analyzers without these (or that leave ``FileAnalysis.tree`` None) opt out of
    # AST verification.
    def parse_block(self, text: str) -> object | None:
        """Parse a standalone block of code (dedented), or None if it doesn't parse."""
        ...

    def normalized_dump(
        self, node: object, renames: dict[str, str], strip_annotations: bool
    ) -> str:
        """A canonical dump of an AST node with docstrings removed, identifiers mapped
        through ``renames`` and, optionally, type annotations dropped. Equal dumps mean
        the code is the same program."""
        ...


def split_lines(text: str) -> list[str]:
    """Split on "\\n" the same way line-based tokenizers number lines."""
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line.rstrip("\r") for line in lines]


def char_col(lines: list[str], lineno: int, byte_col: int) -> int:
    """Convert a UTF-8 byte offset (as parsers report it) to the character offset tokens use."""
    if lineno - 1 >= len(lines):
        return byte_col
    line = lines[lineno - 1]
    return len(line.encode("utf-8")[:byte_col].decode("utf-8", errors="replace"))
