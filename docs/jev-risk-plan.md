# Risk scoring with Jev

The needs-review list is what is left after the engine has collapsed the repeated mechanical
edits. It is in file order, and nothing in it says which hunk deserves the reviewer's attention
first. "Score with Jev" asks TypeSafe AI's Jev, a decision model that returns probabilities
rather than prose, a fixed set of typed questions about each leftover hunk, and uses the
answers to rank the list, mark each hunk and fill a RISK section in the inspector.

## Why a third category

Three kinds of annotation now exist and they never mix:

- **Refactor warnings** (`report.warnings`): what the deterministic engine found (missed
  renames, near misses). Part of the report JSON, pinned by goldens.
- **Code issues**: squiggly diagnostics from the language analyzers, served per file and never
  shown in the Warnings tab or counted in the status bar.
- **Risk scores**: a classifier's opinion. Probabilistic, external, paid for, and only present
  when the user asked for it. Served from `/api/report/{id}/jev`, kept in the inspector's
  RISK section, the hunk-bar chip and the sort order. They never enter the report JSON or
  `report.warnings`, and a score is shown with its probability so it reads as a hint, not a
  verified fact.

## Provider

Vercel AI Gateway only: `POST {base_url}/v1/evaluate` with a bearer key, model
`typesafe-ai/jev`, `providerOptions.gateway.only = ["typesafe-ai"]` and an optional
`zeroDataRetention`. The client (`ai/jev/mod.rs`) retries once on 429 and 5xx (honouring
`Retry-After` up to 10 s), gives the whole call 60 s, and turns the gateway's
`{"error": {"message"}}` into a readable sentence. A 401/403 aborts the run instead of failing
every hunk.

Settings live in their own `jev` section of `settings.json` (`api_key`, `model`, `base_url`,
`auto`, `zero_data_retention`), independent of the Ask provider. The section is not part of
the stored defaults, so a file never touched by Jev keeps the bytes the Python tool wrote.

## What is sent

One request per residual hunk, nothing for collapsed pattern members. The state object is
built by `ai/jev/state.rs` from what the tool already knows:

```json
{
  "path": "api/auth.py", "category": "source", "language": "python",
  "docs_only": false, "adds_tests": false,
  "changed_test_files": ["tests/test_auth.py"],
  "pr_title": "…" , "patterns": ["rename get_user → fetch_user ×42 in 17 files"],
  "stated_change": "Remove program",
  "pr_body": "<first 2000 characters of the PR description>",
  "enclosing": {"kind": "def", "qualname": "login", "before": "…", "after": "…"},
  "hunk": "<numbered unified diff with [[pattern: …]] annotations>"
}
```

`adds_tests` and `docs_only` are computed, not asked: a file's category and the units'
signatures already say so. `stated_change` is the PR title when there is one, else
``removes `{stem}` `` for the strongest concept in `report.concepts`, else left out; `pr_body`
is left out when the PR has no description. The enclosing function is capped at 120 lines per
side; up to 300 unscored hunks are sent per run, four at a time.

When `report.concepts` is non-empty, hunks whose every unit is in the strongest concept's
`explained_unit_ids` are sent last, and when more than 300 hunks are waiting they are not
sent at all (`select_hunks` in `job.rs`). Without a concept the order is the report's.

## The questions (`ai/jev/questions.rs`, version 2)

| name | type | answer |
|---|---|---|
| `hardcoded_secret`, `injection_risk`, `touches_auth`, `weakens_tests`, `breaks_api`, `data_migration`, `debug_leftovers`, `removes_guard_or_validation` | boolean | probability, with `true`/`false` criteria; these eight are `BOOLEAN_CONCERNS` |
| `behavior_beyond_stated_change` | boolean | does the hunk change behaviour beyond `stated_change`? Mechanical consequences of it (removing a field the change removes, deleting code that used it) are not beyond it. Not a concern: it only nudges `risk` |
| `blast_radius` | score | 0 none · 1 local · 2 module · 3 core |
| `reviewer_effort` | score | 0 glance · 1 careful read · 2 expert |
| `primary_concern` | choice | secret, injection, auth, api_contract, test_weakening, debug, migration, guard_removed, logic, none, insufficient_evidence |
| `change_role` | choice | core (what the PR is about), cascade (call sites, types, props forced by it), cleanup (dead code, imports, comments left by it), test-fixture (tests, mocks, fixtures, stories), collateral (anything else that changes behaviour) |

`description_matches` and `merge_ready` are deliberately not asked. Changing any wording or
criteria means bumping `QUESTION_SET_VERSION`, which invalidates the cache (version 1 records
are discarded on load).

## Scoring (`ai/jev/risk.rs`)

```
concern_max = max p over the eight BOOLEAN_CONCERNS
scope       = (blast / 3) × (0.5 + 0.5 × effort / 2)
base        = 0.6 × concern_max + 0.4 × scope
risk        = base + 0.25 × p_beyond × (1 − base)      (0..1; W_BEYOND = 0.25)
confidence  = probability of the chosen primary_concern (the endpoint returns none)
flagged     = concern_max ≥ 0.8
              or primary_concern ∉ {none, insufficient_evidence} and confidence ≥ 0.6
level       = high if flagged and risk ≥ 0.6; medium if flagged or risk ≥ 0.3; else low
```

`behavior_beyond_stated_change` pulls `risk` part of the way towards 1 instead of feeding
`concern_max`, so it reorders hunks but never flags one by itself; with `p_beyond = 0` the
formula is the version 1 one. Every constant lives at the top of `risk.rs`. The record kept
per hunk (`HunkRisk`) carries the raw answers too, so the formula can change without
re-asking; it also exposes `behavior_beyond_stated_change`, `removes_guard_or_validation`
(probabilities), `change_role` (`null` without an answer) and `change_role_confidence`.

## Group verification (`ai/jev/groups.rs`)

After the hunks, every mechanical group with at least three units is checked: its first,
middle and last member are sampled and Jev is asked one question
(`group_questions()`, `GROUP_QUESTION_SET_VERSION` 1):

| name | type | answer |
|---|---|---|
| `same_edit` | boolean | are all sampled occurrences the same mechanical edit, with no occurrence changing behaviour differently? |

The state is `{"pattern": {"kind", "label", "old", "new"}, "occurrences": [hunk text per
sampled unit], "stated_change"?}`, with each occurrence rendered like `hunk` above. Up to 100
unchecked groups are sent per run, four at a time, and they count towards the job's progress.
A group with `same_edit ≥ 0.8` adds its units to `jev_verified_units`. This is a hint next to
the risk scores: `Unit.verified`, the stats and the report JSON are never touched.

## Cache and job

`<config dir>/jev/<repo hash>.json` holds `{"question_set", "hunks": {fingerprint: HunkRisk},
"group_question_set", "groups": {cache key: GroupCheck}}`, newest 5000 records of each.
Keying by hunk fingerprint means a score survives line shifts, re-analysis and new commits,
and disappears when the hunk's content changes. A group's cache key hashes `group.key` with
the sorted fingerprints of its sampled members' hunks. Each section is discarded on its own
when its version changes. The review marks file is untouched so `review.sample.json` stays
byte-identical.

`ai/jev/job.rs` runs one job per report id: cached hunks and groups are filled in at once,
the rest are sent concurrently (hunks first, then groups), results are flushed to the cache
every ten records and at the end, and every await also watches the server's shutdown token.
An auth failure stops the run before the groups. The UI polls `GET /api/report/{id}/jev`
once a second while the status is `running`.

## Endpoints

- `POST /api/report/{id}/jev/score` body `{"force": bool}`: start (or return) the job; `force`
  ignores both caches and restarts a running job. 400 with `settings: true` without a key.
- `GET /api/report/{id}/jev`: `{"status", "hunks": {hunk_id: HunkRisk}, "groups": {group_id:
  {"same_edit", "sampled": [unit ids]}}, "jev_verified_units", "failures", "configured",
  "auto", "question_set", "group_question_set"}`; with no job, the cached scores and group
  checks for the report. `status` while running is `{"state": "running", "done", "total"}`
  (hunks plus groups sent this run); when done it adds `scored`/`cached` (hunks),
  `groups_checked`/`groups_cached`, `failed` and an optional `note`. A failed group check is
  listed in `failures` with its `group_id` and the hunk of its first sampled member.
- `POST /api/settings/test` with `provider: "jev"` runs a one-question probe.
- `POST /api/analyze` starts a job by itself when `jev.auto` is on and a key is set; the
  analysis response is unchanged.

## UI

- Status bar: "Score with Jev" (idle), "Jev 3/12…" (running), "N risk flags" (done).
- `g s`, the palette ("Score with Jev", "Rescore with Jev", "Sort needs review by risk/file").
- Hunk bar: a `low`/`medium`/`high` chip with the primary concern and the risk percent; the
  tooltip lists every concern probability and the blast/effort labels.
- Inspector RISK section: the top four flagged hunks among the visible ones, with concern
  bars and `view`/`ask` links; `ask` opens the Ask menu with a prompt about what Jev found.
- "Sort by risk" (page head and RISK section; preference `refactor-diff:risk-sort`): riskiest
  hunks first within a file, riskiest files first. File order stays the default.

## Interpreting the numbers

Jev's own guidance applies: a boolean near 0 is a confident no, near 0.5 is unknown;
`confidence` describes how concentrated the choice distribution is, not whether it is right;
a score is a probability-weighted rung, not a probability of anything. Before letting any
threshold hide or auto-approve content, compare flagged hunks against what reviewers actually
marked or commented on.
