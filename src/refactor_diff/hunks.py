"""Line-level diffing: split a file change into hunks and candidate change units."""

from __future__ import annotations

from dataclasses import dataclass
from difflib import SequenceMatcher

CONTEXT = 3


@dataclass(frozen=True)
class Opcode:
    tag: str  # "replace" | "delete" | "insert"
    i1: int  # 0-based old line slice [i1, i2)
    i2: int
    j1: int  # 0-based new line slice [j1, j2)
    j2: int

    @property
    def old_range(self) -> tuple[int, int] | None:
        return (self.i1 + 1, self.i2) if self.i2 > self.i1 else None

    @property
    def new_range(self) -> tuple[int, int] | None:
        return (self.j1 + 1, self.j2) if self.j2 > self.j1 else None


def line_matcher(old: list[str], new: list[str]) -> SequenceMatcher:
    """The line matcher shared by the engine and the file viewer. Blank lines are junk: they
    may extend a match but never anchor one, so an import block followed by a blank line is not
    torn apart to pair the blank line with another."""
    return SequenceMatcher(_blank, old, new, autojunk=False)


def _blank(line: str) -> bool:
    return not line.strip()


def diff_hunks(old: list[str], new: list[str]) -> list[list[tuple[str, int, int, int, int]]]:
    """Unified-diff style hunk groups (opcodes including surrounding context)."""
    return [list(g) for g in line_matcher(old, new).get_grouped_opcodes(CONTEXT)]


def candidate_units(op: Opcode) -> tuple[list[Opcode], list[Opcode] | None]:
    """The whole opcode as one block, plus a line-by-line pairing when the old and new sides
    have the same number of lines (the engine keeps whichever classifies more cleanly)."""
    block = [op]
    n = op.i2 - op.i1
    if op.tag == "replace" and n == op.j2 - op.j1 and n > 1:
        pairs = [
            Opcode("replace", op.i1 + k, op.i1 + k + 1, op.j1 + k, op.j1 + k + 1) for k in range(n)
        ]
        return block, pairs
    return block, None
