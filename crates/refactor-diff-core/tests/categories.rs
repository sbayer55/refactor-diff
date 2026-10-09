//! Port of tests/test_categories.py: file roles for the UI's category filter.

use refactor_diff_core::{Category, categorize};

#[test]
fn categorize_paths() {
    use Category::*;
    let cases: &[(&str, bool, Category)] = &[
        ("src/app/users.py", true, Source),
        ("tests/test_users.py", true, Tests),
        ("src/app/users_test.py", true, Tests),
        ("src/app/test_users.py", true, Tests),
        ("conftest.py", true, Tests),
        ("tests/fixtures/README.md", false, Tests),
        ("README.md", false, Docs),
        ("docs/conf.py", true, Docs),
        ("docs/guide/index.rst", false, Docs),
        ("CHANGELOG", false, Docs),
        ("pyproject.toml", false, Config),
        ("requirements-dev.txt", false, Config),
        (".github/workflows/ci.yml", false, Config),
        ("Dockerfile", false, Config),
        ("static/app.js", false, Other),
        ("src/users.ts", true, Source),
        ("src/components/Button.tsx", true, Source),
        ("src/users.test.ts", true, Tests),
        ("src/__tests__/users.ts", true, Tests),
        ("tsconfig.json", false, Config),
        ("vite.config.ts", true, Config),
    ];
    for (path, analyzed, expected) in cases {
        assert_eq!(
            categorize(path, *analyzed),
            *expected,
            "categorize({path:?}, {analyzed})"
        );
    }
}
