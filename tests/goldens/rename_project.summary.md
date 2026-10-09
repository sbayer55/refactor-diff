# refactor-diff: main...feature

`0123456` → `89abcde` · **94%** of changed lines collapsed · **1** change to review · 4 mechanical patterns · 6/7 files analyzed · 9 verified by AST

## Mechanical patterns

| | Kind | Pattern | Count | Files |
|---|---|---|---|---|
| ✓ | rename | `get_user → fetch_user` | 8 | 4 |
|  | retype | `int → str` | 4 | 3 |
|  | replace | `cfg.get("timeout") → settings.timeout` | 2 | 2 |
|  | formatting | `Whitespace / layout only` | 4 | 3 |

## Needs review

- [x] `api.py:6` — `if user is None or not user.get("active"):`

## Warnings

- **missed-rename**: get_user was renamed to fetch_user and is no longer defined, but 2 references remain (`legacy.py:1`, `legacy.py:5`)
