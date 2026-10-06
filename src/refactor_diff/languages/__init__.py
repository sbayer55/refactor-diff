"""Registry of language analyzers. Add new languages here."""

from __future__ import annotations

from refactor_diff.languages.base import LanguageAnalyzer
from refactor_diff.languages.python import PythonAnalyzer
from refactor_diff.languages.typescript import analyzers as typescript_analyzers

ANALYZERS: list[LanguageAnalyzer] = [PythonAnalyzer(), *typescript_analyzers()]


def analyzer_for(path: str) -> LanguageAnalyzer | None:
    for analyzer in ANALYZERS:
        if analyzer.handles(path):
            return analyzer
    return None
