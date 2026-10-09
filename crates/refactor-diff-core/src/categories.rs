//! Classify changed files by role (tests, docs, config, ...) so the UI can filter them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Source,
    Tests,
    Docs,
    Config,
    Other,
}

impl Category {
    pub const ALL: [Category; 5] = [
        Category::Source,
        Category::Tests,
        Category::Docs,
        Category::Config,
        Category::Other,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Source => "source",
            Category::Tests => "tests",
            Category::Docs => "docs",
            Category::Config => "config",
            Category::Other => "other",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

// Order matters: the first matching category wins (tests/fixtures/README.md is a test file).
const TEST_DIRS: &[&str] = &["test", "tests", "testing", "__tests__", "spec", "specs"];
const TEST_FILES: &[&str] = &[
    "test_*",
    "*_test.*",
    "*_tests.*",
    "tests.py",
    "conftest.py",
    "*.spec.*",
    "*.test.*",
];
const DOC_DIRS: &[&str] = &["doc", "docs", "documentation"];
const DOC_FILES: &[&str] = &[
    "*.md",
    "*.rst",
    "*.txt",
    "*.adoc",
    "readme*",
    "changelog*",
    "license*",
    "authors*",
];
const CONFIG_DIRS: &[&str] = &[".github", ".circleci", ".devcontainer"];
#[rustfmt::skip]
const CONFIG_FILES: &[&str] = &[
    "*.toml", "*.cfg", "*.ini", "*.yaml", "*.yml", "*.json", "*.lock", "*.env*",
    "dockerfile*", "makefile", ".gitignore", ".gitattributes", ".pre-commit-config.yaml",
    "requirements*.txt", "pipfile", "tox.ini", "noxfile.py", "setup.py",
    "*.config.js", "*.config.cjs", "*.config.mjs", "*.config.ts", "*.config.mts", ".eslintrc*",
    ".prettierrc*", ".npmrc", ".nvmrc",
];

pub fn categorize(path: &str, analyzed: bool) -> Category {
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let name = parts.pop().unwrap_or("").to_lowercase();
    let dirs: Vec<String> = parts.iter().map(|d| d.to_lowercase()).collect();
    let in_dirs = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    if in_dirs(TEST_DIRS) || matches_any(&name, TEST_FILES) {
        return Category::Tests;
    }
    if matches_any(&name, CONFIG_FILES) || in_dirs(CONFIG_DIRS) {
        return Category::Config;
    }
    if in_dirs(DOC_DIRS) || matches_any(&name, DOC_FILES) {
        return Category::Docs;
    }
    if analyzed {
        Category::Source
    } else {
        Category::Other
    }
}

fn matches_any(name: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pat| fnmatch(name, pat))
}

/// Python `fnmatch.fnmatchcase`: `*`, `?` and `[...]` (with `!` negation and ranges).
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pat: Vec<char> = pattern.chars().collect();
    glob_match(&name, &pat)
}

fn glob_match(name: &[char], pat: &[char]) -> bool {
    match pat.first() {
        None => name.is_empty(),
        Some('*') => {
            let rest = &pat[1..];
            (0..=name.len()).any(|i| glob_match(&name[i..], rest))
        }
        Some('?') => !name.is_empty() && glob_match(&name[1..], &pat[1..]),
        Some('[') => {
            let Some(close) = pat.iter().skip(1).position(|&c| c == ']').map(|i| i + 1) else {
                // No closing bracket: the '[' is literal.
                return name.first() == Some(&'[') && glob_match(&name[1..], &pat[1..]);
            };
            let Some(&ch) = name.first() else {
                return false;
            };
            let mut set = &pat[1..close];
            let negate = set.first() == Some(&'!');
            if negate {
                set = &set[1..];
            }
            let mut matched = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == '-' {
                    if set[i] <= ch && ch <= set[i + 2] {
                        matched = true;
                    }
                    i += 3;
                } else {
                    if set[i] == ch {
                        matched = true;
                    }
                    i += 1;
                }
            }
            matched != negate && glob_match(&name[1..], &pat[close + 1..])
        }
        Some(&c) => name.first() == Some(&c) && glob_match(&name[1..], &pat[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorizes_like_python() {
        let cases = [
            ("src/app/models.py", true, Category::Source),
            ("tests/test_models.py", true, Category::Tests),
            ("tests/fixtures/README.md", false, Category::Tests),
            ("src/thing_test.go", false, Category::Tests),
            ("app/__tests__/x.spec.ts", true, Category::Tests),
            ("pyproject.toml", false, Category::Config),
            (".github/workflows/ci.yml", false, Category::Config),
            ("Dockerfile", false, Category::Config),
            ("vite.config.ts", true, Category::Config),
            ("README.md", false, Category::Docs),
            ("docs/index.html", false, Category::Docs),
            ("CHANGELOG", false, Category::Docs),
            ("src/lib.rs", false, Category::Other),
            ("setup.py", true, Category::Config),
        ];
        for (path, analyzed, want) in cases {
            assert_eq!(categorize(path, analyzed), want, "{path}");
        }
    }

    #[test]
    fn fnmatch_basics() {
        assert!(fnmatch("test_x.py", "test_*"));
        assert!(fnmatch("a.spec.ts", "*.spec.*"));
        assert!(!fnmatch("aspec.ts", "*.spec.*"));
        assert!(fnmatch("x1", "x[0-9]"));
        assert!(fnmatch("xa", "x[!0-9]"));
        assert!(fnmatch("ab", "a?"));
    }
}
