# Golden files for the Rust port

These files pin the observable behaviour of the Python `refactor_diff` package so the
Rust rewrite can be checked against it after the Python code is deleted.

They were produced by `tests/goldens/generate.py`, a script that ran the Python package
with git stubbed out. The Python package and the script were removed once the Rust crates
reached parity; both are in the git history (last at the commit that deleted `src/refactor_diff`)
and can be checked out from there if a golden ever needs regenerating.

Generated with CPython `3.14.5 (main, Jun  2 2026, 22:28:56) [Clang 22.1.3 ]`.

All JSON written by the generator uses sorted keys, 2-space indent, `ensure_ascii=False`
and a trailing newline. `settings.sample.json` and `review.sample.json` are the raw bytes
the package itself wrote (`json.dumps(indent=1, sort_keys=True)`, no trailing newline).

## Files

- `seqmatch.json`: `difflib.SequenceMatcher(isjunk, a, b, autojunk=False)` cases. `junk` is
  `none` (`isjunk=None`) or `blank` (`isjunk=lambda s: not s.strip()`, as `hunks.line_matcher`
  uses). Each case records `get_opcodes()`, `get_matching_blocks()` (sentinel included),
  `ratio()`, `quick_ratio()` and `get_grouped_opcodes(3)`. Hand-picked cases first, then
  `random.Random(1234)` cases over small line alphabets (both junk modes), character-level
  cases (as `grouping._sim` compares) and token-stream cases (as `moves.py` compares).
- `python_tokens.json`: `PythonAnalyzer().analyze(source)` for each snippet: the token
  stream (kind/value/text/start/end; positions are `[line, col]`, 1-based line, 0-based
  character column), annotations, docstring spans, statement spans, call sites, def sites
  and import sites. `parsed` is false when `tokenize` failed and the regex fallback
  tokenizer was used; when `ast.parse` fails too, the structural lists are empty.
- `builtins.json`: `sorted(dir(builtins))`, `keyword.kwlist`, `keyword.softkwlist`.
- `rename_project.report.json`, `ts_rename_project.report.json`:
  `engine.analyze(repo, "main", "feature").to_dict()` for `tests/fixtures/<project>`,
  with git stubbed out: `sources.resolve` returns
  `ResolvedSource(label="main...feature", base="main", head="feature", base_sha="0123456789abcdef0123456789abcdef01234567",
  head_sha="89abcdef0123456789abcdef0123456789abcdef", pr=None)`; `sources.load_changes` diffs `before/` against
  `after/` (files in both are `M` even if identical, exact-content matches between a
  deleted and an added file become `R`, NUL-containing files are skipped);
  `sources.grep_files` scans `after/` files matching the globs for the word between
  non-word characters (git grep `-w -F`); `sources.read_file_at` reads from `after/`.
- `rename_project.worktree.report.json`: the same analysis with `head=":worktree:"`
  (`head_sha=None`, label `main → working tree`, identity `worktree:main`), `min_count=2`.
- `rename_project.summary.md`: `export.markdown_summary(report, {"groups": [<first
  mechanical group id>], "hunks": [<fingerprint of the first residual hunk>]})`.
- `rename_project.fileview.json`: `fileview.file_diff(report, "api.py")`.
- `settings.sample.json`: the bytes `settings.save(data, root)` wrote for
  `data = {"ai": {"context": {"references": true}, "provider": "ollama", "providers": {"claude": {"api_key": "sk-ant-abcdef1234"}, "ollama": {"model": "llama3", "num_ctx": 65536}, "openai": {"base_url": "http://localhost:1/v1", "model": "m"}}}}`.
- `settings.public.json`: `settings.public_view(settings.load(root))` for that file.
- `review.sample.json`: the bytes written by `state.ReviewStore(Path("/srv/example-repo"), state_dir)`
  after `record_analysis("refs:main:feature", "aaaa", {"fp1": "a.py", "fp2": "b.py"})`,
  `mark(identity, groups={"add": ["g1", "g2"]}, hunks={"add": ["fp1"]})`, then
  `record_analysis(identity, "bbbb", {"fp2": "b.py", "fp3": "c.py"})`, with
  `state.datetime.now()` pinned to `2026-10-08T12:00:00+00:00`. The store names the file
  `short_hash(str(repo.resolve()))`: `b0962e9729bb.json` for `/srv/example-repo`.
  (`/tmp/...` was avoided because `Path.resolve()` turns it into `/private/tmp/...` on macOS.)
- `review.expected.json`: the return values of those three calls, plus
  `review(identity, final_hunks)` and `_delta(entry, final_hunks)` for the final state.
- `short_hash.json`: `model.short_hash(*parts)` cases. Parts are joined with `\x1f` after
  `str()`, SHA-1 hashed as UTF-8, first 12 hex digits (so `1` and `"1"` hash the same).

## Sizes at generation time

- `builtins.json`: 3356 bytes
- `python_tokens.json`: 280014 bytes
- `rename_project.fileview.json`: 2535 bytes
- `rename_project.report.json`: 32661 bytes
- `rename_project.summary.md`: 721 bytes
- `rename_project.worktree.report.json`: 32629 bytes
- `review.expected.json`: 1108 bytes
- `review.sample.json`: 399 bytes
- `seqmatch.json`: 98202 bytes
- `settings.public.json`: 706 bytes
- `settings.sample.json`: 446 bytes
- `short_hash.json`: 667 bytes
- `ts_rename_project.report.json`: 32208 bytes

## Deliberate divergences of the Rust port

Every golden above is matched exactly by the Rust crates (`cargo test`), except where noted:

- Files the Python tokenizer could not tokenize (`parsed: false` snippets in
  `python_tokens.json`: `syntax_error_unclosed_paren`, `indentation_error`,
  `unterminated_triple_quote`) were tokenized by Python's regex fallback, which emits no
  structural tokens and mis-lexes an unterminated `"""`. The Rust analyzer tokenizes them
  from tree-sitter's error-recovering parse instead; the test only requires `parsed == false`,
  empty structure and the golden's name/number tokens to be present.
- Import classification: where Python picked `same[0]` / `renamed[0]` out of unordered sets,
  the Rust port iterates the added bindings sorted by `(alias, module, name)`. Identical on
  every fixture; deterministic where Python was hash-order dependent.
- `normalized_dump` (AST verification) also renames identifiers inside `match` capture
  patterns and type-parameter names (Python's `_Normalize` left `MatchAs.name` untouched),
  and `parse_block` rejects inconsistent dedents like CPython's `IndentationError`. Both are
  only ever more permissive about *parsing* and never mark a different program as verified.
- TypeScript string values: a lone-surrogate escape such as `"\ud83d"` keeps its raw text as
  the token value (Rust strings cannot hold a lone surrogate); Python produced `'\ud83d'`.
- `settings.json` and review files are written as UTF-8 escaped to ASCII exactly like
  `json.dumps`; the Rust writer produces the same bytes for every sample here.
