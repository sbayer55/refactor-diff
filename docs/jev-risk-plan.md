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
  "enclosing": {"kind": "def", "qualname": "login", "before": "…", "after": "…"},
  "hunk": "<numbered unified diff with [[pattern: …]] annotations>"
}
```

`adds_tests` and `docs_only` are computed, not asked: a file's category and the units'
signatures already say so. The enclosing function is capped at 120 lines per side; up to 300
unscored hunks are sent per run, four at a time.

## The questions (`ai/jev/questions.rs`, version 1)

| name | type | answer |
|---|---|---|
| `hardcoded_secret`, `injection_risk`, `touches_auth`, `weakens_tests`, `breaks_api`, `data_migration`, `debug_leftovers` | boolean | probability, with `true`/`false` criteria |
| `blast_radius` | score | 0 none · 1 local · 2 module · 3 core |
| `reviewer_effort` | score | 0 glance · 1 careful read · 2 expert |
| `primary_concern` | choice | secret, injection, auth, api_contract, test_weakening, debug, migration, logic, none, insufficient_evidence |

`description_matches` and `merge_ready` are deliberately not asked. Changing any wording or
criteria means bumping `QUESTION_SET_VERSION`, which invalidates the cache.

## Scoring (`ai/jev/risk.rs`)

```
concern_max = max p over the seven booleans
scope       = (blast / 3) × (0.5 + 0.5 × effort / 2)
risk        = 0.6 × concern_max + 0.4 × scope          (0..1)
confidence  = probability of the chosen primary_concern (the endpoint returns none)
flagged     = concern_max ≥ 0.8
              or primary_concern ∉ {none, insufficient_evidence} and confidence ≥ 0.6
level       = high if flagged and risk ≥ 0.6; medium if flagged or risk ≥ 0.3; else low
```

Every constant lives at the top of `risk.rs`. The record kept per hunk (`HunkRisk`) carries
the raw answers too, so the formula can change without re-asking.

## Cache and job

`<config dir>/jev/<repo hash>.json` holds `{"question_set", "hunks": {fingerprint: HunkRisk}}`,
newest 5000 records. Keying by hunk fingerprint means a score survives line shifts, re-analysis
and new commits, and disappears when the hunk's content changes. The review marks file is
untouched so `review.sample.json` stays byte-identical.

`ai/jev/job.rs` runs one job per report id: cached hunks are filled in at once, the rest are
sent concurrently, results are flushed to the cache every ten hunks and at the end, and every
await also watches the server's shutdown token. The UI polls `GET /api/report/{id}/jev`
once a second while the status is `running`.

## Endpoints

- `POST /api/report/{id}/jev/score` body `{"force": bool}`: start (or return) the job; `force`
  ignores the cache and restarts a running job. 400 with `settings: true` without a key.
- `GET /api/report/{id}/jev`: `{"status", "hunks": {hunk_id: HunkRisk}, "failures",
  "configured", "auto", "question_set"}`; with no job, the cached scores for the report.
- `POST /api/settings/test` with `provider: "jev"` runs a one-question probe.
- `POST /api/analyze` starts a job by itself when `jev.auto` is on and a key is set; the
  analysis response is unchanged.

## UI

- Status bar: "Score with Jev" (idle), "Jev 3/12…" (running), "N risk flags" (done).
- `g s`, the palette ("Score with Jev", "Rescore with Jev", "Sort needs review by risk/file").
- Hunk bar: a `low`/`medium`/`high` chip with the primary concern and the risk percent; the
  tooltip lists all seven probabilities and the blast/effort labels.
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
