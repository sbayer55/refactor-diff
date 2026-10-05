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
refactor-diff main..HEAD --hide tests,comments --exclude migrations
```

### What you see

- **Summary**: how much of the diff was collapsed, and how many changes are left to review.
- **Needs review**: the hunks that contain changes not explained by a repeated pattern.
  Lines that a pattern does explain are dimmed and tagged, so you keep the context.
- **Mechanical patterns**: one entry per repeated edit, listing every occurrence grouped by
  file. Tick **Reviewed** as you go. The checkmarks are saved in your browser for that commit range.
- **Warnings**: places where the refactor may be incomplete or inconsistent (see below).
- **Files**: every changed file, with its status, kind and whether it was analyzed.

### Context and file versions

- **Show context**: in a pattern's occurrence or a Needs review hunk, show the surrounding
  lines. Use the ↑/↓ controls to reveal 10 more lines at a time, or jump to the start or
  end of the file. The window never cuts a change in half.
- **Original / New / Full diff**: links on every file header and occurrence open a
  whole-file viewer at that line. Switch between the base version, the head version and the
  complete diff of the file; the line stays in view. Use **↑ Prev change / ↓ Next change** or
  the `p` / `n` keys to move between changes.

### Filtering

The filter bar under the summary narrows everything on the page to what you care about. The
summary, patterns, review list and warnings all recount for the visible changes. Filtering
happens in the browser and never re-runs the analysis. Your filters are remembered per
repository.

- **Show files**: toggle files by kind. Each changed file is classified from its path:
  - *tests*: `tests/` and `test/` directories, `test_*.py`, `*_test.py`, `conftest.py`
  - *config*: `*.toml`, `*.yaml`, `*.json`, lock files, `requirements*.txt`, `.github/`, …
  - *docs*: `docs/` directories, `*.md`, `*.rst`, `*.txt`, README, CHANGELOG, …
  - *source*: other analyzed files; *other*: everything else
- **Comment & docstring edits**: hide changes that only touch comments or docstrings. Even
  when shown, these are collapsed into their own `docs` pattern, not listed under Needs review.
- **Exclude**: comma-separated globs. A pattern without `/` matches any path segment
  (`migrations`, `*_pb2.py`); a pattern with `/` matches the whole path (`src/legacy/**`).

To pre-set filters from the command line, use `--hide` (any of `source`, `tests`, `docs`,
`config`, `other`, `comments`) and `--exclude GLOB` (repeatable).

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
   | `rename` | `get_user(id)` → `fetch_user(id)`. Records the context: definition, call, attribute, import, keyword argument or name |
   | `retype` | `def f(x: int)` → `def f(x: str)`, `-> List[int]` → `-> list[int]`, adding an annotation |
   | `replace` | any other repeated substitution, e.g. `cfg.get("timeout")` → `settings.timeout` |
   | `formatting` | only whitespace or layout changed (quote style, re-wrapping) |
   | `docs` | only comments or docstrings changed |

   Nearby edits that belong together become one template instead of fragments. This
   applies when an edit opens a bracket that a later edit closes, or when two edits are
   separated only by `.` or `=`. Unchanged names and literals in between become `…`, so
   `role="admin"` → `roles=("admin",)` and `role="faculty"` → `roles=("faculty",)` share the
   pattern `role=… → roles=(…,)`, and `actor` → `str(actor.user_id)` reads as `… → str(….user_id)`.

4. **Group.** Units are grouped by signature. A pattern is *mechanical* when it repeats at
   least **Min repeats** times (default 2). A unit counts as explained only when every
   signature on it is mechanical. So a line that renames `get_user` *and* changes logic still
   shows up in **Needs review**.
5. **Sanity checks.**
   - *Missed renames*: a definition (`def`/`class`) was renamed, the old name is no longer
     defined anywhere in the repository at head, yet code still references it. This produces
     one warning per rename, listing every location. Renamed locals, parameters, keyword
     arguments and builtins are not checked: other variables with the same name are usually
     unrelated.
   - *Inconsistent renames*: a definition or import was renamed to different names in
     different places.

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
| `categories.py` | file kinds (source, tests, docs, config, other) for filtering |
| `engine.py` | `analyze()`, which turns a source into a `Report` |
| `fileview.py` | whole-file diff of one changed file, for context and the old/new viewer |
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
