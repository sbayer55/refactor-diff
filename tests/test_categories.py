import pytest

from refactor_diff.categories import CONFIG, DOCS, OTHER, SOURCE, TESTS, categorize


@pytest.mark.parametrize(
    ("path", "analyzed", "expected"),
    [
        ("src/app/users.py", True, SOURCE),
        ("tests/test_users.py", True, TESTS),
        ("src/app/users_test.py", True, TESTS),
        ("src/app/test_users.py", True, TESTS),
        ("conftest.py", True, TESTS),
        ("tests/fixtures/README.md", False, TESTS),
        ("README.md", False, DOCS),
        ("docs/conf.py", True, DOCS),
        ("docs/guide/index.rst", False, DOCS),
        ("CHANGELOG", False, DOCS),
        ("pyproject.toml", False, CONFIG),
        ("requirements-dev.txt", False, CONFIG),
        (".github/workflows/ci.yml", False, CONFIG),
        ("Dockerfile", False, CONFIG),
        ("static/app.js", False, OTHER),
        ("src/users.ts", True, SOURCE),
        ("src/components/Button.tsx", True, SOURCE),
        ("src/users.test.ts", True, TESTS),
        ("src/__tests__/users.ts", True, TESTS),
        ("tsconfig.json", False, CONFIG),
        ("vite.config.ts", True, CONFIG),
    ],
)
def test_categorize(path, analyzed, expected):
    assert categorize(path, analyzed) == expected
