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


@dataclass
class FileAnalysis:
    lines: list[str]
    tokens: list[Token]
    annotations: list[Annotation] = field(default_factory=list)
    parsed: bool = True  # False when the analyzer had to fall back to a rough tokenizer


class LanguageAnalyzer(Protocol):
    name: str

    def handles(self, path: str) -> bool: ...

    def analyze(self, text: str) -> FileAnalysis: ...

    def is_keyword(self, value: str) -> bool: ...

    def import_keywords(self) -> frozenset[str]:
        """Tokens that start an import statement."""
        ...

    def definition_keywords(self) -> frozenset[str]:
        """Tokens that introduce a named definition (def, class, ...)."""
        ...


def split_lines(text: str) -> list[str]:
    """Split on "\\n" the same way line-based tokenizers number lines."""
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return [line.rstrip("\r") for line in lines]
