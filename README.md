# refactor-diff

Review refactor-heavy diffs without reading the same one-line change fifty times.

When you rename a function, rename a variable, or change a type, the diff fills up with
near-identical hunks spread across many files. `refactor-diff` reads the diff at the token
level, collapses the repeated mechanical edits into patterns such as
`rename get_user → fetch_user ×42 in 17 files`, and shows you only the leftover changes
that need a real review.

It currently analyzes **Python only**. Other changed files are listed but not collapsed.
Support for more languages is planned (see [Roadmap](#roadmap)).

## Install

Requires Python 3.10+ and git. Uses the [GitHub CLI](https://cli.github.com/) (`gh`) for pull requests.

```bash
uv tool install .
```

For development:

```bash
uv sync
```

## Usage

Run it inside a git repository. It starts a local web UI on `127.0.0.1` and opens it in your browser:

```bash
refactor-diff
```

In the UI, pick what to compare:

- **Branches**: base and head refs. The diff is taken against their merge-base, the same way GitHub does it.
- **Pull request**: a GitHub PR number. Missing commits are fetched from `origin`.
- **Working tree**: uncommitted changes to tracked files, compared against a base ref.

You can also pre-select a source on the command line:

```bash
refactor-diff main..my-refactor        # compare branches
refactor-diff --pr 123                 # a GitHub pull request
refactor-diff --worktree main          # uncommitted changes vs main
refactor-diff --repo ../other --port 8000 --no-browser
```

### What you see

- **Summary**: how much of the diff was collapsed, and how many changes are left to review.
- **Needs review**: the hunks that contain changes not explained by a repeated pattern.
  Lines that a pattern does explain are dimmed and tagged, so you keep the context.
- **Mechanical patterns**: one entry per repeated edit, listing every occurrence grouped by
  file. Tick **Reviewed** as you go. The checkmarks are saved in your browser for that commit range.
- **Warnings**: places where the refactor may be incomplete or inconsistent (see below).
- **Files**: every changed file, with its status and whether it was analyzed.

## How it works

1. **Load the change set.** Uses `git diff --name-status -M` between the merge-base and head,
   and reads file contents with `git cat-file --batch`.
2. **Split into change units.** A line-level diff is taken per file. When a replaced block has
   the same number of lines on each side, the lines are paired one by one. Otherwise the block
   stays whole, for example when a call is re-wrapped across lines.
3. **Classify each unit.** Old and new tokens (from `tokenize`, with `ast` for type
   annotations) are aligned, and every differing span becomes a signature:

   | Kind | Example |
   |---|---|
   | `rename` | `get_user(id)` → `fetch_user(id)`. Records the context: definition, call, attribute, import or name |
   | `retype` | `def f(x: int)` → `def f(x: str)`, `-> List[int]` → `-> list[int]`, adding an annotation |
   | `replace` | any other repeated substitution, e.g. `cfg.get("timeout")` → `settings.timeout` |
   | `formatting` | only whitespace or layout changed (quote style, re-wrapping) |

4. **Group.** Units are grouped by signature. A pattern is *mechanical* when it repeats at
   least **Min repeats** times (default 2). A unit counts as explained only when every
   signature on it is mechanical. So a line that renames `get_user` *and* changes logic still
   shows up in **Needs review**.
5. **Sanity checks.**
   - *Missed renames*: the old name still appears after a mechanical rename. If the
     definition itself was renamed, this is checked across the whole repository at head.
     Otherwise it is only checked in the files where the rename happened.
   - *Inconsistent renames*: the same identifier was renamed to different names.

## Development

```bash
uv run pytest
uv run ruff check . && uv run ruff format --check .
```

Layout (`src/refactor_diff/`):

| Module | Purpose |
|---|---|
| `sources.py` | git, GitHub PR and working-tree loading |
| `hunks.py` | line diff, hunk grouping, candidate units |
| `languages/` | language analyzers (`base.py` interface, `python.py`) |
| `patterns.py` | token alignment, signature classification |
| `grouping.py` | grouping, mechanical threshold, warnings |
| `engine.py` | `analyze()`, which turns a source into a `Report` |
| `model.py` | serializable report model with stable IDs |
| `web/` | Starlette server and the vanilla-JS single-page UI |

### Adding a language

Implement the `LanguageAnalyzer` protocol in `languages/base.py`. It turns source text into
tokens and type-annotation spans and names the language's keywords. Then register the
analyzer in `languages/__init__.py`. The rest of the pipeline does not depend on the language.

## Roadmap

- Post review comments to a PR from the UI
- Apply or revert edits in the working tree from the UI
- More languages (TypeScript, Go, …)
- Detect code moved between files
