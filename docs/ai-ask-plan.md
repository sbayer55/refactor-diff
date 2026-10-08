# Implementation plan: the Ask (AI) menu

Design 3 from the mockups (https://claude.ai/artifact/LWUVPSHaA1811yChVJvCSu): an **Ask** link in
every hunk bar and an Ask badge on hovered diff lines, a menu styled like the sidebar with
eight predefined tasks plus a custom prompt, answers rendered in a band under the hunk with
line citations that highlight rows in the diff, a provider chip in the top bar, and a settings
dialog for Claude, an OpenAI-compatible endpoint, and Ollama.

Decisions already made: read-only answers, answers live only for the browser session,
surfaces are Needs review hunks and their lines, settings in
`~/.config/refactor-diff/settings.json`.

## 0. Two things to settle first

**Cursor has no completions API.** Cursor's API docs list Admin, Analytics, Bugbot, Cloud
Agents and SDKs, and state that none of them is a model-inference or chat-completions API.
Community probes of `/v1/chat/completions` on `api.cursor.com` return 404; the only way to
reach Cursor's models from outside the editor is an unofficial local proxy. The plan therefore
ships a third provider called **OpenAI-compatible endpoint** (base URL, key, model). It covers a
Cursor proxy if you run one, and also OpenRouter, LM Studio, vLLM, and Ollama's own
`/v1` endpoint. If an official Cursor endpoint appears, it becomes a preset base URL on that tab.

**Default Claude model is `claude-opus-5-5`** with adaptive thinking left at the API default and
`effort: "medium"`. The model field is editable, so Sonnet or Haiku are one edit away.

## 1. Architecture

```
browser (app.js)                         server (Starlette)                    providers
───────────────────────────────────      ──────────────────────────────────    ─────────────
Ask link / line badge ─► ask menu ──POST /api/report/{id}/ai/ask──► ai/context.py builds   ──► ai/providers.py
                                         (SSE stream)               the prompt from the       Claude SDK stream
answer band ◄── delta events ◄───────────────────────────────────── Report, texts, nav,       Ollama /api/chat
cite chips highlight rows                                            git, gh                  OpenAI-compatible
top-bar chip ◄─GET /api/settings─── settings.py (settings.json, 0600, masked keys)
settings dialog ─POST /api/settings, POST /api/settings/test
```

Nothing is sent to a provider until the user picks a task. The server stays bound to
127.0.0.1. Answers are never written to disk.

## 2. Backend

### 2.1 `src/refactor_diff/settings.py` (new)

- `load() -> dict` and `save(data) -> None` on `config_dir() / "settings.json"` (reuse
  `config_dir()` from `state.py`; same XDG rules, so the existing `isolated_config` test
  fixture isolates it). Write via temp file + `os.replace`, then `chmod 0600`.
- Schema (all keys optional, defaults filled in by `load`):

```json
{
  "ai": {
    "provider": "claude",
    "providers": {
      "claude": {"api_key": "", "model": "claude-opus-5-5", "base_url": ""},
      "openai": {"base_url": "", "api_key": "", "model": ""},
      "ollama": {"host": "http://127.0.0.1:11434", "model": "", "num_ctx": 32768}
    },
    "context": {"function": true, "pr": true, "references": false}
  }
}
```

- `public_view(data) -> dict`: same shape with every `api_key` replaced by a mask
  (`"…" + last 4 chars`, or `""` when unset) plus `configured: bool` per provider. The UI sends
  the mask back unchanged to mean "keep the stored key".

### 2.2 `src/refactor_diff/ai/providers.py` (new)

A tiny protocol so the rest of the code never sees SDK types:

```python
class Provider(Protocol):
    name: str          # "claude" | "openai" | "ollama"
    model: str
    def stream(self, system: str, messages: list[dict]) -> Iterator[str]: ...  # text deltas
    def test(self) -> dict: ...  # {"ok": bool, "latency_ms": int, "models": [...], "error": str|None}
```

- `build(settings) -> Provider` picks the active provider and raises `ProviderError` with a
  user-facing message when it is not configured ("Claude needs an API key. Open Settings.").
- **ClaudeProvider**: `anthropic.Anthropic(api_key=..., base_url=... or None)`;
  `client.messages.stream(model=..., max_tokens=16000, system=..., messages=...,
  output_config={"effort": "medium"})`, yield `text_stream`. Omit `thinking` (adaptive by
  default on Opus 5.5). Map `AuthenticationError`, `NotFoundError` (bad model),
  `RateLimitError`, `APIStatusError`, `APIConnectionError` to `ProviderError` messages,
  most-specific first. Check `stop_reason == "refusal"` on the final message and surface it as
  an error event rather than an empty answer. `test()` sends a 1-token request and returns
  latency; models list is the handful of current IDs so the settings dropdown has
  suggestions.
- **OllamaProvider**: `POST {host}/api/chat` with `{"model", "messages": [system + turns],
  "stream": true, "options": {"num_ctx": N}}`; NDJSON lines, yield `message.content` until
  `done`. `test()` calls `GET {host}/api/tags` and returns the model names.
- **OpenAIProvider**: `POST {base_url}/chat/completions`, `Authorization: Bearer`, `stream:
  true`; parse `data:` lines, yield `choices[0].delta.content`, stop at `[DONE]`. `test()`
  calls `GET {base_url}/models` and falls back to a 1-token completion if that 404s.
- HTTP client for the last two: `httpx2` (already pulled in by the `anthropic` package).
  Timeouts: 10 s connect, 120 s read; the stream is closed when the generator is closed.

Dependencies: add `anthropic>=1` to `pyproject.toml`; move `httpx2` from dev to runtime.

### 2.3 `src/refactor_diff/ai/context.py` (new)

Builds everything a task might want from a `Report`, cheaply and deterministically, keyed by
one location:

```python
@dataclass
class Focus:
    hunk: Hunk
    path: str
    side: str        # "old" | "new"
    line: int        # the line the user asked about (hunk anchor line for hunk-level asks)
```

Pieces, each a function returning a dict (or None when unavailable), so tasks and the custom
prompt can pick:

| Piece | Source | Notes |
|---|---|---|
| `hunk` | `report.hunks[id]`, `report.units`, `report.groups` | Every line with its numbers and a marker per unit: `[pattern: rename getUser → fetchUser ×42]`, `[verified]`, `[near miss: …]`. Unexplained lines are what the model should focus on; the system prompt says so. |
| `function` | `languages.analyzer_for(path).analyze(text).statements` on `report.texts[path]` for each side; the innermost `StmtSpan` whose `contains(line, line)` holds, walking `children` | Returns `{"qualname", "old": {"start","end","text"}, "new": {...}}`. The old side uses the renamed-file path via `_side_path`. Cap each side at 400 lines, then say it was cut. The old-side span is found through the hunk's old line numbers (the unit's `old_start`). |
| `references` | `Navigator.references(head_sha, path, line, col)` at the def name of the enclosing function (regex for the last segment of `qualname` on the span's first line) | Only when `function` exists. Up to 50 locations with their line text, grouped by file; counts the rest. Falls back to the old side when the def was deleted. |
| `history` | `git log --format=%H%x1f%s%x1f%b base_sha..head_sha -L{line},{line}:{path}` for the new side; `sources.list_commits` for the whole range | Empty for the working tree. |
| `pr` | `report.source["pr"]` | Add `body` to the `--json` field list in `sources._resolve_pr` so the description is available. |
| `patterns` | `report.groups` | The mechanical patterns as one line each (kind, label, count); always included, it is small and explains the dimmed lines. |

`render(pieces) -> str` turns the chosen pieces into the user message: fenced blocks with a
one-line heading each, in a fixed order, so prompts are stable across requests (and cacheable
on Claude).

### 2.4 `src/refactor_diff/ai/tasks.py` (new)

One `Task` per menu item: `id`, `label`, `group`, `pieces` (which context to include),
`available(ctx) -> str | None` (None when available, otherwise the reason shown greyed out),
`hint(ctx) -> str` (the small text on the right of the menu row), and `prompt`.

| id | label | pieces | unavailable when | hint |
|---|---|---|---|---|
| `explain` | Explain this change | hunk, function, pr, patterns | never | "N unexplained lines" |
| `function` | How does this function work | function (new side only), hunk | no enclosing def | qualname |
| `compare` | Compare old vs new | function (both sides), hunk | no def on both sides | |
| `uses` | Who uses this | references, function, hunk | no def, or navigation unavailable | "N refs" (filled in async) |
| `why` | Why was this changed | history, pr, hunk | working tree with no commits and no PR | "PR #n · N commits" |
| `review` | Review this change | hunk, function, pr, patterns | never | |
| `preserving` | Is this behavior-preserving | function (both sides), hunk | no def on both sides | "verified" tag when the unit is already ✓ verified, dashed "not verified" otherwise |
| `break` | What could break | references, function, hunk | as `uses` | |
| `custom` | Custom prompt | whatever the checkboxes say | never | |

System prompt (shared): what refactor-diff is, how the context is marked up (pattern
markers, verified, near miss), and the output contract:

- Markdown, lead with the answer, short paragraphs or a numbered list, no code restatement.
- Cite lines as `L<n>` for the new side and `O<n>` for the old side, ranges as `L17–18`.
- Name other files as `path:line`.
- When a task is a judgement (review, preserving, break), end with one line that says
  whether anything needs the reviewer's attention.

Each task adds two or three sentences of instruction. Follow-ups reuse the same system prompt
and context, with the prior turns appended as `assistant`/`user` messages.

### 2.5 Routes in `web/server.py`

| Route | Body / query | Returns |
|---|---|---|
| `GET /api/settings` | | `settings.public_view()` plus `active: {provider, model, configured}` |
| `POST /api/settings` | the same shape; masked keys mean keep | the public view |
| `POST /api/settings/test` | `{provider, config}` (unsaved values allowed, masked key means stored key) | `provider.test()` |
| `POST /api/report/{id}/ai/menu` | `{hunk_id, side?, line?}` | `{focus: {path, side, line, qualname}, tasks: [{id, label, group, hint, unavailable}], provider: {name, model, configured}}`; cheap pieces only, no references |
| `POST /api/report/{id}/ai/refs-count` | `{hunk_id, side?, line?}` | `{count}` for the async "N refs" hint; runs navigation in the threadpool |
| `POST /api/report/{id}/ai/ask` | `{task, hunk_id, side?, line?, prompt?, pieces?: {function, references, pr}, history?: [{role, content}]}` | SSE stream |

The ask stream (`StreamingResponse`, `text/event-stream`), events in order:

```
event: meta   data: {"provider":"claude","model":"claude-opus-5-5","focus":{...},"pieces":["hunk","function","pr"]}
event: delta  data: {"text":"..."}          (many)
event: done   data: {"elapsed_ms":2140}
event: error  data: {"message":"..."}       (instead of done)
```

Implementation: `starlette.concurrency.iterate_in_threadpool` over the provider generator;
a client disconnect closes the generator, which closes the HTTP stream. Validation errors
(unknown hunk, task unavailable, provider not configured) return JSON 400 before streaming
starts so the UI can show a toast and open Settings.

`create_app` grows a `provider_factory` keyword (default `providers.build`) so tests inject a
fake. `references` reuse the existing `navigator` instance.

## 3. Frontend (`web/static/app.js`, `app.css`, `index.html`)

### 3.1 State

```js
state.ai = { provider: null, model: "", configured: false };   // from GET /api/settings
state.answers = new Map();   // hunk id -> [{id, task, label, focus, provider, model, status, elapsed, turns: [{role, text}]}]
```

Answers are keyed by hunk id, so `rerender()` and filter changes keep them; they vanish on
reload, which is the "session only" decision. Viewing a single commit's report keeps a separate
map entry because hunk ids differ.

### 3.2 Entry points

- `hunkHtml`: add `<button class="link" data-hunk-ask title="Ask the AI about this hunk (a)">✦ Ask</button>` before Comment, for every report (not PR-gated), and a `<span class="badge-new">N answers</span>` next to the `@@` label when `state.answers` has entries.
- Line badge: `unifiedRows` and `splitRows` get an `ask: true` option (passed only from
  `hunkRows`). For changed lines it adds `<button class="ask-line" data-ask-line data-s="n|o" data-l="21">✦ Ask</button>` inside the text cell, absolutely positioned at the right edge, visible on row hover or focus. No new table column, so tag rows and the split layout are untouched.
- `onContentClick`: `hunkAsk` → `openAskMenu(hunk, anchor)`; `askLine` → same with `side`/`line` from the dataset.
- Keyboard: `a` opens the menu for the focused hunk; `Escape` closes it (add to the `Escape` branch in `onKey`); `KEY_HELP` gains the row. The menu itself handles ↑/↓/Enter/Home/End and type-ahead on the first letter.

### 3.3 The menu (`openAskMenu`)

A single `<div id="ask-menu" role="menu">` appended to `body`, positioned from the anchor's `getBoundingClientRect()` (flipped above when there is no room below), closed on outside click, Escape, scroll of the content, or a route change.

Contents, in the mockup's order:

1. Header: `path:line` in mono, provider pill (`Claude · opus-5-5`) that opens Settings. When no provider is configured the pill reads "Set up AI" and every task row is disabled.
2. Group headers `UNDERSTAND` and `REVIEW AND RISK` in the sidebar's `h4` style; rows in `nav-item` style with the hint on the right. Rows come from `/ai/menu`, requested when the menu opens; the menu renders immediately from a static list and fills hints and disabled states when the response lands (under 50 ms for everything but references). The refs count is requested separately and dropped in when it arrives.
3. Custom prompt box: a contenteditable-free `<textarea>` with a location chip rendered beside it (the chip is a label, the server adds the location), checkboxes Hunk (always on, disabled), Function, References, PR with defaults from settings, and an Ask button; ⌘⏎ submits.

Picking a row calls `ask(hunk, focus, taskId, {prompt, pieces})`.

### 3.4 Asking and streaming (`ask`)

1. Create an answer record with `status: "streaming"`, push it onto `state.answers`, and insert the answer band after the hunk's `.diff` table (`renderAnswers(hunkEl)` re-renders the whole band for that hunk).
2. `fetch` the SSE endpoint with an `AbortController`; read `response.body` with a `TextDecoder`, split on blank lines, dispatch events. `delta` appends to the last assistant turn and re-renders only that answer's body (throttled to animation frames). `done` sets elapsed and status. `error` shows the message inside the band with a Retry link.
3. Stop link aborts the controller; the partial text stays with a "stopped" note.
4. Follow-up field sends the same request with `history` = the answer's turns; the reply is appended as a new turn in the same band.

Only one stream per hunk at a time; the Ask link is disabled while one runs.

### 3.5 The answer band

Markup mirrors the mockup: `.ai-band` (comment-box styling) containing a header row (AI kind badge, task label, provider pill, elapsed, Copy · Post as comment (PR only, fills `openCommentBox` with the text) · Dismiss), one `.ai-answer` card per assistant turn, a `.ai-you` row per user turn, and the follow-up field with the location chip. It sits inside `.hunk`, so marking the hunk reviewed folds it with the diff; the "N answers" badge on the bar stays visible.

Markdown: a small escape-first renderer in app.js (paragraphs, `**bold**`, `` `code` ``, fenced code, ordered and unordered lists, headings rendered as bold lines). Everything is HTML-escaped before the few markdown patterns are applied, so model output cannot inject markup. After rendering:

- `L<n>`, `O<n>` and ranges become `<button class="cite" data-side data-from data-to>`; hover adds `.cite` to the rows of that hunk whose `td.text[data-s][data-l]` fall in the range; click scrolls the first row into view, or opens the file viewer at that line when the row is outside the hunk.
- `path:line` tokens whose path is in `state.report.files` become links to `#file/<path>/n<line>` (the existing file viewer route), matching the mockup's `src/billing.ts:45` links.

### 3.6 Top bar and settings

- `index.html`: add `<a id="ai-chip" class="chip" href="#" title="AI provider · open settings">` after `#repo`, and `<dialog id="settings" class="help modal">`.
- `#ai-chip` reads `AI Claude · opus-5-5`, or `AI · not set up`; click opens `showSettings()`. It refreshes after a save.
- `showSettings()`: the Post-summary dialog shell with the diff-source segmented control for Claude / OpenAI-compatible / Ollama, `.field` inputs per tab (Claude: API key, model, base URL; OpenAI-compatible: base URL, API key, model; Ollama: host, model with suggestions from the last test, context window), a Test connection button that posts the current unsaved values and shows status next to the control, the "How it shows up" preview block, the three context default checkboxes, and Cancel / Save. The key field shows the mask when a key is stored and sends it back untouched unless edited.
- If an ask fails with "not configured", the toast gets an "Open Settings" action.

### 3.7 CSS

New rules only: `.ask-line` (hidden until `tr:hover`, `tr:focus-within`, or `.kbd-focus`), `.diff tr.cite td` (accent tint plus left bar), `#ask-menu` (surface, border, radius, shadow, `nav-item` rows, hint, disabled), `.ai-band`, `.ai-answer`, `.ai-you`, `.cite`, `.chip`, `#ai-chip`, and the settings form. Reuse `.kind`, `.pill`, `.badge-new`, `.link`, `.toggle`, `.field`, `.segmented`, `.help`, `.modal`. Dark mode comes from the existing tokens.

## 4. Desktop app

- `desktop/sidecar.spec`: remove `httpx2` from `excludes`; add `anthropic` to `hiddenimports` only if PyInstaller misses submodules (verify with a frozen run).
- The sidecar already uses `config_dir()`, so the app and the CLI share `settings.json`. No Rust changes needed; optionally add a `Settings…` item in `menu.rs` that evaluates `showSettings()` in the webview.

## 5. Tests

| File | Covers |
|---|---|
| `tests/test_settings.py` | defaults, round trip, file mode 0600, masked view, mask-means-keep on save, XDG isolation |
| `tests/test_ai_context.py` | enclosing function on both sides for `rename_repo` and `ts_rename_repo`, pattern markers on explained lines, old-side path for a renamed file, line cap, `history` on a fixture with two commits, references through the real navigator (reuse the markers from `test_navigation.py`) |
| `tests/test_ai_tasks.py` | availability rules per task on a worktree report, a PR report, a hunk with and without an enclosing def; rendered prompt is stable (snapshot string) |
| `tests/test_ai_providers.py` | Ollama NDJSON and OpenAI SSE parsing with `httpx2.MockTransport`; error mapping; Claude provider with a fake client exposing `messages.stream` |
| `tests/test_ai_server.py` | `GET/POST /api/settings` masking, `/ai/menu` shape, `/ai/ask` event sequence with a fake provider injected through `provider_factory`, 400 when not configured, 400 for an unavailable task, generator closed when the client disconnects |

JavaScript has no test harness in this repo; the UI is verified by running the app against the fixture repo with the browser pane (menu open, stream, stop, follow-up, cite hover, settings save, dark mode).

## 6. Phases

Each phase is shippable on its own.

1. **Settings and providers.** `settings.py`, `ai/providers.py`, the three routes, the settings dialog, the top-bar chip, Test connection, dependency and spec changes, `test_settings.py`, `test_ai_providers.py`. Nothing asks the model yet.
2. **Explain this change end to end.** `ai/context.py` (hunk, function, pr, patterns pieces), `ai/tasks.py` with the shared system prompt and the `explain` task, `/ai/menu` and `/ai/ask`, the hunk-bar Ask link, the menu, the answer band with streaming, Stop, Copy, Dismiss, the markdown renderer. Server tests for context and the stream.
3. **The rest of the tasks.** `references` and `history` pieces, PR body, the remaining seven tasks with availability and hints, the async refs count, custom prompt with context checkboxes.
4. **Line-level and polish.** Line Ask badge, cite chips with row highlighting and file links, follow-up turns, Post as comment, `a` key and help entry, "N answers" badge, README section, desktop smoke test.

## 7. Risks and notes

- **Context size.** A hunk plus two function bodies plus 50 references is usually 2k–6k tokens. The caps above keep the worst case under ~15k. Ollama users set `num_ctx` in Settings; the default 32k is the floor for coding models.
- **Navigation availability.** `uses` and `break` depend on Jedi or tsserver being usable; the menu shows the reason when they are not, reusing `navigator.describe_environment`.
- **Stale reports.** Reports live in memory; after a server restart the UI already re-analyzes. Answers are in the browser, so they survive a backend restart but not a reload.
- **Secrets.** Keys live only in the 0600 settings file and in the provider object; the public view masks them; the ask stream's `meta` event names the provider and model, never the key. Nothing is logged.
- **Model output is untrusted.** Rendered through the escape-first markdown path; cite and path links are built from regex matches on the escaped text, never from raw HTML.
- **Prompt caching on Claude.** The system prompt is constant and the context block is ordered deterministically, so follow-ups on the same hunk hit the cache; pass `cache_control` on the system block once that is measured to matter.
