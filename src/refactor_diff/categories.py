"""Classify changed files by role (tests, docs, config, ...) so the UI can filter them."""

from __future__ import annotations

from fnmatch import fnmatch
from pathlib import PurePosixPath

SOURCE = "source"
TESTS = "tests"
DOCS = "docs"
CONFIG = "config"
OTHER = "other"
CATEGORIES = (SOURCE, TESTS, DOCS, CONFIG, OTHER)

# Order matters: the first matching category wins (tests/fixtures/README.md is a test file).
_TEST_DIRS = {"test", "tests", "testing", "__tests__", "spec", "specs"}
_TEST_FILES = ["test_*", "*_test.*", "*_tests.*", "tests.py", "conftest.py", "*.spec.*", "*.test.*"]
_DOC_DIRS = {"doc", "docs", "documentation"}
_DOC_FILES = ["*.md", "*.rst", "*.txt", "*.adoc", "readme*", "changelog*", "license*", "authors*"]
_CONFIG_DIRS = {".github", ".circleci", ".devcontainer"}
_CONFIG_FILES = [
    "*.toml", "*.cfg", "*.ini", "*.yaml", "*.yml", "*.json", "*.lock", "*.env*",
    "dockerfile*", "makefile", ".gitignore", ".gitattributes", ".pre-commit-config.yaml",
    "requirements*.txt", "pipfile", "tox.ini", "noxfile.py", "setup.py",
]  # fmt: skip


def categorize(path: str, analyzed: bool) -> str:
    p = PurePosixPath(path)
    dirs = {d.lower() for d in p.parts[:-1]}
    name = p.name.lower()
    if dirs & _TEST_DIRS or _matches(name, _TEST_FILES):
        return TESTS
    if _matches(name, _CONFIG_FILES) or dirs & _CONFIG_DIRS:
        return CONFIG
    if dirs & _DOC_DIRS or _matches(name, _DOC_FILES):
        return DOCS
    return SOURCE if analyzed else OTHER


def _matches(name: str, patterns: list[str]) -> bool:
    return any(fnmatch(name, pat) for pat in patterns)
