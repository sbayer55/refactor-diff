# Concept lens and review-filter expansion: shared contract

This note pins the data shapes several workstreams build against at the same time. Motivation:
on a "remove concept X" PR (vertex-ui#1310, removing `program`), most residual units are pure
deletions of the concept and only ~20 are real collateral changes. These features cut the
review down to that collateral.

## Ground rules

- **The concept lens is an overlay.** `Unit::explained`, `stats` and `residual_hunk_ids` keep
  their meaning. Concept data lives in `report.concepts`, which is serialized only when
  non-empty.
- **Code issues stay separate from refactor warnings.** Cross-reference checks are code issues
  (`xref-*` codes, `--issue-*` colors) and never enter `report.warnings`.
- **Jev data stays out of the report JSON.** It is served from `/api/report/{id}/jev` only.
- `just check` must stay green on every branch.

## Report additions (core `model.rs`)

### New `SignatureKind` values

| kind | meaning | mechanical |
|---|---|---|
| `substitution` | `a → b` where `b` already existed in the base | never |
| `list` | elements added to or removed from one bracketed list, plus the formatter reflow | always |
| `guard` | an early `return`/`throw` guard removed or added | never |
| `cascade` | an import or local dropped because its last use was dropped | always |
| `test` | a whole `it`/`test`/`describe` block deleted or added | by count |
| `i18n` | an i18n key plus the `t("key")` calls that used it | by count |
| `fixture` | the same JSON/object field path dropped across records | always |

- `SignatureKind::never_mechanical()` wins over both `always_mechanical()` and `min_count`
  (grouping.rs).
- For the new kinds, `Signature::label()` prints `delete {old}` / `insert {new}` when the
  other side is empty, and `old → new` otherwise. A workstream controls the label through
  `old`/`new`.

### New `UnitTag` values

`precondition-removed` and `test-removed`.

### `report.concepts`

`report.concepts` is a `Vec<Concept>`, strongest first:

```jsonc
{
  "stem": "program",
  "auto": true,                 // false when asked for via --concept / UI
  "score": 0.71,                // share of changed identifier tokens containing the stem
  "explained_unit_ids": ["…"],  // diff is empty once the concept is stripped
  "touched_unit_ids": ["…"],    // mentions the concept but still differs
  "patterns": [
    {"norm_key": "replace\u0000programId: <str>…", "label": "delete programId: <str>",
     "role": "field", "group_ids": ["…", "…"]}
  ],
  "files": {"src/a.ts": {"concept_lines": 12, "collateral_lines": 3}}
}
```

`role` is one of `field|import|fixture|i18n|prop|deps|type|test|other`.

### Engine

- `analyze_with(source, changes, min_count, head, &AnalyzeOptions { concept })`.
  `analyze(...)` is the same call with default options.
- The report id hashes `(base_sha, head, min_count)`, plus `concept` only when one is set.
- Post-passes, in order, all before `build_groups`:

  | call | module | workstream |
  |---|---|---|
  | `substitution::mark` | `substitution.rs` | B |
  | `cascade::link` | `cascade.rs` | C2 |
  | `testblocks::tag` | `testblocks.rs` | C2 |
  | `barrels::link` | `barrels.rs` | C2 |
  | `json_links::link` | `json_links.rs` | D |

  After `build_groups`, `concepts::detect` (`concepts.rs`, workstream A) runs.

## Server additions

- **`POST /api/analyze`** accepts `concept` (string, optional). The CLI takes `--concept <name>`
  and exposes it as `defaults.concept`.
- **`--hide collateral-explained`** maps to `Filters.hideConceptExplained`: the UI hides units
  in the active concept's `explained_unit_ids`.
- **`GET /api/report/{id}/xref?concept=<stem>`** returns:

  ```jsonc
  {"checks": [{"code": "xref-dangling", "severity": "warning", "path": "src/a.ts",
               "line": 12, "message": "`currentProgram` still referenced after removal"}]}
  ```

  Codes are `xref-dangling`, `xref-i18n-orphan`, `xref-storage-shape` and `xref-payload-drop`.
  Severity uses the `issues.rs` `Severity` strings.
- **`GET /api/report/{id}/jev`** keeps its current fields and adds:
  - `hunks[*].change_role`: `core|cascade|cleanup|test-fixture|collateral` (question set v2)
  - `hunks[*].behavior_beyond_stated_change` and `removes_guard_or_validation` (probabilities)
  - `groups`: `{group_id: {same_edit: p, sampled: [unit ids]}}`
  - `jev_verified_units`: the units in groups with `same_edit ≥ 0.8`. `Unit.verified` is never
    touched.

## Workstream file ownership

| | workstream | owns |
|---|---|---|
| A | concept lens engine | `core/src/concepts.rs`, `core/tests/concepts.rs` |
| B | correctness fixes | `classify/imports.rs`, `classify/rename.rs`, `grouping.rs` (below the mechanical line), `substitution.rs`, new tests in `core/tests/classify_fixes.rs` |
| C1 | in-classifier patterns | `classify/mod.rs`, `classify/lists.rs`, `classify/guards.rs`, `core/tests/patterns_c1.rs` |
| C2 | cross-unit patterns | `cascade.rs`, `testblocks.rs`, `barrels.rs`, barrel helpers in `moves.rs`, `core/tests/patterns_c2.rs` |
| D | JSON + i18n/fixtures | `lang/json/`, the JSON line in `lang/mod.rs`, `json_links.rs`, `core/tests/json_links.rs` |
| E | xref checks | `server/src/xref/`, `http/routes/xref.rs`, its route line in `http/mod.rs` and `routes/mod.rs`, `grep_lines` in `git/sources.rs`, `server/tests/xref_api.rs` |
| F | Jev upgrades | `server/src/ai/jev/*`, `http/routes/jev.rs`, `pr_piece` in `ai/context.rs`, `tests/jev_*.rs`, `docs/jev-risk-plan.md` |
| G | CLI + options | `crates/refactor-diff/src/main.rs`, `server/src/config.rs`, `http/routes/analyze.rs` |
| H | UI (after integration) | `assets/*`, `http/assets.rs` |

`model.rs`, `engine.rs`, `lib.rs` and `Cargo.toml` are frozen during Phase 1. If you need a
contract change, say so in your final report instead of editing them.
