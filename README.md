# refactor-diff

Review refactor-heavy diffs without reading the same one-line change fifty times.

When you rename a function, rename a variable, or change a type, the diff fills up with
near-identical hunks spread across many files. `refactor-diff` reads the diff at the token
level, collapses the repeated mechanical edits into patterns such as
`rename get_user → fetch_user ×42 in 17 files`, and shows you only the leftover changes
that need a real review.

It analyzes **Python, TypeScript and JavaScript** (`.py`, `.pyi`, `.ts`, `.tsx`, `.mts`,
`.cts`, `.js`, `.jsx`, `.mjs`, `.cjs`). Other changed files are listed but not collapsed.
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

### Desktop app (macOS)

There is also a self-contained macOS app that bundles the backend, so it needs no Python
on the machine it runs on (it still uses the `git`, `gh` and `node` it finds on your PATH).
It opens with a folder picker and remembers recent repositories; you can also drop a
repository on its Dock icon or run `open -a "Refactor Diff" ~/code/my-repo`.

Building it requires [Rust](https://rustup.rs), Node 20+, the Xcode command line tools and
`uv`:

```bash
cd desktop
npm install
npm run icons            # once: generates src-tauri/icons from app-icon.svg
npm run build:sidecar    # freezes the Python backend with PyInstaller
npm run dev              # run it
npm run build            # bundles src-tauri/target/release/bundle/{macos,dmg}
```

The build is ad-hoc signed, not notarized: on another Mac, right-click the app and choose
Open the first time. The app builds for the architecture it is built on; there is no
universal build because the backend's native extensions are per-architecture.

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
refactor-diff main..HEAD --python ~/.virtualenvs/myproject   # environment for code navigation
refactor-diff main..HEAD --tsserver ~/tools/node_modules/typescript   # TypeScript for navigation
refactor-diff main..HEAD --editor cursor                      # "Open" links target Cursor
```

### What you see

The window is a workbench: the comparison in a command strip along the top, an **explorer**
on the left, the diff stream in the middle, an **inspector** on the right, and a **status
bar** along the bottom.

- **Status bar**: how much of the diff was collapsed, how many changes are left to review,
  `done / total` hunks, warnings, and the current diff settings.
- **Explorer**: three tabs. **Files** lists every changed file by directory, with the number
  of hunks it still has to review (click one to jump to it). **Patterns** lists the
  mechanical patterns with their reviewed ticks. **Warnings** lists the warnings. The tab
  follows the view, and you can switch it by hand. The buttons at either end of the command
  strip (or `\` and `|`) hide and show the explorer and the inspector.
- **Inspector**: facts about what's focused. On Needs review, the focused hunk: which
  patterns explain its dimmed lines, what is left to read, whether it verified, and buttons
  for the keyboard actions. On a pattern page, the pattern's counts and where it was applied.
  Below that, always: review progress, the warnings, the report's source with
  **Copy as Markdown** and **Post summary to PR**, and the main keys.
- **Needs review**: the hunks that contain changes not explained by a repeated pattern.
  Lines that a pattern does explain are dimmed and tagged, so you keep the context. Tick a
  hunk once you've read it: it folds up, and the status bar shows `done / total` hunks.
  **Mark all reviewed** on a file header ticks every hunk in that file.
- **Mechanical patterns**: one entry per repeated edit, listing every occurrence grouped by
  file. Tick **Reviewed** as you go.
  A moved block is one pattern: its page shows the block as a diff from where it came from,
  followed by the import lines that changed because of the move.
- **✓ verified**: a change whose enclosing statement is provably the same program on both
  sides (see [Verification](#verification)). The status bar counts them.
- **≈ almost …**: a leftover change that nearly matches a pattern — usually a typo. The
  **Only near misses** filter shows just those.
- **Warnings**: places where the refactor may be incomplete or inconsistent (see below).
- **Files**: every changed file, with its status, kind and whether it was analyzed.
- **Commits**: when the range has more than one commit, the list of commits with their
  sizes. **Analyze** one to review it against its parent on its own — a refactor PR often has
  one mechanical commit and one substantive one — with **← Back** to return to the whole
  range (`}` / `{` step through the commits). A commit's reviewed marks are separate.

### Getting the review out

- **Copy as Markdown** (in the inspector's Report card) copies the review: stats, a table of the mechanical
  patterns with their reviewed ticks, a task list of the hunks that need review, and the
  warnings. `GET /api/report/{id}/summary.md` serves the same text.
- For a pull request, **Post summary to PR** shows that Markdown in an editable preview and
  posts it as a PR comment, and every hunk in Needs review has a **Comment** button (or `c`)
  that posts an inline review comment on the hunk's first unexplained line (new side when it
  has one). Both go through `gh`, as you, after one confirmation per session.

### Review marks survive restarts and new commits

Reviewed marks are stored in `~/.config/refactor-diff/` (or `$XDG_CONFIG_HOME/refactor-diff/`),
one file per repository, keyed by *what* you compared — the PR number, or the branch names —
rather than by commit. Hunks are identified by a fingerprint of their changed lines, so a
reviewed hunk stays reviewed when lines above it shift.

When you analyze the same comparison again after new commits, a banner at the top of
**Needs review** says what happened since the previous head: how many changes are new
(badged **new** on their hunk, and on pattern occurrences), and which reviewed hunks were
modified — those lose their mark so you read them again. **Show only what's new** (also a
filter chip) narrows everything to the new changes; a reviewed pattern that gained
occurrences shows **+N** in the explorer's Patterns tab.

### Context and file versions

- **Show context**: in a pattern's occurrence or a Needs review hunk, show the surrounding
  lines. Use the ↑/↓ controls to reveal 10 more lines at a time, or jump to the start or
  end of the file. The window never cuts a change in half.
- **Original / New / Full diff**: links on every file header and occurrence open a
  whole-file viewer at that line. Switch between the base version, the head version and the
  complete diff of the file; the line stays in view. Use **↑ Prev change / ↓ Next change** or
  the `p` / `n` keys to move between changes.
- **Unified / Split**: switch every diff on the page (review hunks, pattern occurrences,
  context and the full-file diff) between unified and side-by-side layout. The toggle is in
  the toolbar above the diffs and in the file viewer. Your choice is remembered. Added and
  deleted files, which have only one side, and windows narrower than 760px always use unified.

### Filtering

The filters under the explorer, and the search box in the toolbar, narrow everything on the
page to what you care about. The status bar, patterns, review list and warnings all recount
for the visible changes. Filtering
happens in the browser and never re-runs the analysis. Your filters are remembered per
repository.

- **Show files**: toggle files by kind. Each changed file is classified from its path:
  - *tests*: `tests/` and `test/` directories, `test_*.py`, `*_test.py`, `conftest.py`
  - *config*: `*.toml`, `*.yaml`, `*.json`, lock files, `requirements*.txt`, `.github/`, …
  - *docs*: `docs/` directories, `*.md`, `*.rst`, `*.txt`, README, CHANGELOG, …
  - *source*: other analyzed files; *other*: everything else
- **Comment & docstring edits**: hide changes that only touch comments or docstrings. Even
  when shown, these are collapsed into their own `docs` pattern, not listed under Needs review.
- **Import edits**: hide changes made only of import statements, whatever they did to them
  (added, removed, reordered or re-pointed imports). A line that mixes an import with other
  code (`import os; x = 1`) stays.
- **File renames**: hide files that were renamed or moved without any change, and import
  updates that only follow a renamed file (`from pkg.models import User` →
  `from pkg.entities import User` after `models.py` became `entities.py`, or a relative import
  adjusted because the importing file moved). Edits inside a renamed file stay.
- **Moved functions**: hide functions, methods and classes that were moved verbatim, together
  with the import edits the move explains. Only certain moves are hidden (see
  [Moved code](#how-it-works)); a move with any edit inside, even to a comment, stays visible.
- **Exclude**: comma-separated globs. A pattern without `/` matches any path segment
  (`migrations`, `*_pb2.py`); a pattern with `/` matches the whole path (`src/legacy/**`).

- **Search**: text (or a regular expression with the `.*` toggle) matched against changed
  lines, file paths and pattern labels. Everything narrows to the matching changes and the
  matches are highlighted. `/` focuses the box, `Esc` clears it.

To pre-set filters from the command line, use `--hide` (any of `source`, `tests`, `docs`,
`config`, `other`, `comments`, `imports`, `file-moves`, `moves`) and `--exclude GLOB`
(repeatable).

### Keyboard

Press `?` for the full list. The review loop is `j` / `k` to move between hunks (or
occurrences on a pattern page), `x` to mark the focused one reviewed (focus moves on to the
next unreviewed hunk), `e` to show its context and then reveal more, `o` to open it in your
editor, `]` / `[` to step through the mechanical patterns, `g r` / `g w` / `g f` to jump to
Needs review / Warnings / Files, and `/` to search. In the file viewer `n` / `p` move between
changes.

### Open in editor

Every file header, occurrence, warning location and the file viewer have an **Open** link
that opens the file at that line in your editor — VS Code by default. `--editor` takes
`vscode`, `cursor`, `zed`, `idea`, `pycharm`, or a URL template with `{path}`, `{line}` and
`{col}` (e.g. `--editor 'x-mine://{path}?l={line}'`). The editor opens your checkout, which
is the head revision only when you're reviewing the working tree or have the head branch
checked out.

### Highlighting

- **Diff** (default): changed lines show exactly which tokens changed. Wherever there's no
  diff to show, code is always syntax-colored: unchanged lines, added and deleted files (the
  whole file is one change), and files viewed outside the diff (unchanged files reached by
  navigation, and library code).
- **Syntax**: all code is syntax-colored on a plain background.

In both modes the gutter (line numbers and the `-`/`+` sign) is tinted red or green for removed
and added lines, so changes stay visible. The Diff | Syntax toggle sits next to Unified |
Split in the toolbar and the file viewer, and is remembered per browser. Highlighting is
done in the browser (`web/static/syntax.js`) for Python, TypeScript and JavaScript.

### Code navigation

**⌘-click** (Ctrl-click on Linux/Windows) a name in any diff or file view to go to its
definition. **⌘⇧-click** finds every reference to it. Holding ⌘ underlines the name under
the pointer.

- **Each side resolves in its own revision.** A name on a removed line is looked up in the
  base commit, and a name on an added or unchanged line in the head (or your working tree).
  So clicking a function that the diff deleted still finds where it used to be defined.
- **Results:** a single definition opens directly at its line. Multiple definitions, and all
  references, open in a side panel grouped by file. Definitions can lead to files the diff
  doesn't touch, to installed packages (shown read-only), or to standard-library stubs.
- **Environment:** imports of installed packages are resolved with your project's virtualenv:
  `.venv`, `venv` or `env` in the repository, or the one given with `--python`. Without one,
  navigation within the repository still works, but jumps into third-party packages won't.
- **TypeScript and JavaScript** need Node.js and TypeScript: the repository's own
  `node_modules/typescript`, a `tsserver` on your PATH, or the one given with `--tsserver`
  (a `tsserver` executable, `tsserver.js`, or a `typescript` package directory). Each
  package's installed `node_modules` is used to resolve imports, at both revisions.

Navigation uses [Jedi](https://github.com/davidhalter/jedi) for Python and TypeScript's
`tsserver` for TypeScript and JavaScript. For a branch or PR, the source files of each revision
(plus `package.json` and `tsconfig*.json`) are written to a temporary snapshot on first use (well under a second
for a ~1,300-file repo) and deleted when the server stops. If the virtualenv has the project
installed in editable mode, its paths are pointed at the snapshot, so imports resolve to the
code at that revision rather than your current checkout.

## How it works

1. **Load the change set.** Uses `git diff --name-status -M` between the merge-base and head,
   and reads file contents with `git cat-file --batch`.
2. **Split into change units.** A line-level diff is taken per file (blank lines never anchor
   a match, so an import block isn't torn apart to pair a blank line). When a replaced block
   has the same number of lines on each side, the lines are paired one by one. Otherwise the
   block stays whole, for example when a call is re-wrapped across lines.
3. **Classify each unit.** Old and new tokens (from `tokenize`, with `ast` for type
   annotations) are aligned, and every differing span becomes a signature:

   | Kind | Example |
   |---|---|
   | `rename` | `get_user(id)` → `fetch_user(id)`. Records the context: definition, call, attribute, import, keyword argument or name |
   | `retype` | `def f(x: int)` → `def f(x: str)`, `-> List[int]` → `-> list[int]`, adding an annotation |
   | `replace` | any other repeated substitution, e.g. `cfg.get("timeout")` → `settings.timeout` |
   | `formatting` | only whitespace or layout changed (quote style, re-wrapping, indentation) |
   | `docs` | only comments or docstrings changed |
   | `args` | a call or def changed the shape of its argument list: `fetch(x, y)` → `fetch(x, y, timeout=5)` is `fetch: +kw:timeout` whatever the value, and groups with the `def` that gained the parameter. Also removed keywords, added positionals and positional → keyword conversions. An edit inside an argument's value is never swallowed |
   | `import` | import lines that only changed module (`from a import x` → `from b import x`), or added / removed a name |
   | `move` | a block deleted in one place and inserted in another (see below) |

   Nearby edits that belong together become one template instead of fragments. This
   applies when an edit opens a bracket that a later edit closes, or when two edits are
   separated only by `.` or `=`. Unchanged names and literals in between become `…`, so
   `role="admin"` → `roles=("admin",)` and `role="faculty"` → `roles=("faculty",)` share the
   pattern `role=… → roles=(…,)`, and `actor` → `str(actor.user_id)` reads as `… → str(….user_id)`.

4. **Group.** Units are grouped by signature. A pattern is *mechanical* when it repeats at
   least **Min repeats** times (default 2). A unit counts as explained only when every
   signature on it is mechanical. So a line that renames `get_user` *and* changes logic still
   shows up in **Needs review**.
5. **Moved code.** After every file is diffed, deletion-only and insertion-only blocks (at
   least 3 lines / 12 tokens) are compared by their token streams, ignoring layout and
   comments. Exact matches pair first, then near matches (≥ 75% similar), across files or
   within one. When only one function out of a deleted block moved, the block is split at
   statement boundaries so the rest still shows up for review. A pair becomes a `move`
   pattern, which is always mechanical: an exact move disappears from review, and a move with
   edits inside leaves only those edits, classified like any other change and highlighted on
   the moved block. An `import` change that only follows a move — the importer now points at
   the new module, or the destination gained an import the block needs — is folded into the
   move. Not detected: a block that replaces other code in the same hunk (that is one
   `replace`, not a deletion plus an insertion).

   A move is *certain*, and only then hidden by the **Moved functions** filter, when all of
   these hold: the token streams match exactly and no comment changed; the block is made only
   of whole functions, methods or classes; they keep their qualified names (a method moved to
   another class, or a function turned into a method, is not certain); and the same code was
   not deleted or inserted anywhere else, so there is no other way to pair it. Anything less
   is still shown as a move, but stays visible under that filter.

6. **Verification.** For every statement (function, method, top-level statement) whose
   changes are all formatting, docs, rename or retype, the old and new versions are parsed and
   compared after normalization: docstrings dropped, the diff's mechanical renames applied to
   the old side, annotations dropped when the statement has a retype. Equal trees mark every
   unit inside the statement **✓ verified**. It is a property of the whole statement, so a
   function with one rename and one logic change verifies neither. Verification never changes
   whether a unit is collapsed; it only says which collapsed changes are safe beyond doubt.

7. **Sanity checks.**
   - *Missed renames*: a definition (`def`/`class`) was renamed, the old name is no longer
     defined anywhere in the repository at head, yet code still references it. This produces
     one warning per rename, listing every location. Renamed locals, parameters, keyword
     arguments and builtins are not checked: other variables with the same name are usually
     unrelated.
   - *Inconsistent renames*: a definition or import was renamed to different names in
     different places.
   - *Near misses*: a leftover change whose signature almost matches a mechanical pattern
     (same old name, new name ≥ 80% similar, or vice versa; the same rule on the text of
     templates and types). The unit is tagged **≈ almost …** and one warning lists every
     near miss of a pattern.

## Development

```bash
uv run pytest
uv run ruff check . && uv run ruff format --check .
```

With [just](https://github.com/casey/just) installed, `just` lists the project's recipes:
`just check` runs the linters and tests, `just fmt` formats, `just run main..HEAD` starts
the CLI from the checkout, and the `desktop-*` recipes wrap the desktop app's build steps.

Layout (`src/refactor_diff/`):

| Module | Purpose |
|---|---|
| `sources.py` | git, GitHub PR and working-tree loading; commit lists; posting PR comments via `gh` |
| `hunks.py` | line diff, hunk grouping, candidate units |
| `languages/` | language analyzers (`base.py` interface, `python.py`, `typescript.py`) |
| `patterns.py` | token alignment, signature classification |
| `grouping.py` | grouping, mechanical threshold, warnings (inconsistent renames, near misses) |
| `moves.py` | moved-code detection and the import churn a move explains |
| `verify.py` | AST-equivalence verification of collapsed changes |
| `categories.py` | file kinds (source, tests, docs, config, other) for filtering |
| `engine.py` | `analyze()`, which turns a source into a `Report` |
| `state.py` | reviewed marks and the previous analysis, in `~/.config/refactor-diff` |
| `export.py` | the Markdown review summary |
| `fileview.py` | whole-file diff of one changed file, for context and the old/new viewer |
| `snapshots.py` | analyzable sources of a revision written to a temp dir, for navigation |
| `navigation.py` | go-to-definition / find-references, dispatched by language; Jedi backend |
| `tsserver.py` | TypeScript/JavaScript navigation backend (tsserver) |
| `model.py` | serializable report model with stable IDs |
| `web/` | Starlette server and the vanilla-JS single-page UI |

The macOS app lives in `desktop/`: a [Tauri](https://tauri.app) shell (`src-tauri/`, Rust)
that runs the backend frozen by PyInstaller (`sidecar.spec`) as a child process and shows
its UI in a webview; `ui/index.html` is the landing page with the repository picker. To
iterate on Python code without re-freezing, point the app at the checkout:
`REFACTOR_DIFF_SIDECAR=$PWD/scripts/sidecar-dev.sh npm run dev`.

### Adding a language

Implement the `LanguageAnalyzer` protocol in `languages/base.py`. It turns source text into
tokens and type-annotation spans, names the language's keywords, and lists the `globs` used
to search the repository for missed renames. Optionally it also exposes statement spans, call
sites, defs and imports, and can parse and normalize a block for verification; an analyzer
that leaves those empty simply opts out of moves, `args`, `import` and verification. Then
register the analyzer in `languages/__init__.py`. The rest of the analysis pipeline does not
depend on the language.

For the UI, add a highlighter to `LANGUAGES` in `web/static/syntax.js`, and for code
navigation a backend like `tsserver.py` that `Navigator` in `navigation.py` dispatches to
(and the file suffixes to `SNAPSHOT_SUFFIXES` in `snapshots.py`).

## Roadmap

- Apply or revert edits in the working tree from the UI
- More languages (Go, …)
