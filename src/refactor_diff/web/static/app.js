"use strict";

const $ = (sel, root = document) => root.querySelector(sel);
const PAGE = 300; // occurrences rendered before "show more"

const state = {
  mode: "refs",
  sources: null,
  report: null,
  groupsByKey: new Map(),
  reviewed: new Set(), // reviewed group ids
  reviewedHunks: new Set(), // reviewed hunk fingerprints
  delta: { prevHead: null, newHunks: new Set(), changed: [] }, // since the previous analysis
  shown: PAGE,
  filters: emptyFilters(),
  view: null, // the report as filtered by state.filters; see applyFilters()
  fileDiffs: new Map(), // path -> Promise of the whole-file diff (see /api/report/{id}/file)
  split: false, // side-by-side diffs (preference; see splitActive())
  syntax: false, // syntax-color changed lines too (unchanged lines always are)
  repo: "", // absolute repository path (for editor links)
  editor: null, // URL template from --editor
  focus: -1, // keyboard focus: index into the page's hunks / occurrences
  commits: [], // commits between base and head (see /api/report/{id}/commits)
  parent: null, // {id, label} of the whole-range report while viewing one of its commits
  postingOk: false, // the user confirmed posting to the PR in this session
  explorerTab: "files", // "files" | "patterns" | "warnings"
  panes: { explorer: true, inspector: true }, // which side panes are shown (preference)
  analyzeMs: 0, // how long the last analysis took, for the status bar
  pendingScroll: null, // a selector to scroll to once the next route has rendered
};

// Side-by-side needs room for two code columns; narrower windows always get unified diffs.
const narrowQuery = window.matchMedia("(max-width: 760px)");

const CTX_STEP = 10; // lines revealed per "show more" click
const CTX_PAD = 5; // lines of context shown when context is first opened

const CATEGORIES = [
  ["source", "Source"],
  ["tests", "Tests"],
  ["docs", "Docs"],
  ["config", "Config"],
  ["other", "Other"],
];

// ---------- helpers ----------

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const MOD_KEY = IS_MAC ? "⌘" : "Ctrl";
const modDown = (e) => (IS_MAC ? e.metaKey : e.ctrlKey);

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
  ));
}

function highlight(text, ranges) {
  if (!ranges || !ranges.length) return esc(text);
  let out = "", pos = 0;
  for (const [s, e] of ranges) {
    const start = Math.max(s, pos), end = Math.min(e, text.length);
    if (start >= end) continue;
    out += esc(text.slice(pos, start)) + "<mark>" + esc(text.slice(start, end)) + "</mark>";
    pos = end;
  }
  return out + esc(text.slice(pos));
}

function plural(n, word, pluralWord) {
  return `${n.toLocaleString()} ${n === 1 ? word : (pluralWord || word + "s")}`;
}

async function api(path, body) {
  const res = await fetch(path, body === undefined ? {} : {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({ error: `HTTP ${res.status}` }));
  if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
  return data;
}

// ---------- review state ----------
// Reviewed marks live on the server (in ~/.config/refactor-diff), keyed by what is being
// compared rather than by commit, so they survive restarts and new commits. Hunks are tracked
// by content fingerprint; see /api/report/{id}/review.

function setReview(review) {
  state.reviewed = new Set(review.groups);
  state.reviewedHunks = new Set(review.hunks);
  state.delta = {
    prevHead: review.delta.prev_head,
    newHunks: new Set(review.delta.new),
    changed: review.delta.changed_reviewed,
  };
}

async function postReview(changes) {
  try {
    setReview(await api(`/api/report/${state.report.id}/review`, changes));
  } catch (e) {
    toast(`Couldn't save the review mark: ${e.message}`, { error: true });
  }
}

function toggleReviewed(id, on) {
  if (on) state.reviewed.add(id); else state.reviewed.delete(id);
  renderExplorer();
  renderInspector();
  postReview({ groups: { [on ? "add" : "remove"]: [id] } });
}

function markHunks(fingerprints, on) {
  for (const fp of fingerprints) {
    if (on) state.reviewedHunks.add(fp); else state.reviewedHunks.delete(fp);
  }
  applyFilters();
  renderStatus();
  renderExplorer();
  renderInspector();
  postReview({ hunks: { [on ? "add" : "remove"]: fingerprints } });
}

function hunkIsNew(h) {
  return state.delta.newHunks.has(h.fingerprint);
}

// ---------- filters ----------

// "migrations" or "*_pb2.py" match any path segment; patterns with a "/" match the whole path.
function globToRegex(glob) {
  let g = glob.trim();
  if (g.endsWith("/")) g += "**";
  const body = g.split(/(\*\*\/?|\*|\?)/).map((part) => {
    if (part === "**/") return "(?:.*/)?";
    if (part === "**") return ".*";
    if (part === "*") return "[^/]*";
    if (part === "?") return "[^/]";
    return part.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  }).join("");
  return g.includes("/") ? new RegExp(`^${body}$`) : new RegExp(`(?:^|/)${body}(?:/|$)`);
}

function filtersKey() {
  return `refactor-diff:filters:${$("#repo").title}`;
}
function loadFilters(cliDefaults) {
  let saved = {};
  try { saved = JSON.parse(localStorage.getItem(filtersKey()) || "{}"); } catch {}
  const f = { ...saved, ...(cliDefaults || {}) };
  state.filters = {
    hidden: new Set(f.hidden || []),
    hideDocs: Boolean(f.hideDocs),
    hideImports: Boolean(f.hideImports),
    hideFileMoves: Boolean(f.hideFileMoves),
    hideMoves: Boolean(f.hideMoves),
    exclude: f.exclude || [],
    nearOnly: false,
    newOnly: false,
    search: f.search || "",
    regex: Boolean(f.regex),
  };
}
function saveFilters() {
  const f = state.filters;
  try {
    localStorage.setItem(filtersKey(), JSON.stringify({
      hidden: [...f.hidden], hideDocs: f.hideDocs, hideImports: f.hideImports, hideFileMoves: f.hideFileMoves,
      hideMoves: f.hideMoves, exclude: f.exclude, search: f.search, regex: f.regex,
    }));
  } catch {}
}
function filtersActive() {
  const f = state.filters;
  return f.hidden.size > 0 || f.hideDocs || f.hideImports || f.hideFileMoves || f.hideMoves
    || f.exclude.length > 0 || f.nearOnly || f.newOnly || Boolean(f.search);
}
function emptyFilters() {
  return {
    hidden: new Set(), hideDocs: false, hideImports: false, hideFileMoves: false, hideMoves: false,
    exclude: [], nearOnly: false, newOnly: false, search: "", regex: false,
  };
}

// Kinds of change the filter bar can hide, by the unit tag the analysis gives them.
const CHANGE_FILTERS = [
  { key: "hideImports", tag: "imports", id: "imports-toggle", label: "Import edits",
    title: "Changes that only touch import statements" },
  { key: "hideFileMoves", tag: "file-move", id: "file-moves-toggle", label: "File renames",
    title: "Files renamed or moved without changes, and import updates that only follow a renamed file" },
  { key: "hideMoves", tag: "moved", id: "moves-toggle", label: "Moved functions",
    title: "Functions and classes moved verbatim. Only certain moves: an exact copy under the same name, "
      + "with no other candidate. Moves with any edit stay visible" },
];
function hiddenByTag(u) {
  return CHANGE_FILTERS.some((c) => state.filters[c.key] && u.tags.includes(c.tag));
}

// The search box as a RegExp (case-insensitive), null when empty, false when invalid.
function searchRegex() {
  const f = state.filters;
  if (!f.search) return null;
  try {
    return new RegExp(f.regex ? f.search : f.search.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "gi");
  } catch { return false; }
}

function applyFilters() {
  const r = state.report;
  const f = state.filters;
  const excludes = f.exclude.map(globToRegex);
  const fileOf = new Map(r.files.map((x) => [x.path, x]));
  const fileVisible = (path) => {
    const file = fileOf.get(path);
    if (file && f.hidden.has(file.category)) return false;
    if (file && file.pure_rename && f.hideFileMoves) return false;
    return !excludes.some((re) => re.test(path));
  };
  const docsOnly = (u) => u.signatures.length > 0 && u.signatures.every((k) => k === "docs");
  const re = searchRegex() || null;
  const test = (text) => { re.lastIndex = 0; return re.test(text); };
  const matches = (u) => !re || test(u.path)
    || u.old.some((ln) => test(ln.text)) || u.new.some((ln) => test(ln.text))
    || u.signatures.some((k) => { const g = state.groupsByKey.get(k); return g && test(g.label); });
  const unitVisible = (u) => fileVisible(u.path) && !(f.hideDocs && docsOnly(u)) && !hiddenByTag(u)
    && !(f.nearOnly && !u.explained && !(u.near && u.near.length))
    && !(f.newOnly && !hunkIsNew(r.hunks[u.hunk_id]))
    && matches(u);

  const units = new Set(Object.values(r.units).filter(unitVisible).map((u) => u.id));
  const groups = r.groups
    .map((g) => ({ ...g, visible: g.unit_ids.filter((id) => units.has(id)) }))
    .filter((g) => g.visible.length > 0);
  const residualHunks = r.residual_hunk_ids.filter((hid) =>
    r.hunks[hid].unit_ids.some((id) => units.has(id) && !r.units[id].explained));
  const warnings = [];
  for (const w of r.warnings) {
    if (!w.locations.length) { warnings.push(w); continue; }
    const locations = w.locations.filter((loc) => fileVisible(loc.path));
    if (locations.length) warnings.push({ ...w, locations, filteredOut: w.locations.length - locations.length });
  }
  const files = r.files.filter((x) => fileVisible(x.path));
  const explained = [...units].filter((id) => r.units[id].explained).length;
  const hunksDone = residualHunks.filter((hid) => state.reviewedHunks.has(r.hunks[hid].fingerprint)).length;

  state.view = {
    units,
    groups,
    groupsById: new Map(groups.map((g) => [g.id, g])),
    residualHunks,
    warnings,
    files,
    fileVisible,
    hiddenUnits: Object.keys(r.units).length - units.size,
    docsUnits: Object.values(r.units).filter((u) => fileVisible(u.path) && docsOnly(u)).length,
    nearUnits: Object.values(r.units).filter((u) => fileVisible(u.path) && !u.explained && u.near && u.near.length).length,
    stats: {
      files_changed: files.length,
      files_analyzed: files.filter((x) => x.analyzed).length,
      units: units.size,
      residual_units: units.size - explained,
      collapsed_pct: units.size ? Math.round((100 * explained) / units.size) : 0,
      mechanical_groups: groups.filter((g) => g.mechanical).length,
      verified_units: [...units].filter((id) => r.units[id].verified).length,
      moves: groups.filter((g) => g.kind === "move").length,
      hunks: residualHunks.length,
      hunks_done: hunksDone,
    },
  };
}

function rerender() {
  applyFilters();
  renderStatus();
  renderFilters();
  route({ keepScroll: true });
}

function renderFilters() {
  const r = state.report;
  const f = state.filters;
  const v = state.view;
  const counts = {};
  for (const file of r.files) counts[file.category] = (counts[file.category] || 0) + 1;
  const chips = CATEGORIES.filter(([cat]) => counts[cat]).map(([cat, label]) => `
    <button type="button" class="filter-chip" data-cat="${cat}" aria-pressed="${!f.hidden.has(cat)}">
      ${label.toLowerCase()} <span class="n">${counts[cat]}</span></button>`).join("");
  const docsTotal = Object.values(r.units)
    .filter((u) => u.signatures.length && u.signatures.every((k) => k === "docs")).length;
  const changeChips = CHANGE_FILTERS.map((c) => {
    let n = Object.values(r.units).filter((u) => u.tags.includes(c.tag)).length;
    if (c.tag === "file-move") n += r.files.filter((x) => x.pure_rename).length;
    return `<button type="button" class="filter-chip" id="${c.id}" aria-pressed="${!f[c.key]}"
      title="${esc(c.title)}" ${n ? "" : "disabled"}>${c.label.toLowerCase()} <span class="n">${n}</span></button>`;
  }).join("");
  const fresh = state.delta.newHunks.size;

  // File and change filters live under the explorer; search and the diff toggles sit in the
  // stream's toolbar, next to what they affect.
  const foot = $("#explorer-foot");
  foot.hidden = false;
  foot.innerHTML = `
    <div class="row"><span class="filter-label">show</span>${chips}</div>
    <div class="row"><span class="filter-label">changes</span>
      <button type="button" class="filter-chip" id="docs-toggle" aria-pressed="${!f.hideDocs}"
        title="Changes that only touch comments or docstrings" ${docsTotal ? "" : "disabled"}>
        docs edits <span class="n">${docsTotal}</span></button>${changeChips}</div>
    ${v.nearUnits || fresh ? `<div class="row"><span class="filter-label">only</span>
      ${v.nearUnits ? `<button type="button" class="filter-chip mode near" id="near-toggle" aria-pressed="${f.nearOnly}"
        title="Only leftover changes that almost match a mechanical pattern (likely typos)">≈ near misses <span class="n">${v.nearUnits}</span></button>` : ""}
      ${fresh ? `<button type="button" class="filter-chip mode" id="new-toggle" aria-pressed="${f.newOnly}"
        title="Only changes that weren't in the diff last time you analyzed it">new since ${esc(state.delta.prevHead.slice(0, 7))} <span class="n">${fresh}</span></button>` : ""}
    </div>` : ""}
    <label class="exclude"><span class="filter-label">exclude</span>
      <input id="exclude-input" type="text" spellcheck="false" value="${esc(f.exclude.join(", "))}"
        placeholder="globs: migrations, *_pb2.py, src/legacy/**"></label>`;

  const bar = $("#toolbar");
  bar.hidden = false;
  bar.innerHTML = `
    <h1 id="view-title"></h1>
    <span class="spacer"></span>
    <div class="search" title="Narrow everything to changes whose lines, path or pattern match">
      <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><circle cx="7" cy="7" r="5"/><path d="M11 11l4 4"/></svg>
      <input id="search-input" type="text" spellcheck="false" value="${esc(f.search)}" class="${searchRegex() === false ? "invalid" : ""}"
        placeholder="search changed lines, paths, patterns" aria-label="Search">
      <button type="button" class="filter-chip mode regex" id="regex-toggle" aria-pressed="${f.regex}" title="Regular expression">.*</button>
      <kbd>/</kbd>
    </div>
    ${highlightToggle()}
    ${layoutToggle()}
    ${filtersActive() ? `<span class="hidden-note">${plural(v.hiddenUnits, "change")} hidden</span>
      <button type="button" class="link" id="reset-filters">Reset</button>` : ""}`;

  for (const b of foot.querySelectorAll("[data-cat]")) {
    b.addEventListener("click", () => {
      const cat = b.dataset.cat;
      if (f.hidden.has(cat)) f.hidden.delete(cat); else f.hidden.add(cat);
      saveFilters();
      rerender();
    });
  }
  bindLayoutToggle(bar);
  $("#docs-toggle").addEventListener("click", () => {
    f.hideDocs = !f.hideDocs;
    saveFilters();
    rerender();
  });
  for (const c of CHANGE_FILTERS) {
    $(`#${c.id}`).addEventListener("click", () => {
      f[c.key] = !f[c.key];
      saveFilters();
      rerender();
    });
  }
  $("#near-toggle")?.addEventListener("click", () => {
    f.nearOnly = !f.nearOnly;
    rerender();
  });
  $("#new-toggle")?.addEventListener("click", () => {
    f.newOnly = !f.newOnly;
    rerender();
  });
  const input = $("#exclude-input");
  input.addEventListener("change", () => {
    f.exclude = input.value.split(",").map((s) => s.trim()).filter(Boolean);
    saveFilters();
    rerender();
  });
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") input.blur(); });
  const search = $("#search-input");
  search.addEventListener("change", () => {
    f.search = search.value.trim();
    saveFilters();
    rerender();
  });
  search.addEventListener("keydown", (e) => {
    if (e.key === "Enter") search.blur();
    if (e.key === "Escape") { search.value = ""; search.blur(); if (f.search) { f.search = ""; saveFilters(); rerender(); } }
  });
  $("#regex-toggle").addEventListener("click", () => {
    f.regex = !f.regex;
    saveFilters();
    rerender();
  });
  $("#reset-filters")?.addEventListener("click", () => {
    state.filters = emptyFilters();
    saveFilters();
    rerender();
  });
}

// ---------- source form ----------

function setMode(mode) {
  state.mode = mode;
  for (const b of document.querySelectorAll(".segmented button")) {
    b.setAttribute("aria-selected", String(b.dataset.mode === mode));
  }
  for (const f of document.querySelectorAll("[data-show]")) {
    f.hidden = !f.dataset.show.split(" ").includes(mode);
  }
  $("[name=base]").required = mode !== "pr";
  $("[name=pr]").required = mode === "pr";
}

async function loadSources(defaults) {
  try {
    const src = await api("/api/sources");
    state.sources = src;
    $("#branch-list").innerHTML = src.branches.map((b) => `<option value="${esc(b)}">`).join("");
    $("#pr-list").innerHTML = src.prs.map((p) =>
      `<option value="${p.number}">#${p.number} ${esc(p.title)} (${esc(p.headRefName)})</option>`
    ).join("");
    const form = $("#source-form");
    if (!form.base.value) form.base.value = defaults.base || src.default_base;
    if (!form.head.value) {
      form.head.value = defaults.head || (src.current !== src.default_base ? src.current : "HEAD");
    }
    if (src.pr_error) $("[name=pr]").placeholder = "Number (gh unavailable)";
  } catch (e) {
    showError(e.message);
  }
}

async function runAnalysis(evt) {
  evt?.preventDefault();
  const form = $("#source-form");
  const body = { min_count: Number(form.min_count.value) || 2 };
  if (state.mode === "pr") body.pr = form.pr.value.replace(/^#/, "").trim();
  else if (state.mode === "worktree") Object.assign(body, { base: form.base.value, head: ":worktree:" });
  else Object.assign(body, { base: form.base.value, head: form.head.value || "HEAD" });

  const btn = $("#analyze-btn");
  btn.disabled = true;
  btn.innerHTML = '<span class="spinner"></span> Analyzing';
  const started = performance.now();
  try {
    const report = await api("/api/analyze", body);
    state.analyzeMs = performance.now() - started;
    state.parent = null;
    state.commits = [];
    loadReport(report);
    loadCommits();
  } catch (e) {
    showError(e.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Analyze";
  }
}

function loadReport(report) {
  state.report = report;
  state.groupsByKey = new Map(report.groups.map((g) => [g.key, g]));
  state.shown = PAGE;
  state.fileDiffs = new Map();
  setReview(report.review);
  applyFilters();
  renderStatus();
  renderFilters();
  if (!location.hash || location.hash === "#" || location.hash.startsWith("#file/") || location.hash.startsWith("#lib/")) {
    if (location.hash === "#review") route(); else location.hash = "#review";
  } else {
    route();
  }
}

// A report the server already has (e.g. the whole range, after looking at one commit).
async function showReport(id) {
  try {
    const report = await api(`/api/report/${id}`);
    state.parent = null;
    loadReport(report);
  } catch (e) {
    showError(e.message);
  }
}

// ---------- commits ----------

async function loadCommits() {
  const id = state.report.id;
  try {
    const { commits } = await api(`/api/report/${id}/commits`);
    if (state.report.id !== id) return;
    state.commits = commits;
    renderExplorer();
    if (location.hash === "#commits") route({ keepScroll: true });
  } catch { state.commits = []; }
}

// Analyze one commit of the range against its parent; the whole-range report stays a click away.
async function analyzeCommit(sha) {
  const parent = state.parent || { id: state.report.id, label: state.report.source.label, commits: state.commits };
  toast(`Analyzing ${sha.slice(0, 7)}…`, { busy: true, sticky: true });
  try {
    const report = await api("/api/analyze", { base: `${sha}^`, head: sha, min_count: state.report.source.min_count });
    hideToast();
    state.parent = parent;
    state.commits = parent.commits;
    loadReport(report);
  } catch (e) {
    toast(e.message, { error: true });
  }
}

function currentCommitIndex() {
  if (!state.parent) return -1;
  return state.commits.findIndex((c) => c.sha === state.report.source.head_sha);
}

function commitStep(delta) {
  const i = currentCommitIndex();
  const next = i < 0 ? (delta > 0 ? 0 : state.commits.length - 1) : i + delta;
  if (next >= 0 && next < state.commits.length) analyzeCommit(state.commits[next].sha);
}

function renderCommits() {
  const content = $("#content");
  const commits = state.commits;
  if (!commits.length) {
    content.innerHTML = `<div class="empty"><h2>No commits</h2><p>${state.report.source.head_sha
      ? "The range holds no commits." : "The working tree isn't a commit range."}</p></div>`;
    return;
  }
  const current = currentCommitIndex();
  const label = state.parent ? state.parent.label : state.report.source.label;
  let html = `<div class="page-head">
    <p>${plural(commits.length, "commit")} in ${esc(label)}, oldest first. Analyze one to review it on its own
    (its reviewed marks are separate from the whole range's).</p></div>
    <table class="files commits"><thead><tr><th></th><th>Commit</th><th>Subject</th><th>Author</th><th>Files</th><th>+/−</th><th></th></tr></thead><tbody>`;
  commits.forEach((c, i) => {
    const date = c.date ? new Date(c.date).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : "";
    html += `<tr class="${i === current ? "current" : ""}">
      <td class="num">${i + 1}</td>
      <td class="code">${esc(c.short)}</td>
      <td class="path">${esc(c.subject)}</td>
      <td>${esc(c.author)}<span class="meta"> ${esc(date)}</span></td>
      <td class="num">${c.files}</td>
      <td class="num"><span class="adds">+${c.insertions}</span> <span class="dels">−${c.deletions}</span></td>
      <td>${i === current ? '<span class="meta">viewing</span>'
        : `<button type="button" class="link" data-analyze-commit="${esc(c.sha)}">Analyze</button>`}</td></tr>`;
  });
  content.innerHTML = html + "</tbody></table>";
}

function showError(msg) {
  $("#content").innerHTML = `<div class="error">${esc(msg)}</div>`;
}

// ---------- status bar ----------

// The status bar: the report's numbers in one line at the bottom of the window.
function renderStatus() {
  const v = state.view;
  const { stats } = v;
  const el = $("#status");
  el.hidden = false;
  const ms = state.analyzeMs;
  const took = ms ? `analyzed in ${ms >= 1000 ? (ms / 1000).toFixed(1) + "s" : Math.round(ms) + "ms"}` : "report loaded";
  const left = stats.hunks - stats.hunks_done;
  el.innerHTML = `
    <span><span class="dot"></span>${took}</span>
    <span>${stats.collapsed_pct}% collapsed</span>
    ${stats.verified_units ? `<span class="ok" title="${esc(VERIFIED_TITLE)}">${stats.verified_units} ✓ verified</span>` : ""}
    <span class="${stats.hunks && !left ? "ok" : ""}">${stats.hunks_done}/${stats.hunks} hunks</span>
    <span>${plural(stats.residual_units, "change")} to review</span>
    <span class="${v.warnings.length ? "warn" : ""}">${plural(v.warnings.length, "warning")}</span>
    <span class="spacer"></span>
    <span>${plural(stats.mechanical_groups, "pattern")} · ${stats.files_analyzed}/${stats.files_changed} files</span>
    <span>${splitActive() ? "split" : "unified"} · ${state.syntax ? "syntax" : "diff"} · min repeats ${state.report.source.min_count ?? 2}</span>
    <button type="button" class="link" id="status-help"><kbd>?</kbd> help</button>`;
  $("#status-help").addEventListener("click", () => showHelp(true));
}

// ---------- outputs: Markdown summary and PR comments ----------

async function fetchSummary() {
  const res = await fetch(`/api/report/${state.report.id}/summary.md`);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.text();
}

async function copySummary() {
  try {
    const md = await fetchSummary();
    await navigator.clipboard.writeText(md);
    toast("Review summary copied as Markdown.");
  } catch (e) {
    toast(`Couldn't copy: ${e.message}`, { error: true });
  }
}

function prNumber() {
  return state.report.source.pr ? state.report.source.pr.number : null;
}

// Posting is outward-facing: ask once per session, after showing what will be posted.
function confirmPosting() {
  if (state.postingOk) return true;
  state.postingOk = window.confirm(`Post comments to pull request #${prNumber()} as you (via gh)?`);
  return state.postingOk;
}

async function postSummary() {
  let md;
  try { md = await fetchSummary(); } catch (e) { toast(e.message, { error: true }); return; }
  showModal(`Post summary to PR #${prNumber()}`, md, async (body) => {
    if (!confirmPosting()) return false;
    const res = await api(`/api/report/${state.report.id}/pr/comment`, { body });
    toast(`Posted: ${res.url}`);
    return true;
  });
}

// A dialog with an editable textarea and Post / Cancel.
function showModal(title, text, onPost) {
  const dlg = $("#modal");
  dlg.innerHTML = `<header><h3>${esc(title)}</h3><button type="button" class="close" aria-label="Close" id="modal-close">×</button></header>
    <textarea id="modal-text" spellcheck="false"></textarea>
    <footer><span class="meta" id="modal-note"></span><span class="spacer"></span>
      <button type="button" class="toggle" id="modal-cancel">Cancel</button>
      <button type="button" class="primary" id="modal-post">Post</button></footer>`;
  $("#modal-text").value = text;
  const close = () => dlg.close();
  $("#modal-close").addEventListener("click", close);
  $("#modal-cancel").addEventListener("click", close);
  $("#modal-post").addEventListener("click", async () => {
    const btn = $("#modal-post");
    btn.disabled = true;
    try {
      if (await onPost($("#modal-text").value)) close();
    } catch (e) {
      $("#modal-note").textContent = e.message;
    } finally {
      btn.disabled = false;
    }
  });
  dlg.showModal();
}

// Inline comment box under a hunk (c key or the Comment button).
function openCommentBox(el) {
  $("#comment-box")?.remove();
  if (!el || !el.dataset.hunk) return;
  if (!prNumber()) { toast("Comments can only be posted when reviewing a pull request.", { error: true }); return; }
  const h = state.report.hunks[el.dataset.hunk];
  const at = hunkAnchor(h);
  const where = at.new_no ? `${h.path}:${at.new_no} (new side)` : `${h.path}:${at.old_no} (original side)`;
  const box = document.createElement("div");
  box.id = "comment-box";
  box.className = "comment-box";
  box.innerHTML = `<textarea placeholder="Comment on this hunk…" spellcheck="true"></textarea>
    <div class="comment-actions"><span class="meta">Review comment on ${esc(where)} of PR #${prNumber()}</span>
      <span class="spacer"></span>
      <button type="button" class="toggle" data-comment-cancel>Cancel</button>
      <button type="button" class="primary" data-comment-post>Post</button></div>`;
  el.querySelector(".hunk-bar").after(box);
  const ta = box.querySelector("textarea");
  ta.focus();
  box.querySelector("[data-comment-cancel]").addEventListener("click", () => box.remove());
  ta.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") box.querySelector("[data-comment-post]").click();
  });
  box.querySelector("[data-comment-post]").addEventListener("click", async () => {
    const body = ta.value.trim();
    if (!body) { ta.focus(); return; }
    if (!confirmPosting()) return;
    const btn = box.querySelector("[data-comment-post]");
    btn.disabled = true;
    try {
      const res = await api(`/api/report/${state.report.id}/pr/review-comment`, { body, hunk_id: h.id });
      toast(`Posted: ${res.url}`);
      box.remove();
    } catch (e) {
      toast(e.message, { error: true });
      btn.disabled = false;
    }
  });
}

// The line a comment on (or a jump to) this hunk should target: the first changed line of a
// unit that still needs review, preferring the new side (mirrors export.anchor_line).
function hunkAnchor(h) {
  const r = state.report;
  const changed = h.lines.filter((ln) => ln.type !== " ");
  const residual = changed.filter((ln) => ln.unit && !r.units[ln.unit].explained);
  const pool = residual.length ? residual : changed.length ? changed : h.lines;
  return pool.find((ln) => ln.type === "+") || pool[0];
}

// ---------- explorer (left pane): files, patterns and warnings ----------

function renderExplorer() {
  const r = state.report;
  if (!r) return;
  const v = state.view;
  const tab = state.explorerTab;
  const mech = v.groups.filter((g) => g.mechanical);
  const tabs = $("#explorer-tabs");
  for (const b of tabs.querySelectorAll("button")) b.setAttribute("aria-selected", String(b.dataset.tab === tab));
  tabs.querySelector("[data-tab=files]").innerHTML = `Files <span class="n">${v.files.length}</span>`;
  tabs.querySelector("[data-tab=patterns]").innerHTML = `Patterns <span class="n">${mech.length}</span>`;
  tabs.querySelector("[data-tab=warnings]").innerHTML = `Warnings <span class="n">${v.warnings.length}</span>`;

  const here = location.hash;
  const active = (h) => (here === h ? " sel" : "");
  const list = $("#explorer-list");
  list.innerHTML = tab === "patterns" ? explorerPatterns(mech, active)
    : tab === "warnings" ? explorerWarnings(active)
    : explorerFiles(active);
  for (const cb of list.querySelectorAll("[data-review]")) {
    cb.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleReviewed(cb.dataset.review, cb.checked);
      if (location.hash === "#group/" + cb.dataset.review) route({ keepScroll: true });
    });
  }
  markExplorerFile();
}

function reviewNavRow(active) {
  const v = state.view;
  const left = v.stats.hunks - v.stats.hunks_done;
  return `<a class="node nav${active("#review")}" href="#review"><span class="label sans"><strong>Needs review</strong></span>
    <span class="pill ${left ? "attention" : "ok"}" title="${v.stats.hunks_done} of ${v.stats.hunks} hunks reviewed">${left}</span></a>`;
}

function warningsNavRow(active) {
  const n = state.view.warnings.length;
  return `<a class="node nav${active("#warnings")}" href="#warnings"><span class="label sans">Warnings</span>
    <span class="pill ${n ? "warn" : ""}">${n}</span></a>`;
}

// Every changed file, grouped by directory, with what is left to read in it.
function explorerFiles(active) {
  const r = state.report;
  const v = state.view;
  const skipped = v.files.filter((f) => !f.analyzed).length;
  let html = reviewNavRow(active) + `
    <a class="node nav${active("#files")}" href="#files"><span class="label sans">All files</span>
      <span class="count">${v.files.length}${skipped ? ` · ${skipped} not analyzed` : ""}</span></a>
    ${state.commits.length > 1 || state.parent ? `<a class="node nav${active("#commits")}" href="#commits">
      <span class="label sans">Commits</span>
      <span class="count">${state.parent ? `${currentCommitIndex() + 1} of ${state.commits.length}` : state.commits.length}</span></a>` : ""}`;
  const residual = new Map(); // path -> {total, left}
  for (const hid of v.residualHunks) {
    const h = r.hunks[hid];
    const e = residual.get(h.path) || { total: 0, left: 0 };
    e.total++;
    if (!state.reviewedHunks.has(h.fingerprint)) e.left++;
    residual.set(h.path, e);
  }
  let dir = null;
  for (const f of r.files) {
    const i = f.path.lastIndexOf("/");
    const d = i >= 0 ? f.path.slice(0, i + 1) : "";
    if (d !== dir) { dir = d; html += `<div class="dir">${esc(d || "./")}</div>`; }
    const res = residual.get(f.path);
    const visible = v.fileVisible(f.path);
    let badge = "", cls = "";
    if (!visible) cls = " filtered";
    else if (res && res.left) badge = `<span class="badge bad" title="${plural(res.left, "hunk")} to review">${res.left}</span>`;
    else if (res) badge = `<span class="badge ok" title="All hunks reviewed">✓</span>`;
    else if (f.analyzed) { badge = `<span class="badge" title="Every change matched a pattern">·</span>`; cls = " dim"; }
    else { badge = `<span class="badge" title="Not analyzed">—</span>`; cls = " dim"; }
    const inReview = res && visible;
    html += `<a class="node${cls}" href="${inReview ? "#review" : fileHref(f.path, "diff")}"${inReview ? ` data-scroll-file="${esc(f.path)}"` : ""}
      data-file-node="${esc(f.path)}" title="${esc(f.path)}">
      <span class="st ${esc(f.status.toLowerCase())}">${esc(f.status)}</span><span class="label">${esc(f.path.slice(i + 1))}</span>${badge}</a>`;
  }
  return html;
}

function explorerPatterns(mech, active) {
  const r = state.report;
  const done = mech.filter((g) => state.reviewed.has(g.id)).length;
  let html = `<div class="hd"><span>mechanical</span><span>${done}/${mech.length} reviewed</span></div>`;
  if (!mech.length) {
    html += `<div class="node dim"><span class="label sans">${
      filtersActive() ? "No patterns in the visible files" : "No repeated patterns found"}</span></div>`;
  }
  for (const g of mech) {
    const isDone = state.reviewed.has(g.id);
    const fresh = g.visible.filter((uid) => hunkIsNew(r.hunks[r.units[uid].hunk_id])).length;
    html += `
      <a class="node nav${active("#group/" + g.id)}${isDone && !fresh ? " done" : ""}" href="#group/${g.id}" title="${esc(g.label)}${isDone && fresh ? ` — reviewed, but ${fresh} new since` : ""}">
        <input type="checkbox" data-review="${g.id}" ${isDone ? "checked" : ""} aria-label="Mark reviewed">
        <span class="kind ${g.kind}">${g.kind}</span>
        <span class="label">${esc(g.label)}</span>
        ${fresh ? `<span class="badge-new">+${fresh}</span>` : ""}
        <span class="count">×${g.visible.length}</span>
      </a>`;
  }
  html += `<div class="hd"><span>leftover</span></div>` + reviewNavRow(active) + warningsNavRow(active);
  return html;
}

function explorerWarnings(active) {
  const v = state.view;
  if (!v.warnings.length) {
    return `<div class="node dim"><span class="label sans">${filtersActive() ? "No warnings in the visible files" : "No warnings"}</span></div>`;
  }
  let html = `<div class="hd"><span>warnings</span><span>${v.warnings.length}</span></div>`;
  v.warnings.forEach((w, i) => {
    const n = w.locations.length || w.total;
    html += `<a class="node nav" href="#warnings" data-warning-index="${i}" title="${esc(w.message)}">
      <span class="badge warn">${n || "!"}</span><span class="label sans">${esc(w.message)}</span></a>`;
  });
  return html + warningsNavRow(active);
}

// Highlight the file being viewed, or the one the focused hunk lives in.
function markExplorerFile() {
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  const path = view === "file" ? decodeURIComponent(id || "") : focusedLocation()?.path;
  for (const a of document.querySelectorAll("#explorer-list [data-file-node]")) {
    a.classList.toggle("sel", Boolean(path) && a.dataset.fileNode === path);
  }
}

function onExplorerClick(e) {
  const file = e.target.closest("[data-scroll-file]");
  if (file) {
    e.preventDefault();
    goTo("#review", `[data-file-section="${CSS.escape(file.dataset.scrollFile)}"]`);
    return;
  }
  const w = e.target.closest("[data-warning-index]");
  if (w) {
    e.preventDefault();
    goTo("#warnings", `#warning-${Number(w.dataset.warningIndex)}`);
  }
}

// Navigate to a view and scroll to one element in it (a file's section, a warning).
function goTo(hash, selector) {
  if (location.hash === hash) { scrollToSelector(selector); return; }
  state.pendingScroll = selector;
  location.hash = hash;
}

function scrollToSelector(selector) {
  const el = $(`#content ${selector}`);
  if (!el) return;
  const top = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--stick-h")) || 0;
  window.scrollTo({ top: el.getBoundingClientRect().top + window.scrollY - top - 8 });
  const hunk = el.classList.contains("hunk") ? el : el.querySelector(".hunk");
  const i = hunk ? focusables().indexOf(hunk) : -1;
  if (i >= 0) { state.focus = i; applyFocus(false); }
  else if (el.classList.contains("warning")) {
    for (const x of document.querySelectorAll(".warning.kbd-focus")) x.classList.remove("kbd-focus");
    el.classList.add("kbd-focus");
  }
}

// ---------- inspector (right pane): facts about what is focused, progress, the report ----------

function renderInspector() {
  const el = $("#inspector");
  if (!state.report) { el.innerHTML = ""; return; }
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  let html = "";
  if (view === "group" && id) html += inspectorPattern(id);
  else if (!view || view === "review") html += inspectorHunk();
  html += inspectorProgress() + inspectorWarnings() + inspectorReport() + inspectorKeys();
  el.innerHTML = `<div class="insp">${html}</div>`;
}

function basename(path) {
  return path.slice(path.lastIndexOf("/") + 1);
}

function inspectorHunk() {
  const r = state.report;
  const v = state.view;
  const items = focusables();
  const el = focusedElement();
  const h = el && el.dataset.hunk ? r.hunks[el.dataset.hunk] : null;
  if (!h) {
    return `<h2>Focused hunk</h2><div class="card"><div class="prose">${v.residualHunks.length
      ? `Press <kbd>j</kbd> to focus the first hunk, or click one. The facts about it show here.`
      : "Nothing left to review in the visible files."}</div></div>`;
  }
  const units = h.unit_ids.map((uid) => r.units[uid]).filter((u) => v.units.has(u.id));
  const explainedBy = new Map();
  const leftover = new Map();
  for (const u of units) {
    for (const key of u.signatures) {
      const g = state.groupsByKey.get(key);
      if (!g) continue;
      if (g.mechanical) explainedBy.set(g.id, g);
      else if (!u.explained) leftover.set(g.id, g);
    }
  }
  const near = [...new Set(units.flatMap((u) => u.near || []).map((n) => n.group_id))]
    .map((gid) => v.groupsById.get(gid)).filter(Boolean);
  const verified = units.filter((u) => u.verified).length;
  const done = state.reviewedHunks.has(h.fingerprint);
  const added = h.lines.filter((ln) => ln.new_no != null).length;
  const span = added ? `${h.new_start}–${h.new_start + added - 1}` : `${h.old_start}`;
  const row = (l, v, cls = "") => `<div class="row"><span class="l">${l}</span><span class="v wrap ${cls}">${v}</span></div>`;
  const label = (g) => `<a href="#group/${g.id}" class="code">${esc(shortLabel(g.label))}</a>`;
  return `<h2>Focused hunk</h2>
    <div class="card">
      <div class="loc"><b>${esc(basename(h.path))}</b>:${span} · hunk ${items.indexOf(el) + 1} of ${items.length}</div>
      ${explainedBy.size ? row("explained by", [...explainedBy.values()].map(label).join("<br>")) : ""}
      ${leftover.size ? row("left to read", [...leftover.values()].map((g) => `${esc(g.kind)} <span class="code">${esc(shortLabel(g.label))}</span>`).join("<br>"), "warn")
        : row("left to read", units.some((u) => !u.explained) ? "a change with no pattern" : "nothing", units.some((u) => !u.explained) ? "warn" : "ok")}
      ${near.length ? row("≈ almost", near.map(label).join("<br>"), "warn") : ""}
      ${verified ? row("verified", `${verified} of ${units.length} by AST`, "ok") : ""}
      ${hunkIsNew(h) ? row("since last time", "new") : ""}
      ${done ? row("status", "reviewed", "ok") : ""}
      <div class="actions">
        <button type="button" class="go" data-act="reviewed">${done ? "Unmark" : "Reviewed"} <kbd>x</kbd></button>
        <button type="button" data-act="context">Context <kbd>e</kbd></button>
        <button type="button" data-act="open">Open in editor <kbd>o</kbd></button>
        <button type="button" data-act="next">Next hunk <kbd>j</kbd></button>
        ${r.source.pr ? `<button type="button" class="span" data-act="comment">Comment on the PR <kbd>c</kbd></button>` : ""}
      </div>
    </div>`;
}

function inspectorPattern(id) {
  const r = state.report;
  const v = state.view;
  const g = v.groupsById.get(id);
  if (!g) return "";
  const units = g.visible.map((uid) => r.units[uid]);
  const files = new Set(units.map((u) => u.path)).size;
  const verified = units.filter((u) => u.verified).length;
  const fresh = units.filter((u) => hunkIsNew(r.hunks[u.hunk_id])).length;
  const isDone = state.reviewed.has(g.id);
  const mech = v.groups.filter((x) => x.mechanical);
  const i = mech.findIndex((x) => x.id === g.id);
  const details = Object.entries(g.details).sort((a, b) => b[1] - a[1]).slice(0, 8);
  const max = details.length ? details[0][1] : 1;
  let html = `<h2>Pattern <span>${i >= 0 ? `${i + 1} of ${mech.length}` : "leftover"}</span></h2>
    <div class="card">
      <div class="row"><span class="l">kind</span><span class="v" style="color: var(--kind-${esc(g.kind)})">${esc(g.kind)}</span></div>
      <div class="row"><span class="l">occurrences</span><span class="v">${g.visible.length} in ${plural(files, "file")}</span></div>
      ${verified ? `<div class="row"><span class="l">verified</span><span class="v ok" title="${esc(VERIFIED_TITLE)}">${verified} of ${units.length} by AST</span></div>` : ""}
      ${fresh ? `<div class="row"><span class="l">since last time</span><span class="v">${fresh} new</span></div>` : ""}
      <div class="actions">
        <button type="button" class="go span" data-act="group-reviewed">${isDone ? "Unmark reviewed" : "Mark pattern reviewed"} <kbd>x</kbd></button>
        <button type="button" data-act="pattern-prev">Previous <kbd>[</kbd></button>
        <button type="button" data-act="pattern-next">Next pattern <kbd>]</kbd></button>
      </div>
    </div>`;
  if (details.length && g.kind !== "move") {
    html += `<h2>Where it was applied</h2><div class="card"><div class="bars">${details.map(([d, n]) =>
      `<span class="d" title="${esc(d)}">${esc(d)}</span><span class="b"><i style="width:${Math.round((100 * n) / max)}%"></i></span><span class="n">${n}</span>`).join("")}</div></div>`;
  }
  const el = focusedElement();
  const u = el && el.dataset.unit ? r.units[el.dataset.unit] : null;
  if (u) {
    const items = focusables();
    html += `<h2>Occurrence <span>${items.indexOf(el) + 1} of ${items.length}</span></h2>
      <div class="card">
        <div class="loc">${esc(u.path)}:${u.new.length ? u.new_start : u.old_start}</div>
        <div class="links">${fileLinks(u.path, { oldLine: u.old.length ? u.old_start : null, newLine: u.new.length ? u.new_start : null })}</div>
      </div>`;
  }
  return html;
}

function inspectorProgress() {
  const v = state.view;
  const s = v.stats;
  const mech = v.groups.filter((g) => g.mechanical);
  const done = mech.filter((g) => state.reviewed.has(g.id)).length;
  const pct = (a, b) => (b ? Math.round((100 * a) / b) : 0);
  const d = state.delta;
  const since = d.prevHead && (d.newHunks.size || d.changed.length)
    ? `<div class="row"><span class="l">since ${esc(d.prevHead.slice(0, 7))}</span><span class="v wrap">${[
      d.newHunks.size ? plural(d.newHunks.size, "new change") : "",
      d.changed.length ? `${d.changed.length} reviewed modified` : "",
    ].filter(Boolean).join(" · ")}</span></div>` : "";
  return `<h2>Progress</h2>
    <div class="card">
      <div class="row"><span class="l">hunks reviewed</span><span class="v">${s.hunks_done} / ${s.hunks}</span></div>
      <div class="meter"><div style="width:${pct(s.hunks_done, s.hunks)}%"></div></div>
      <div class="row"><span class="l">patterns reviewed</span><span class="v">${done} / ${mech.length}</span></div>
      <div class="meter"><div style="width:${pct(done, mech.length)}%"></div></div>
      <div class="row"><span class="l">lines collapsed</span><span class="v">${s.collapsed_pct}%${s.verified_units ? ` · ${s.verified_units} verified` : ""}</span></div>
      <div class="meter"><div style="width:${s.collapsed_pct}%"></div></div>
      ${since}
    </div>`;
}

function inspectorWarnings() {
  const v = state.view;
  const r = state.report;
  const shown = v.warnings.slice(0, 4);
  const cards = shown.map((w, i) => {
    const g = w.group_id ? v.groupsById.get(w.group_id) : null;
    const loc = w.locations[0];
    return `<div class="card warn">
      <div class="prose">${esc(w.message)}</div>
      ${loc ? `<div class="loc" style="color: var(--muted)">${esc(loc.path)}:${loc.line}${w.locations.length > 1 ? ` +${w.locations.length - 1}` : ""}</div>` : ""}
      <div class="links"><a href="#warnings" data-warning-index="${i}">view</a>${g ? `<a href="#group/${g.id}">pattern</a>` : ""}</div>
    </div>`;
  }).join("");
  const more = v.warnings.length > shown.length ? `<a href="#warnings" class="prose">…and ${v.warnings.length - shown.length} more</a>` : "";
  return `<h2>Warnings <span class="${v.warnings.length ? "warn" : ""}" style="${v.warnings.length ? "color: var(--warn-fg)" : ""}">${v.warnings.length}</span></h2>
    ${cards || `<div class="prose" style="color: var(--muted)">${r.warnings.length ? "None in the visible files." : "None found."}</div>`}${more}`;
}

function inspectorReport() {
  const { source } = state.report;
  const sha = (s) => (s ? s.slice(0, 8) : "working tree");
  return `<h2>Report</h2>
    <div class="card">
      <div class="row"><span class="l">comparing</span><span class="v wrap">${esc(source.label)}</span></div>
      ${state.parent ? `<div class="row"><span class="l">part of</span><span class="v wrap">${esc(state.parent.label)}</span></div>`
        : `<div class="row"><span class="l">commits</span><span class="v">${sha(source.base_sha)} → ${sha(source.head_sha)}</span></div>`}
      <div class="links">
        ${state.parent ? `<button type="button" class="link" data-act="back-range">← Back to the whole range</button>` : ""}
        <button type="button" class="link" data-act="copy-md" title="Copy the review summary as Markdown">Copy as Markdown</button>
        ${source.pr ? `<button type="button" class="link" data-act="post-summary" title="Post the summary as a comment on the pull request">Post summary to PR #${source.pr.number}</button>` : ""}
      </div>
    </div>`;
}

function inspectorKeys() {
  return `<h2>Keys</h2>
    <div class="keys">
      <span><kbd>j</kbd> <kbd>k</kbd></span><span>next / previous hunk</span>
      <span><kbd>x</kbd> <kbd>e</kbd></span><span>mark reviewed / show context</span>
      <span><kbd>]</kbd> <kbd>[</kbd></span><span>step through patterns</span>
      <span><kbd>g</kbd> <kbd>r</kbd></span><span>needs review · <kbd>g</kbd> <kbd>w</kbd> warnings</span>
      <span><kbd>?</kbd></span><span><button type="button" class="link" data-act="help">all shortcuts</button></span>
    </div>`;
}

function onInspectorClick(e) {
  const w = e.target.closest("[data-warning-index]");
  if (w) {
    e.preventDefault();
    goTo("#warnings", `#warning-${Number(w.dataset.warningIndex)}`);
    return;
  }
  const btn = e.target.closest("[data-act]");
  if (!btn) return;
  switch (btn.dataset.act) {
    case "reviewed":
      if (state.focus < 0 && focusables().length) { state.focus = 0; applyFocus(false); }
      toggleFocusedReviewed();
      break;
    case "group-reviewed": toggleFocusedReviewed(); break;
    case "context": expandFocused(); break;
    case "open": { const at = focusedLocation(); if (at) openInEditor(at.path, at.line); break; }
    case "next": moveFocus(1); break;
    case "comment": { const el = focusedElement(); if (el) openCommentBox(el); break; }
    case "pattern-next": patternStep(1); break;
    case "pattern-prev": patternStep(-1); break;
    case "back-range": showReport(state.parent.id); break;
    case "copy-md": copySummary(); break;
    case "post-summary": postSummary(); break;
    case "help": showHelp(true); break;
  }
}

// ---------- views ----------

let lastRoute = "";
function route(opts) {
  if (!state.report) return;
  const [view, id, ...rest] = location.hash.replace(/^#/, "").split("/");
  const key = `${view}/${id || ""}`;
  if (key !== lastRoute) {
    state.focus = -1;
    // The explorer follows the view: patterns while reading a pattern, warnings on the
    // warnings page, files everywhere else.
    state.explorerTab = view === "group" ? "patterns" : view === "warnings" ? "warnings" : "files";
  }
  lastRoute = key;
  setViewTitle(view, id, rest);
  renderExplorer();
  if (view === "file") {
    renderFileView(decodeURIComponent(id || ""), rest[0] || "diff", rest[1] || "");
    renderInspector();
    return;
  }
  if (view === "lib") {
    renderLibraryView(decodeURIComponent(id || ""), Number(rest[0]) || 0);
    renderInspector();
    return;
  }
  if (view === "group") renderGroup(id);
  else if (view === "warnings") renderWarnings();
  else if (view === "files") renderFiles();
  else if (view === "commits") renderCommits();
  else renderReview();
  markSearch($("#content"));
  applyFocus(false);
  if (state.pendingScroll) {
    const selector = state.pendingScroll;
    state.pendingScroll = null;
    scrollToSelector(selector);
  } else if (!opts?.keepScroll) {
    window.scrollTo({ top: 0 });
  }
}

// The stream toolbar names the view: a section, a pattern's transform, or a file.
function setViewTitle(view, id, rest) {
  const el = $("#view-title");
  if (!el) return;
  const v = state.view;
  const meta = (s) => `<span class="meta">${s}</span>`;
  let html;
  if (view === "group") {
    const g = v.groupsById.get(id) || state.report.groups.find((x) => x.id === id);
    html = g ? `<span class="kind ${g.kind}">${g.kind}</span>${groupTransform(g)}` : "Pattern";
  } else if (view === "warnings") {
    html = `Warnings ${meta(v.warnings.length)}`;
  } else if (view === "files") {
    html = `Files ${meta(v.files.length)}`;
  } else if (view === "commits") {
    html = `Commits ${meta(state.commits.length)}`;
  } else if (view === "file") {
    const mode = { diff: "full diff", old: "original", new: "new" }[rest[0] || "diff"];
    html = `<span class="transform">${esc(decodeURIComponent(id || ""))}</span>${meta(mode)}`;
  } else if (view === "lib") {
    html = `<span class="transform">${esc(shortPath(decodeURIComponent(id || "")))}</span>${meta("library")}`;
  } else {
    const s = v.stats;
    html = `Needs review ${meta(`${plural(s.hunks, "hunk")} · ${plural(s.residual_units, "change")} · ${s.hunks_done} reviewed`)}`;
  }
  el.innerHTML = html;
}

function groupTransform(g) {
  return g.kind === "formatting" || g.kind === "docs" || g.kind === "move"
    ? `<span class="transform">${esc(g.label)}</span>`
    : `<span class="transform"><span class="old">${esc(g.old || "∅")}</span><span class="arrow">→</span><span class="new">${esc(g.new || "∅")}</span></span>`;
}

// Wrap search matches in code cells with <mark class="search"> (after rendering, so the
// diff/syntax markup stays untouched).
function markSearch(root) {
  const re = searchRegex();
  if (!re) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => (n.parentElement.closest("td.text, td.side, .path") && n.nodeValue.trim()
      ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT),
  });
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  for (const node of nodes) {
    const text = node.nodeValue;
    re.lastIndex = 0;
    let m, pos = 0;
    const frag = document.createDocumentFragment();
    while ((m = re.exec(text)) && m[0]) {
      frag.append(text.slice(pos, m.index));
      const mark = document.createElement("mark");
      mark.className = "search";
      mark.textContent = m[0];
      frag.append(mark);
      pos = m.index + m[0].length;
    }
    if (!pos) continue;
    frag.append(text.slice(pos));
    node.replaceWith(frag);
  }
}

const TAG_LABEL_MAX = 90;

function shortLabel(label) {
  return label.length > TAG_LABEL_MAX ? label.slice(0, TAG_LABEL_MAX - 1) + "…" : label;
}

const VERIFIED_TITLE = "Verified: the enclosing statement parses to the same program on both sides "
  + "once the diff's renames are applied (docstrings and type annotations ignored).";

function unitTags(unit) {
  const tags = unit.signatures.map((key) => {
    const g = state.groupsByKey.get(key);
    if (!g || !state.view.groupsById.has(g.id)) return "";
    if (g.mechanical) {
      return `<a class="tag" href="#group/${g.id}">${esc(g.kind)}: <span class="code">${esc(shortLabel(g.label))}</span></a>`;
    }
    return `<span class="tag unique">unique ${esc(g.kind)}: <span class="code">${esc(shortLabel(g.label))}</span></span>`;
  });
  for (const n of unit.near || []) {
    const g = state.view.groupsById.get(n.group_id);
    if (g) tags.push(`<a class="tag near" href="#group/${g.id}" title="${esc(n.hint)}">≈ almost <span class="code">${esc(shortLabel(g.label))}</span></a>`);
  }
  if (unit.verified) tags.push(`<span class="tag verified" title="${esc(VERIFIED_TITLE)}">✓ verified</span>`);
  return tags.join("");
}

// The other half of a move, for showing a moved block as a diff against where it came from.
function partnerOf(unit) {
  return unit && unit.partner ? state.report.units[unit.partner] : null;
}

function renderReview() {
  const r = state.report;
  const v = state.view;
  const content = $("#content");
  if (!v.residualHunks.length) {
    const why = state.filters.newOnly ? "Nothing new since last time in the visible files."
      : `Every changed line in the ${filtersActive() ? "visible" : "analyzed"} files matched a
      mechanical pattern. Skim the patterns in the sidebar and check the warnings.`;
    content.innerHTML = `<div class="empty"><h2>Nothing left to review</h2><p>${why}</p></div>`;
    return;
  }
  const byFile = new Map();
  for (const hid of v.residualHunks) {
    const h = r.hunks[hid];
    if (!byFile.has(h.path)) byFile.set(h.path, []);
    byFile.get(h.path).push(h);
  }
  let html = `<div class="page-head">
    <p>Changes that don't belong to a repeated pattern. Lines already explained by a pattern are dimmed.
    Tick a hunk when you've read it; ticked hunks fold up and stay ticked across restarts and new commits.</p></div>
    ${deltaBanner()}
    ${navHint()}`;
  for (const [path, hunks] of byFile) {
    const residual = hunks.reduce((n, h) =>
      n + h.unit_ids.filter((u) => v.units.has(u) && !r.units[u].explained).length, 0);
    const left = hunks.filter((h) => !state.reviewedHunks.has(h.fingerprint)).length;
    html += `<section class="file" data-file-section="${esc(path)}"><header><span class="path">${esc(path)}</span>
      <span class="meta">${plural(residual, "change")}</span>
      ${left ? `<button type="button" class="link" data-review-file="${esc(path)}">Mark all ${hunks.length > 1 ? `${left} ` : ""}reviewed</button>` : ""}
      ${fileLinks(path)}</header>`;
    for (const h of hunks) html += hunkHtml(h);
    html += "</section>";
  }
  content.innerHTML = html;
}

function hunkHtml(h) {
  const done = state.reviewedHunks.has(h.fingerprint);
  return `<div class="hunk${done ? " done" : ""}" data-hunk="${h.id}">
    <div class="hunk-bar">
      <label class="review-box" title="Reviewed (x)"><input type="checkbox" data-review-hunk="${h.fingerprint}" ${done ? "checked" : ""}></label>
      <span>@@ line ${h.new_start} @@</span>
      ${hunkIsNew(h) ? `<span class="badge-new" title="Not in the diff last time">new</span>` : ""}
      <span class="spacer"></span>
      ${state.report.source.pr ? `<button type="button" class="link" data-hunk-comment title="Comment on this hunk in the PR (c)">Comment</button>` : ""}
      <button type="button" class="link" data-hunk-open>${done ? "Show" : "Hide"}</button>
      <button type="button" class="link" data-hunk-context>Show context</button></div>
    <table class="diff">${hunkRows(h)}</table></div>`;
}

// What changed since the previous analysis of the same comparison (new commits pushed).
function deltaBanner() {
  const d = state.delta;
  if (!d.prevHead || (!d.newHunks.size && !d.changed.length)) return "";
  const r = state.report;
  const visibleNew = state.view.residualHunks.filter((hid) => hunkIsNew(r.hunks[hid])).length;
  const parts = [];
  if (d.newHunks.size) parts.push(`${plural(d.newHunks.size, "new change")}${visibleNew !== d.newHunks.size ? ` (${visibleNew} to review)` : ""}`);
  if (d.changed.length) {
    const files = [...new Set(d.changed.map((c) => c.path))];
    parts.push(`${plural(d.changed.length, "change")} you had reviewed ${d.changed.length === 1 ? "was" : "were"} modified (${files.map(esc).join(", ")}) and ${d.changed.length === 1 ? "is" : "are"} unmarked`);
  }
  const f = state.filters;
  return `<div class="banner">
    <strong>Since ${esc(d.prevHead.slice(0, 7))}:</strong> ${parts.join(" · ")}.
    ${d.newHunks.size ? `<button type="button" class="link" id="banner-new">${f.newOnly ? "Show everything" : "Show only what's new"}</button>` : ""}
  </div>`;
}

function hunkRows(h) {
  const lines = [];
  const shown = new Set();
  for (const ln of h.lines) {
    // A block moved here from elsewhere: show the original above it so the reader sees a
    // diff of the move rather than a bare insertion.
    const unit = ln.type === "+" && ln.unit ? state.report.units[ln.unit] : null;
    const from = unit && !shown.has(unit.id) ? partnerOf(unit) : null;
    if (from && from.old.length) {
      shown.add(unit.id);
      lines.push({ t: " ", text: "", from: `moved from ${from.path}:${from.old_start}`, unit: unit.id });
      lines.push(...from.old.map((l, k) => ({
        t: "-", o: from.old_start + k, n: null, text: l.text, unit: unit.id, hl: l.hl, path: from.path,
      })));
    }
    lines.push({ t: ln.type, o: ln.old_no, n: ln.new_no, text: ln.text, unit: ln.unit, hl: ln.hl });
  }
  return diffRows(lines, { tags: true, path: h.path });
}

// Rows for a list of diff lines ({t, o, n, text, unit, hl}) from the file at `opts.path`.
// Lines of units that a pattern explains (or that filters hide) are dimmed, except the units
// in `focus`.
function diffRows(lines, opts = {}) {
  return splitFor(opts.path) ? splitRows(lines, opts) : unifiedRows(lines, opts);
}

function lineState(ln, focus) {
  const unit = ln.unit ? state.report.units[ln.unit] : null;
  const hidden = Boolean(unit) && !state.view.units.has(unit.id);
  const focused = Boolean(unit && focus && focus.has(unit.id));
  return {
    unit,
    hidden,
    focused,
    cls: ln.t === "-" ? "del" : ln.t === "+" ? "add" : "ctx",
    dim: Boolean(unit) && !focused && (unit.explained || hidden),
  };
}

// Pattern tags go above the first line of each unit that isn't dimmed-out or focused.
// `lead` empty cells line the tags up with the code column, which spans `span` columns.
function tagRow(st, tagged, lead, span) {
  if (!st.unit || st.hidden || st.focused || tagged.has(st.unit.id)) return "";
  tagged.add(st.unit.id);
  const t = unitTags(st.unit);
  return t ? `<tr class="tags">${"<td></td>".repeat(lead)}<td colspan="${span}">${t}</td></tr>` : "";
}

// `anchors` adds data-o / data-n line attributes and marks where each run of changes starts
// (used by the file viewer for jumping to a line and between changes).
function unifiedRows(lines, { tags = false, focus = null, anchors = false, path = null, syntax = null } = {}) {
  const sx = syntax || syntaxSpans(lines, path);
  const plain = oneSided(path);
  const tagged = new Set();
  let rows = "";
  let prevChanged = false;
  for (const ln of lines) {
    const st = lineState(ln, focus);
    if (tags) rows += tagRow(st, tagged, 3, 1);
    if (ln.from) { rows += fromRow(ln, 3, 1); continue; }
    const changed = ln.t !== " ";
    const attrs = anchors ? anchorAttrs(ln.o, ln.n) : "";
    const p = ln.path || path;
    rows += `<tr class="${st.cls}${st.dim ? " explained" : ""}${st.focused ? " focus" : ""}${
      anchors && changed && !prevChanged ? " chg-start" : ""}"${attrs}>
      <td class="no">${ln.o ?? ""}</td><td class="no">${ln.n ?? ""}</td>
      <td class="sign">${ln.t === " " ? "" : ln.t}</td>
      <td class="text${plain ? " plain" : ""}"${ln.t === "-" ? navAttrs(p, "o", ln.o) : navAttrs(p, "n", ln.n)}>${
        codeHtml(ln, sx, plain)}</td></tr>`;
    prevChanged = changed;
  }
  return rows;
}

// A label row above lines pulled in from elsewhere ("moved from a.py:12").
function fromRow(ln, lead, span) {
  return `<tr class="tags">${"<td></td>".repeat(lead)}<td colspan="${span}"><span class="from">↓ ${esc(ln.from)}</span></td></tr>`;
}

// Side by side: removed lines on the left, added lines on the right, paired in order within
// each block of changes; unchanged lines appear on both sides.
function splitRows(lines, { tags = false, focus = null, anchors = false, path = null, syntax = null } = {}) {
  const sx = syntax || syntaxSpans(lines, path);
  const tagged = new Set();
  let rows = "";
  let prevChanged = false;
  const cell = (ln, side) => {
    if (!ln) return `<td class="no"></td><td class="side empty"></td>`;
    const st = lineState(ln, focus);
    const num = side === "old" ? ln.o : ln.n;
    return `<td class="no ${st.cls}">${num}</td><td class="side ${st.cls}${st.dim ? " explained" : ""}${
      st.focused ? " focus" : ""}"${navAttrs(ln.path || path, side === "old" ? "o" : "n", num)}>${
      codeHtml(ln, sx)}</td>`;
  };
  for (const [left, right] of pairLines(lines)) {
    if (tags) {
      for (const ln of left === right ? [left] : [left, right].filter(Boolean)) {
        rows += tagRow(lineState(ln, focus), tagged, 1, 3);
      }
    }
    if (left && left.from) { rows += fromRow(left, 1, 3); continue; }
    const changed = left !== right;
    const attrs = anchors ? anchorAttrs(left?.o, right?.n) : "";
    rows += `<tr class="split-row${anchors && changed && !prevChanged ? " chg-start" : ""}"${attrs}>${
      cell(left, "old")}${cell(right, "new")}</tr>`;
    prevChanged = changed;
  }
  return rows;
}

function pairLines(lines) {
  const pairs = [];
  let i = 0;
  while (i < lines.length) {
    if (lines[i].t === " ") {
      pairs.push([lines[i], lines[i]]);
      i++;
      continue;
    }
    const dels = [], adds = [];
    while (i < lines.length && lines[i].t === "-") dels.push(lines[i++]);
    while (i < lines.length && lines[i].t === "+") adds.push(lines[i++]);
    for (let k = 0; k < Math.max(dels.length, adds.length); k++) pairs.push([dels[k] || null, adds[k] || null]);
  }
  return pairs;
}

// Marks a code cell as navigable: Cmd/Ctrl+click resolves names on that side of the diff.
function navAttrs(path, side, line) {
  return path && line ? ` data-p="${esc(path)}" data-s="${side}" data-l="${line}"` : "";
}

function anchorAttrs(o, n) {
  return `${o ? ` data-o="${o}"` : ""}${n ? ` data-n="${n}"` : ""}`;
}

// ---------- syntax highlighting ----------

// Unchanged lines are always syntax-colored (there's no diff to show on them), and so is every
// line of an added or deleted file (`plain`): the whole file is one change, so token-level diff
// highlights carry no information. Other changed lines show the diff's token highlights unless
// syntax mode is on. The gutter (line numbers and sign) always carries the diff colors.
function codeHtml(ln, sx, plain = false) {
  if ((ln.t === "-" || ln.t === "+") && !plain && !state.syntax) {
    return highlight(ln.text, ln.hl);
  }
  const spans = sx && sx.get(ln);
  return spans ? paint(ln.text, spans) : esc(ln.text);
}

function paint(text, spans) {
  let out = "", pos = 0;
  for (const [start, end, cls] of spans) {
    if (start < pos) continue;
    out += esc(text.slice(pos, start)) + `<span class="${cls}">${esc(text.slice(start, end))}</span>`;
    pos = end;
  }
  return out + esc(text.slice(pos));
}

// Spans for each line, keyed by line object. Old and new sides are highlighted as separate
// streams (each line continues the state of the previous line on its side), with unchanged
// lines advancing both.
function syntaxSpans(lines, path) {
  const lang = Syntax.forPath(path);
  if (!lang) return null;
  const out = new Map();
  let oldState = lang.start(), newState = lang.start();
  for (const ln of lines) {
    if (ln.t === "-") {
      const r = lang.line(ln.text, oldState);
      oldState = r.state;
      out.set(ln, r.spans);
    } else {
      const r = lang.line(ln.text, newState);
      newState = r.state;
      out.set(ln, r.spans);
      if (ln.t !== "+") oldState = lang.line(ln.text, oldState).state;
    }
  }
  return out;
}

const fileSyntaxCache = new WeakMap();
function fileSyntax(fd) {
  if (!fileSyntaxCache.has(fd)) fileSyntaxCache.set(fd, syntaxSpans(fd.lines, fd.path));
  return fileSyntaxCache.get(fd);
}

// ---------- side panes (explorer / inspector) ----------

function loadPanes() {
  try { Object.assign(state.panes, JSON.parse(localStorage.getItem("refactor-diff:panes") || "{}")); } catch {}
  applyPanes();
}

function setPane(name, on) {
  state.panes[name] = on;
  try { localStorage.setItem("refactor-diff:panes", JSON.stringify(state.panes)); } catch {}
  applyPanes();
}

function applyPanes() {
  for (const name of ["explorer", "inspector"]) {
    const on = state.panes[name];
    document.body.classList.toggle(`hide-${name}`, !on);
    const btn = $(`#toggle-${name}`);
    btn.setAttribute("aria-pressed", String(on));
    btn.title = `${on ? "Hide" : "Show"} the ${name} (${name === "explorer" ? "\\" : "|"})`;
  }
}

function loadSyntax() {
  try { state.syntax = localStorage.getItem("refactor-diff:highlight") === "syntax"; } catch {}
  document.body.classList.toggle("syntax-mode", state.syntax);
}

function setSyntax(on) {
  state.syntax = on;
  try { localStorage.setItem("refactor-diff:highlight", on ? "syntax" : "diff"); } catch {}
  document.body.classList.toggle("syntax-mode", on);
  rerender();
}

function highlightToggle() {
  return `<div class="layout-toggle" role="group" aria-label="Highlighting">
    <button type="button" data-highlight="diff" aria-pressed="${!state.syntax}"
      title="Changed lines show what changed; unchanged lines are syntax-colored">Diff</button>
    <button type="button" data-highlight="syntax" aria-pressed="${state.syntax}"
      title="Syntax-color all code; changes are marked in the gutter">Syntax</button>
  </div>`;
}

// ---------- diff layout (unified / side by side) ----------

function splitActive() {
  return state.split && !narrowQuery.matches;
}

// Added and deleted files have nothing to put on one side, so they always render unified.
function splitFor(path) {
  return splitActive() && !oneSided(path);
}

function oneSided(path) {
  const f = path && state.report.files.find((x) => x.path === path);
  return Boolean(f) && (f.status === "A" || f.status === "D");
}

function loadLayout() {
  try { state.split = localStorage.getItem("refactor-diff:layout") === "split"; } catch {}
}

function setSplit(on) {
  state.split = on;
  try { localStorage.setItem("refactor-diff:layout", on ? "split" : "unified"); } catch {}
  rerender();
}

// With `path`, the toggle reflects that file: added/deleted files are always unified.
function layoutToggle(path) {
  const split = path ? splitFor(path) : splitActive();
  const why = narrowQuery.matches ? "Window too narrow for side-by-side"
    : path && oneSided(path) ? "Added and deleted files are always shown unified" : "";
  return `<div class="layout-toggle" role="group" aria-label="Diff layout">
    <button type="button" data-layout="unified" aria-pressed="${!split}" ${why ? "disabled" : ""}>Unified</button>
    <button type="button" data-layout="split" aria-pressed="${split}" ${why ? `disabled title="${why}"` : 'title="Side-by-side diff"'}>Split</button>
  </div>`;
}

function bindLayoutToggle(root) {
  for (const b of root.querySelectorAll("[data-layout]")) {
    b.addEventListener("click", () => setSplit(b.dataset.layout === "split"));
  }
  for (const b of root.querySelectorAll("[data-highlight]")) {
    b.addEventListener("click", () => setSyntax(b.dataset.highlight === "syntax"));
  }
}

// ---------- whole-file diffs: context, original and new versions ----------

function loadFileDiff(path) {
  if (!state.fileDiffs.has(path)) {
    const url = `/api/report/${state.report.id}/file?path=${encodeURIComponent(path)}`;
    state.fileDiffs.set(path, api(url).catch((e) => { state.fileDiffs.delete(path); throw e; }));
  }
  return state.fileDiffs.get(path);
}

// line is "o12" (old line 12) or "n12" (new line 12)
function fileHref(path, mode, line) {
  return `#file/${encodeURIComponent(path)}/${mode}${line ? "/" + line : ""}`;
}

function fileLinks(path, { oldLine, newLine } = {}) {
  const f = state.report.files.find((x) => x.path === path);
  const links = [];
  if (f && f.status !== "A") links.push(`<a href="${fileHref(path, "old", oldLine && "o" + oldLine)}">Original</a>`);
  if (f && f.status !== "D") links.push(`<a href="${fileHref(path, "new", newLine && "n" + newLine)}">New</a>`);
  const at = newLine ? "n" + newLine : oldLine ? "o" + oldLine : "";
  links.push(`<a href="${fileHref(path, "diff", at)}">Full diff</a>`);
  if (!f || f.status !== "D") links.push(editorLink(path, newLine || oldLine));
  return `<span class="file-links">${links.join("")}</span>`;
}

// ---- open in editor ----
// The editor opens the file in your checkout, which is the head revision only when the
// source is the working tree (or the head branch is checked out).
const EDITOR_TITLE = "Open in your editor (opens your checkout, which may differ from this revision)";

function editorHref(path, line, col) {
  if (!state.editor) return null;
  const abs = `${state.repo.replace(/\/$/, "")}/${path}`;
  return state.editor
    .replace("{path}", encodeURI(abs))
    .replace("{line}", String(line || 1))
    .replace("{col}", String(col || 1));
}

function editorLink(path, line, label = "Open") {
  const href = editorHref(path, line);
  return href ? `<a class="open" href="${esc(href)}" title="${EDITOR_TITLE}">${label}</a>` : "";
}

function openInEditor(path, line) {
  const href = editorHref(path, line);
  if (href) {
    toast(`Opening ${path}:${line || 1} in your editor…`);
    window.location.assign(href);
  }
}

// A context window: a slice [lo, hi) of a file's whole diff that grows with "show more".
function renderCtx(box, fd) {
  // Widen the window so it never cuts through a block of changed lines.
  const changed = (i) => fd.lines[i] && fd.lines[i].t !== " ";
  let lo = Number(box.dataset.lo), hi = Number(box.dataset.hi);
  while (lo > 0 && changed(lo) && changed(lo - 1)) lo--;
  while (hi < fd.lines.length && changed(hi) && changed(hi - 1)) hi++;
  box.dataset.lo = lo;
  box.dataset.hi = hi;
  const focus = new Set((box.dataset.focus || "").split(",").filter(Boolean));
  const more = (dir, n) => `<button type="button" class="link" data-expand="${dir}">${dir === "up" ? "↑" : "↓"} ${plural(n, "more line")}</button>`;
  const all = (dir, n) => `<button type="button" class="link" data-expand="${dir}-all">${dir === "up" ? "to start" : "to end"} (${n})</button>`;
  let rows = "";
  if (lo > 0) rows += `<tr class="expand"><td colspan="4">${more("up", Math.min(CTX_STEP, lo))}${lo > CTX_STEP ? " · " + all("up", lo) : ""}</td></tr>`;
  rows += diffRows(fd.lines.slice(lo, hi), {
    tags: !focus.size, focus, path: box.dataset.path, syntax: fileSyntax(fd),
  });
  const rest = fd.lines.length - hi;
  if (rest > 0) rows += `<tr class="expand"><td colspan="4">${more("down", Math.min(CTX_STEP, rest))}${rest > CTX_STEP ? " · " + all("down", rest) : ""}</td></tr>`;
  box.querySelector("table").innerHTML = rows;
}

async function openCtx(box, path, lo, hi) {
  const fd = await loadFileDiff(path);
  box.classList.add("ctx");
  box.dataset.path = path;
  box.dataset.lo = Math.max(0, lo(fd));
  box.dataset.hi = Math.min(fd.lines.length, hi(fd));
  renderCtx(box, fd);
}

function onContentChange(e) {
  const box = e.target.closest("[data-review-hunk]");
  if (!box) return;
  const hunk = box.closest(".hunk");
  hunk.classList.toggle("done", box.checked);
  hunk.classList.remove("open");
  const open = hunk.querySelector("[data-hunk-open]");
  if (open) open.textContent = box.checked ? "Show" : "Hide";
  markHunks([box.dataset.reviewHunk], box.checked);
}

async function onContentClick(e) {
  const btn = e.target.closest("button");
  if (!btn) return;
  try {
    if (btn.dataset.expand) {
      const box = btn.closest(".ctx");
      const fd = await loadFileDiff(box.dataset.path);
      const lo = Number(box.dataset.lo), hi = Number(box.dataset.hi);
      const dir = btn.dataset.expand;
      if (dir === "up") box.dataset.lo = Math.max(0, lo - CTX_STEP);
      if (dir === "up-all") box.dataset.lo = 0;
      if (dir === "down") box.dataset.hi = Math.min(fd.lines.length, hi + CTX_STEP);
      if (dir === "down-all") box.dataset.hi = fd.lines.length;
      renderCtx(box, fd);
    } else if ("unitContext" in btn.dataset) {
      const box = btn.closest(".occurrence");
      const u = state.report.units[box.dataset.unit];
      if (box.classList.contains("ctx")) {
        box.classList.remove("ctx");
        box.querySelector("table").innerHTML = unitRows(u);
        btn.textContent = "Show context";
        return;
      }
      box.dataset.focus = u.id;
      await openCtx(box, u.path,
        (fd) => fd.lines.findIndex((ln) => ln.unit === u.id) - CTX_PAD,
        (fd) => fd.lines.findLastIndex((ln) => ln.unit === u.id) + 1 + CTX_PAD);
      btn.textContent = "Hide context";
    } else if ("hunkContext" in btn.dataset) {
      const box = btn.closest(".hunk");
      const h = state.report.hunks[box.dataset.hunk];
      const first = h.lines[0];
      const start = (fd) => fd.lines.findIndex((ln) =>
        ln.t === first.type && ln.o === first.old_no && ln.n === first.new_no);
      await openCtx(box, h.path, (fd) => start(fd) - CTX_STEP,
        (fd) => start(fd) + h.lines.length + CTX_STEP);
      btn.remove();
    } else if ("hunkOpen" in btn.dataset) {
      const box = btn.closest(".hunk");
      box.classList.toggle("open");
      btn.textContent = box.classList.contains("done") && !box.classList.contains("open") ? "Show" : "Hide";
    } else if (btn.dataset.reviewFile) {
      const r = state.report;
      const fps = state.view.residualHunks.map((hid) => r.hunks[hid])
        .filter((h) => h.path === btn.dataset.reviewFile && !state.reviewedHunks.has(h.fingerprint))
        .map((h) => h.fingerprint);
      markHunks(fps, true);
      route({ keepScroll: true });
    } else if ("hunkComment" in btn.dataset) {
      openCommentBox(btn.closest(".hunk"));
    } else if (btn.dataset.analyzeCommit) {
      await analyzeCommit(btn.dataset.analyzeCommit);
    } else if (btn.id === "banner-new") {
      state.filters.newOnly = !state.filters.newOnly;
      rerender();
    }
  } catch (err) {
    showError(err.message);
  }
}

function unitRows(u) {
  // A moved block is shown against the place it came from.
  const from = u.new.length ? partnerOf(u) : null;
  const lines = from ? [
    { t: " ", text: "", from: `moved from ${from.path}:${from.old_start}`, unit: u.id },
    ...from.old.map((ln, k) => ({ t: "-", o: from.old_start + k, n: null, text: ln.text, unit: u.id, hl: ln.hl, path: from.path })),
  ] : u.old.map((ln, k) => ({ t: "-", o: u.old_start + k, n: null, text: ln.text, unit: u.id, hl: ln.hl }));
  lines.push(...u.new.map((ln, k) => ({ t: "+", o: null, n: u.new_start + k, text: ln.text, unit: u.id, hl: ln.hl })));
  return diffRows(lines, { focus: new Set([u.id]), path: u.path });
}

function renderGroup(id) {
  const r = state.report;
  const g = state.view.groupsById.get(id);
  const content = $("#content");
  if (!g) {
    const hidden = r.groups.some((x) => x.id === id);
    content.innerHTML = `<div class="empty"><h2>${hidden ? "Pattern hidden by filters" : "Pattern not found"}</h2>
      ${hidden ? "<p>All of its occurrences are in files or changes your filters hide.</p>" : ""}</div>`;
    return;
  }

  const isDone = state.reviewed.has(g.id);
  const warnCount = state.view.warnings.filter((w) => w.group_id === g.id).length;
  const files = new Set(g.visible.map((uid) => r.units[uid].path));
  const hiddenHere = g.unit_ids.length - g.visible.length;
  let units = g.visible.map((uid) => r.units[uid]);
  const verifiedCount = units.filter((u) => u.verified).length;
  // A move is one occurrence shown as old block → new block; its deleted half isn't listed.
  const moved = g.kind === "move" ? units.find((u) => u.new.length && u.partner) : null;
  if (moved) units = [moved, ...units.filter((u) => u !== moved && u.id !== moved.partner)];
  const count = g.kind === "move" ? `moved block${units.length > 1 ? ` + ${plural(units.length - 1, "import edit")}` : ""}`
    : `${plural(g.visible.length, "occurrence")} in ${plural(files.size, "file")}`;
  let html = `<div class="page-head">
      <span class="meta">${count}${hiddenHere ? ` (${hiddenHere} hidden by filters)` : ""}${
        verifiedCount ? ` · <span class="tag verified" title="${esc(VERIFIED_TITLE)}">✓ ${g.kind === "move" ? "verified" : `${verifiedCount} of ${g.visible.length} verified`}</span>` : ""}${
        warnCount ? ` · <a href="#warnings">${plural(warnCount, "warning")}</a>` : ""}</span>
      <span class="spacer"></span>
      <label class="toggle"><input type="checkbox" id="group-reviewed" ${isDone ? "checked" : ""}> Reviewed</label>
    </div>`;
  const details = Object.entries(g.details);
  if (details.length && g.kind !== "move") {
    html += `<div class="chips">${details.map(([d, n]) => `<span class="chip">${esc(d)} <b>×${n}</b></span>`).join("")}</div>`;
  }

  const byFile = new Map();
  for (const u of units.slice(0, state.shown)) {
    if (!byFile.has(u.path)) byFile.set(u.path, []);
    byFile.get(u.path).push(u);
  }
  let importsHead = false;
  for (const [path, us] of byFile) {
    if (moved && !us.includes(moved) && !importsHead) {
      importsHead = true;
      html += `<h3 class="sub-head">Imports updated for this move</h3>`;
    }
    html += `<section class="file"><header><span class="path">${esc(path)}</span>
      <span class="meta">×${us.length}</span>${fileLinks(path)}</header>`;
    for (const u of us) {
      const where = { oldLine: u.old.length ? u.old_start : null, newLine: u.new.length ? u.new_start : null };
      const from = u === moved ? partnerOf(u) : null;
      html += `<div class="occurrence" data-unit="${u.id}"><table class="diff">${unitRows(u)}</table>
        <div class="occ-actions">
          ${hunkIsNew(r.hunks[u.hunk_id]) ? `<span class="badge-new" title="Not in the diff last time">new</span>` : ""}
          ${u.explained ? "" : `<a class="badge-link" href="#review">also has other changes — see Needs review</a>`}
          ${from ? `<span class="meta">moved from ${esc(from.path)}:${from.old_start}</span>${fileLinks(from.path, { oldLine: from.old_start })}` : ""}
          <button type="button" class="link" data-unit-context>Show context</button>
          ${fileLinks(path, where)}
        </div></div>`;
    }
    html += "</section>";
  }
  if (units.length > state.shown) {
    html += `<button class="toggle more" id="show-more">Show ${Math.min(PAGE, units.length - state.shown)} more of ${units.length - state.shown}</button>`;
  }
  content.innerHTML = html;
  $("#group-reviewed").addEventListener("change", (e) => toggleReviewed(g.id, e.target.checked));
  $("#show-more")?.addEventListener("click", () => { state.shown += PAGE; renderGroup(id); });
}

function renderWarnings() {
  const r = state.report;
  const v = state.view;
  const content = $("#content");
  if (!v.warnings.length) {
    content.innerHTML = `<div class="empty"><h2>No warnings</h2>
      <p>No references to renamed definitions are left behind, no symbol was renamed two different ways,
      and no leftover change looks like a near miss of a pattern.</p></div>`;
    return;
  }
  let html = `<div class="page-head">
    <p>Possible problems with the refactor: references to a renamed definition that no longer exists, a symbol
    renamed two different ways, or a leftover change that almost matches a pattern (a likely typo).</p></div>`;
  for (const w of v.warnings) {
    const g = r.groups.find((x) => x.id === w.group_id);
    const re = g && g.kind === "rename"
      ? new RegExp(`\\b${g.old.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "g")
      : null;
    let locs = "";
    for (const loc of w.locations) {
      const text = loc.text.trimStart();
      const ranges = re ? [...text.matchAll(re)].map((m) => [m.index, m.index + m[0].length]) : [];
      locs += `<tr><td class="where"><a href="${fileHref(loc.path, "new", "n" + loc.line)}">${esc(loc.path)}:${loc.line}</a> ${editorLink(loc.path, loc.line, "↗")}</td>
        <td class="text">${highlight(text, ranges)}</td></tr>`;
    }
    const unlisted = w.total - w.locations.length - (w.filteredOut || 0);
    const notes = [
      unlisted > 0 ? `…and ${unlisted} more` : "",
      w.filteredOut ? `${w.filteredOut} in files hidden by filters` : "",
    ].filter(Boolean).join(" · ");
    const more = notes ? `<div class="where">${notes}</div>` : "";
    html += `<div class="warning" id="warning-${v.warnings.indexOf(w)}">
      <div>${esc(w.message)}${g && v.groupsById.has(g.id) ? ` · <a href="#group/${g.id}">view pattern</a>` : ""}</div>
      ${locs ? `<table class="locations">${locs}</table>${more}` : ""}</div>`;
  }
  content.innerHTML = html;
}

function renderFiles() {
  const r = state.report;
  const label = { A: "added", M: "modified", D: "deleted", R: "renamed" };
  const v = state.view;
  let html = `<div class="page-head">
    <p>Python, TypeScript and JavaScript files are analyzed; other files are listed but not collapsed.
    Files hidden by filters are dimmed.</p></div>
    <table class="files"><thead><tr><th>Status</th><th>Path</th><th>Kind</th><th>+/−</th><th>To review</th><th>Analyzed</th></tr></thead><tbody>`;
  for (const f of r.files) {
    html += `<tr class="${v.fileVisible(f.path) ? "" : "filtered"}">
      <td><span class="status-letter" title="${label[f.status] || f.status}">${esc(f.status)}</span></td>
      <td class="path"><a href="${fileHref(f.path, "diff")}">${f.old_path ? esc(f.old_path) + " → " : ""}${esc(f.path)}</a></td>
      <td><span class="cat">${esc(f.category)}</span></td>
      <td class="num"><span class="adds">+${f.additions}</span> <span class="dels">−${f.deletions}</span></td>
      <td class="num">${f.analyzed ? `${f.residual_units} / ${f.units ?? ""}` : "—"}</td>
      <td>${f.analyzed ? (f.parse_ok ? "yes" : "partially (syntax error)") : "no"}</td></tr>`;
  }
  content.innerHTML = html + "</tbody></table>";
}

// ---------- file viewer ----------

const MODES = [["diff", "Diff"], ["old", "Original"], ["new", "New"]];

async function renderFileView(path, mode, line) {
  if (!state.report.files.some((f) => f.path === path)) return renderBrowseView(path, mode, line);
  const content = $("#content");
  const hash = location.hash;
  content.innerHTML = `<div class="empty"><span class="spinner"></span> Loading ${esc(path)}…</div>`;
  let fd;
  try {
    fd = await loadFileDiff(path);
  } catch (e) {
    showError(e.message);
    return;
  }
  if (location.hash !== hash) return; // navigated away while loading

  const rows = mode === "old" ? fd.lines.filter((ln) => ln.t !== "+")
    : mode === "new" ? fd.lines.filter((ln) => ln.t !== "-")
    : fd.lines;
  const tabs = MODES.map(([m, label]) => {
    const disabled = (m === "old" && fd.status === "A") || (m === "new" && fd.status === "D");
    return disabled ? `<span class="tab disabled">${label}</span>`
      : `<a class="tab" href="${fileHref(path, m, line)}" ${m === mode ? 'aria-current="page"' : ""}>${label}</a>`;
  }).join("");
  const title = { diff: "Full diff", old: "Original", new: "New" }[mode];
  const counts = mode === "old" ? plural(fd.old_lines, "line") : mode === "new" ? plural(fd.new_lines, "line")
    : `<span class="adds">+${fd.lines.filter((l) => l.t === "+").length}</span> <span class="dels">−${fd.lines.filter((l) => l.t === "-").length}</span>`;

  let body = mode === "diff" ? diffRows(rows, { anchors: true, path }) : "";
  let prevChanged = false;
  const sx = syntaxSpans(rows, path);
  const plain = oneSided(path);
  for (const ln of mode === "diff" ? [] : rows) {
    const changed = ln.t !== " ";
    const num = mode === "old" ? ln.o : ln.n;
    const cls = ln.t === "-" ? "del" : ln.t === "+" ? "add" : "ctx";
    body += `<tr class="${cls}${changed && !prevChanged ? " chg-start" : ""}"${anchorAttrs(ln.o, ln.n)}>
      <td class="no">${num}</td>
      <td class="sign">${ln.t === " " ? "" : ln.t}</td>
      <td class="text${plain ? " plain" : ""}"${navAttrs(path, mode === "old" ? "o" : "n", num)}>${codeHtml(ln, sx, plain)}</td></tr>`;
    prevChanged = changed;
  }
  const empty = !rows.length
    ? `<div class="empty"><p>${mode === "old" ? "This file was added; there is no original version."
      : mode === "new" ? "This file was deleted; there is no new version." : "Empty file."}</p></div>` : "";

  content.innerHTML = `
    <div class="page-head viewer-head">
      <p>${fd.old_path ? `<span class="code">${esc(fd.old_path)}</span> → ` : ""}${title} · ${counts} · <span class="cat">${esc(fd.category)}</span></p>
    </div>
    <div class="viewer-bar">
      ${backButton()}
      <nav class="tabs" aria-label="File version">${tabs}</nav>
      ${highlightToggle()}
      ${mode === "diff" ? layoutToggle(path) : ""}
      <span class="spacer"></span>
      ${fd.status !== "D" ? `<a class="toggle" href="${esc(editorHref(path, line ? Number(line.slice(1)) : 1) || "#")}" title="${EDITOR_TITLE}">Open in editor</a>` : ""}
      <button type="button" class="toggle" data-jump="prev" title="Previous change (p)">↑ Prev change</button>
      <button type="button" class="toggle" data-jump="next" title="Next change (n)">↓ Next change</button>
    </div>
    ${navHint()}
    ${empty || `<section class="file viewer"><table class="diff">${body}</table></section>`}`;

  $("#back-btn").addEventListener("click", () => history.back());
  bindLayoutToggle(content);
  for (const b of content.querySelectorAll("[data-jump]")) {
    b.addEventListener("click", () => jumpChange(b.dataset.jump === "next" ? 1 : -1));
  }
  const target = line && nearestRow(content, line[0], Number(line.slice(1)));
  if (target) {
    target.classList.add("target");
    target.scrollIntoView({ block: "center" });
  } else {
    window.scrollTo({ top: 0 });
  }
}

// Lives in the sticky viewer bar so it stays reachable while scrolling a long file.
function backButton() {
  return `<button type="button" class="toggle" id="back-btn" title="Back">← Back</button>`;
}

// The row for old ("o") or new ("n") line `num`, or the closest one when that line doesn't exist
// in this version (e.g. a line added in the new file, viewed in the original).
function nearestRow(root, side, num) {
  let best = null, bestDist = Infinity;
  for (const tr of root.querySelectorAll(`tr[data-${side}]`)) {
    const dist = Math.abs(Number(tr.dataset[side]) - num);
    if (dist < bestDist) { best = tr; bestDist = dist; }
    if (dist === 0) break;
  }
  return best;
}

function jumpChange(dir) {
  const starts = [...document.querySelectorAll("#content tr.chg-start")];
  if (!starts.length) return;
  const mid = window.innerHeight / 3;
  const tops = starts.map((el) => el.getBoundingClientRect().top);
  const i = dir > 0 ? tops.findIndex((t) => t > mid + 2)
    : tops.findLastIndex((t) => t < mid - 2);
  const el = starts[i === -1 ? (dir > 0 ? starts.length - 1 : 0) : i];
  el.scrollIntoView({ block: "start" });
  window.scrollBy({ top: -mid });
}

// ---------- keyboard ----------

const KEY_HELP = [
  ["Review", [
    ["j", "k", "next / previous hunk (or occurrence on a pattern page)"],
    ["x", "", "mark the focused hunk reviewed (the pattern, on a pattern page)"],
    ["e", "", "show context for the focused hunk; again to reveal more"],
    ["o", "", "open the focused hunk in your editor"],
    ["c", "", "comment on the focused hunk (pull requests)"],
  ]],
  ["Navigate", [
    ["]", "[", "next / previous mechanical pattern"],
    ["}", "{", "next / previous commit of the range"],
    ["g r", "g w", "go to Needs review / Warnings"],
    ["g f", "g c", "go to Files / Commits"],
    ["n", "p", "next / previous change in the file viewer"],
    ["/", "", "search"],
  ]],
  ["Other", [
    [`${MOD_KEY}-click`, `${MOD_KEY}⇧-click`, "go to definition / find references"],
    ["\\", "|", "show / hide the explorer, the inspector"],
    ["?", "", "this help"],
    ["esc", "", "close panels, clear the search box"],
  ]],
];

let pendingG = 0;

// The hunks (review page) or occurrences (pattern page) that j/k move between.
function focusables() {
  return [...document.querySelectorAll("#content .hunk, #content .occurrence")];
}

function applyFocus(scroll = true) {
  const items = focusables();
  for (const el of document.querySelectorAll(".kbd-focus")) el.classList.remove("kbd-focus");
  if (state.focus < 0 || state.focus >= items.length) {
    state.focus = Math.min(state.focus, items.length - 1);
    renderInspector();
    markExplorerFile();
    return;
  }
  const el = items[state.focus];
  el.classList.add("kbd-focus");
  if (scroll) {
    const r = el.getBoundingClientRect();
    const top = parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--stick-h")) || 0;
    const bottom = window.innerHeight - ($("#status").offsetHeight || 0);
    if (r.top < top + 8 || r.bottom > bottom - 8) {
      window.scrollBy({ top: r.top - top - 60 });
    }
  }
  renderInspector();
  markExplorerFile();
}

function moveFocus(delta) {
  const n = focusables().length;
  if (!n) return;
  state.focus = state.focus < 0 ? (delta > 0 ? 0 : n - 1) : Math.max(0, Math.min(n - 1, state.focus + delta));
  applyFocus();
}

function focusedElement() {
  const items = focusables();
  return state.focus >= 0 ? items[state.focus] || null : null;
}

// Where the focused hunk/occurrence lives: {path, line} on the new side when possible.
function focusedLocation() {
  const el = focusedElement();
  if (!el) return null;
  const r = state.report;
  if (el.dataset.hunk) {
    const h = r.hunks[el.dataset.hunk];
    const at = hunkAnchor(h);
    return { path: h.path, line: at.new_no || at.old_no || h.new_start, hunk: h };
  }
  const u = r.units[el.dataset.unit];
  return { path: u.path, line: u.new.length ? u.new_start : u.old_start, unit: u };
}

function toggleFocusedReviewed() {
  const el = focusedElement();
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  if (view === "group" && id) {
    const box = $("#group-reviewed");
    if (box) { box.checked = !box.checked; box.dispatchEvent(new Event("change")); }
    return;
  }
  const box = el?.querySelector("[data-review-hunk]");
  if (!box) return;
  box.checked = !box.checked;
  box.dispatchEvent(new Event("change", { bubbles: true }));
  if (box.checked) {
    // On to the next hunk that still needs reading.
    const items = focusables();
    const next = items.findIndex((x, i) => i > state.focus && !x.classList.contains("done"));
    if (next >= 0) { state.focus = next; applyFocus(); }
  }
}

function expandFocused() {
  const el = focusedElement();
  if (!el) return;
  const btn = el.querySelector("[data-hunk-context], [data-unit-context]:not(.ctx [data-unit-context])");
  if (btn && !(el.classList.contains("ctx") && "unitContext" in btn.dataset)) { btn.click(); return; }
  el.querySelector('[data-expand="down"]')?.click();
}

function patternStep(delta) {
  const mech = state.view.groups.filter((g) => g.mechanical);
  if (!mech.length) return;
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  const i = view === "group" ? mech.findIndex((g) => g.id === id) : -1;
  const next = i + delta;
  if (next < 0) { location.hash = "#review"; return; }
  if (next >= mech.length) return;
  location.hash = `#group/${mech[next].id}`;
}

function showHelp(on = !$("#help").open) {
  const dlg = $("#help");
  if (!on) { dlg.close(); return; }
  dlg.innerHTML = `<header><h3>Keyboard shortcuts</h3><button type="button" class="close" aria-label="Close" id="help-close">×</button></header>
    ${KEY_HELP.map(([title, rows]) => `<h4>${title}</h4><table>${rows.map(([a, b, what]) =>
      `<tr><td><kbd>${esc(a)}</kbd>${b ? ` <kbd>${esc(b)}</kbd>` : ""}</td><td>${esc(what)}</td></tr>`).join("")}</table>`).join("")}`;
  $("#help-close").addEventListener("click", () => dlg.close());
  dlg.showModal();
}

function onKey(e) {
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  const inField = e.target.closest?.("input, textarea, select, [contenteditable]");
  if (e.key === "Escape") {
    if ($("#help").open) $("#help").close();
    hideNavPanel();
    if (inField) e.target.blur();
    $("#comment-box")?.remove();
    return;
  }
  if (inField || !state.report) return;
  if (pendingG) {
    clearTimeout(pendingG);
    pendingG = 0;
    const go = { r: "#review", w: "#warnings", f: "#files", c: "#commits" }[e.key];
    if (go) { location.hash = go; e.preventDefault(); }
    return;
  }
  const inFile = location.hash.startsWith("#file/") || location.hash.startsWith("#lib/");
  switch (e.key) {
    case "?": showHelp(); break;
    case "\\": setPane("explorer", !state.panes.explorer); break;
    case "|": setPane("inspector", !state.panes.inspector); break;
    case "/": $("#search-input")?.focus(); e.preventDefault(); break;
    case "g": pendingG = setTimeout(() => { pendingG = 0; }, 900); break;
    case "]": patternStep(1); break;
    case "[": patternStep(-1); break;
    case "}": if (state.commits.length) commitStep(1); break;
    case "{": if (state.commits.length) commitStep(-1); break;
    case "n": if (inFile) jumpChange(1); break;
    case "p": if (inFile) jumpChange(-1); break;
    case "j": if (!inFile) moveFocus(1); else return; break;
    case "k": if (!inFile) moveFocus(-1); else return; break;
    case "x": toggleFocusedReviewed(); break;
    case "e": expandFocused(); break;
    case "o": { const at = focusedLocation(); if (at) openInEditor(at.path, at.line); break; }
    case "c": { const el = focusedElement(); if (el) openCommentBox(el); break; }
    default: return;
  }
  e.preventDefault();
}

document.addEventListener("keydown", onKey);

// ---------- code navigation (go to definition / find references) ----------

const ID_CHAR = /[\p{L}\p{N}_$]/u;

function navHint() {
  return `<p class="nav-hint"><kbd>${MOD_KEY}</kbd>-click a name to go to its definition ·
    <kbd>${MOD_KEY}</kbd><kbd>⇧</kbd>-click to find references · <kbd>?</kbd> keyboard shortcuts</p>`;
}

// Character offset of the point (x, y) within a code cell's text, or null.
function caretOffset(cell, x, y) {
  let node = null, offset = 0;
  if (document.caretPositionFromPoint) {
    const pos = document.caretPositionFromPoint(x, y);
    if (pos) ({ offsetNode: node, offset } = pos);
  } else if (document.caretRangeFromPoint) {
    const range = document.caretRangeFromPoint(x, y);
    if (range) ({ startContainer: node, startOffset: offset } = range);
  }
  if (!node || !cell.contains(node)) return null;
  let col = 0;
  const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n === node) return col + offset;
    col += n.length;
  }
  return null;
}

// The identifier at (or just before) `offset` in `text`.
function wordAt(text, offset) {
  let i = offset;
  if (!ID_CHAR.test(text[i] ?? "") && ID_CHAR.test(text[i - 1] ?? "")) i--;
  if (!ID_CHAR.test(text[i] ?? "")) return null;
  let start = i, end = i;
  while (start > 0 && ID_CHAR.test(text[start - 1])) start--;
  while (end < text.length && ID_CHAR.test(text[end])) end++;
  if (/^\p{N}/u.test(text[start])) return null; // a number, not a name
  return { start, end, word: text.slice(start, end) };
}

function wordUnder(e) {
  const cell = e.target.closest?.("[data-l]");
  if (!cell) return null;
  const offset = caretOffset(cell, e.clientX, e.clientY);
  const w = offset == null ? null : wordAt(cell.textContent, offset);
  return w && { cell, ...w };
}

// Underline the name under the pointer while the modifier is held (CSS Custom Highlight API;
// skipped where unsupported).
const hoverHighlight = window.CSS?.highlights && typeof Highlight === "function";
function setHoverWord(hit) {
  if (!hoverHighlight) return;
  if (!hit) { CSS.highlights.delete("nav-word"); return; }
  const range = document.createRange();
  let pos = 0, started = false;
  const walker = document.createTreeWalker(hit.cell, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (!started && hit.start < pos + n.length) { range.setStart(n, hit.start - pos); started = true; }
    if (started && hit.end <= pos + n.length) { range.setEnd(n, hit.end - pos); break; }
    pos += n.length;
  }
  CSS.highlights.set("nav-word", new Highlight(range));
}

let hoverFrame = 0;
function onCodeMouseMove(e) {
  if (!modDown(e)) { document.body.classList.remove("nav-armed"); setHoverWord(null); return; }
  cancelAnimationFrame(hoverFrame);
  hoverFrame = requestAnimationFrame(() => {
    const hit = wordUnder(e);
    document.body.classList.toggle("nav-armed", Boolean(hit));
    setHoverWord(hit);
  });
}

function onCodeMouseDown(e) {
  if (modDown(e) && e.target.closest?.("[data-l]")) e.preventDefault(); // no text selection
}

function onCodeClick(e) {
  if (!modDown(e)) return;
  const hit = wordUnder(e);
  if (!hit) return;
  e.preventDefault();
  e.stopPropagation();
  const query = {
    path: hit.cell.dataset.p,
    side: hit.cell.dataset.s === "o" ? "old" : "new",
    line: Number(hit.cell.dataset.l),
    col: [...hit.cell.textContent.slice(0, hit.start)].length, // code points, like Python
    word: hit.word,
  };
  runNavigation(e.shiftKey ? "references" : "definition", query);
}

async function runNavigation(action, query) {
  const what = action === "definition" ? `Finding the definition of ${query.word}`
    : `Finding references to ${query.word}`;
  toast(`${what}…`, { busy: true, sticky: true });
  let res;
  try {
    res = await api(`/api/report/${state.report.id}/navigate`, { action, ...query });
  } catch (e) {
    toast(e.message, { error: true });
    return;
  }
  const locs = res.locations;
  if (action === "references") {
    hideToast();
    showNavPanel(action, query, res);
    return;
  }
  if (!locs.length) {
    toast(`No definition found for ${query.word}.`, { error: true });
  } else if (locs.length === 1 && locs[0].kind !== "builtin") {
    hideNavPanel();
    toast(`${query.word} → ${shortPath(locs[0].path)}:${locs[0].line}`);
    location.hash = locHref(locs[0], res.side);
  } else if (locs.length === 1) {
    toast(`${query.word} is a builtin (${locs[0].path}); there's no source to show.`);
  } else {
    hideToast();
    showNavPanel(action, query, res);
  }
}

function locHref(loc, side) {
  if (loc.kind === "library") return `#lib/${encodeURIComponent(loc.path)}/${loc.line ?? ""}`;
  let path = loc.path;
  if (side === "old") {
    // A renamed file's old path: open it by its new path so the diff-aware viewer is used.
    const renamed = state.report.files.find((f) => f.old_path === path);
    if (renamed) path = renamed.path;
  }
  return fileHref(path, side, `${side === "old" ? "o" : "n"}${loc.line}`);
}

function shortPath(p) {
  for (const dir of ["/site-packages/", "/node_modules/"]) {
    const i = p.lastIndexOf(dir);
    if (i >= 0) return p.slice(i + dir.length);
  }
  const t = p.lastIndexOf("/typeshed/");
  return t >= 0 ? p.slice(t + 1) : p;
}

// ---- results panel ----

function showNavPanel(action, query, res) {
  const panel = $("#nav-panel");
  const locs = res.locations;
  const sideName = res.side === "old" ? "original (base)" : state.report.source.head_sha ? "new (head)" : "working tree";
  const byFile = new Map();
  for (const loc of locs) {
    const key = `${loc.kind}:${loc.path}`;
    if (!byFile.has(key)) byFile.set(key, []);
    byFile.get(key).push(loc);
  }
  let list = "";
  for (const [, items] of byFile) {
    const first = items[0];
    const label = first.kind === "repo" ? esc(first.path)
      : `<span class="lib-badge">${first.kind === "builtin" ? "builtin" : "library"}</span> ${esc(shortPath(first.path))}`;
    list += `<div class="nav-file"><div class="nav-file-head">${label}<span class="n">${items.length}</span></div>`;
    for (const loc of items) {
      const text = loc.text.trimStart();
      const cut = loc.text.length - text.length;
      const hl = loc.col == null ? [] : [[loc.col - cut, loc.col - cut + loc.name.length]];
      const body = `<span class="ln">${loc.line ?? ""}</span><code>${highlight(text, hl) || esc(loc.name)}</code>${
        loc.is_definition && action === "references" ? '<span class="def-badge">def</span>' : ""}`;
      list += loc.kind === "builtin"
        ? `<div class="nav-item-row disabled">${body}</div>`
        : `<a class="nav-item-row" href="${locHref(loc, res.side)}">${body}</a>`;
    }
    list += "</div>";
  }
  const files = byFile.size;
  const title = action === "definition"
    ? `Definitions of <code>${esc(query.word)}</code>`
    : `References to <code>${esc(query.word)}</code>`;
  panel.innerHTML = `
    <header>
      <div>
        <h3>${title}</h3>
        <p>${plural(locs.length, "result")} in ${plural(files, "file")} · ${sideName}</p>
      </div>
      <button type="button" class="close" aria-label="Close" id="nav-close">×</button>
    </header>
    ${action === "definition" ? `<div class="nav-actions"><button type="button" class="toggle" id="nav-refs">Find references</button></div>` : ""}
    <div class="nav-list">${list || '<p class="muted">Nothing found.</p>'}</div>`;
  panel.hidden = false;
  $("#nav-close").addEventListener("click", hideNavPanel);
  $("#nav-refs")?.addEventListener("click", () => runNavigation("references", query));
  for (const a of panel.querySelectorAll("a.nav-item-row")) {
    a.addEventListener("click", () => {
      for (const x of panel.querySelectorAll(".nav-item-row.current")) x.classList.remove("current");
      a.classList.add("current");
    });
  }
}

function hideNavPanel() {
  $("#nav-panel").hidden = true;
}

// ---- toast ----

let toastTimer = 0;
function toast(message, { busy = false, error = false, sticky = false } = {}) {
  const el = $("#toast");
  el.innerHTML = `${busy ? '<span class="spinner"></span> ' : ""}${esc(message)}`;
  el.classList.toggle("error", error);
  el.hidden = false;
  clearTimeout(toastTimer);
  if (!sticky) toastTimer = setTimeout(hideToast, error ? 5000 : 2500);
}
function hideToast() {
  $("#toast").hidden = true;
}

// ---- viewers for files outside the diff ----

function loadSource(path, side) {
  const key = `${side}:${path}`;
  if (!state.fileDiffs.has(key)) {
    const url = `/api/report/${state.report.id}/source?side=${side}&path=${encodeURIComponent(path)}`;
    state.fileDiffs.set(key, api(url).catch((e) => { state.fileDiffs.delete(key); throw e; }));
  }
  return state.fileDiffs.get(key);
}

// A file with no diff (unchanged, or a library): always syntax-highlighted. `lang` names the
// file for language detection when `path` (which also enables navigation) isn't given.
function plainRows(lines, { path = null, side = null, lang = path } = {}) {
  const rows = lines.map((text) => ({ t: " ", text }));
  const sx = syntaxSpans(rows, lang);
  return rows.map((ln, i) => `<tr class="ctx"${side ? anchorAttrs(side === "o" ? i + 1 : null, side === "n" ? i + 1 : null) : ""}>
    <td class="no">${i + 1}</td><td class="text"${navAttrs(path, side, i + 1)}>${codeHtml(ln, sx)}</td></tr>`).join("");
}

function showTarget(content, side, line) {
  const target = line && nearestRow(content, side, line);
  if (target) {
    target.classList.add("target");
    target.scrollIntoView({ block: "center" });
  } else {
    window.scrollTo({ top: 0 });
  }
}

// A repository file the diff doesn't touch (reached via navigation): same at base and head
// unless it only exists on one side.
async function renderBrowseView(path, mode, line) {
  const content = $("#content");
  const hash = location.hash;
  const side = mode === "old" ? "old" : "new";
  content.innerHTML = `<div class="empty"><span class="spinner"></span> Loading ${esc(path)}…</div>`;
  let src = null, error = null;
  try { src = await loadSource(path, side); } catch (e) { error = e.message; }
  if (location.hash !== hash) return;
  const tok = line || "";
  const tabs = `<span class="tab disabled" title="This file has no changes in this diff">Diff</span>
    <a class="tab" href="${fileHref(path, "old", tok)}" ${side === "old" ? 'aria-current="page"' : ""}>Original</a>
    <a class="tab" href="${fileHref(path, "new", tok)}" ${side === "new" ? 'aria-current="page"' : ""}>New</a>`;
  const s = side === "old" ? "o" : "n";
  content.innerHTML = `
    <div class="page-head viewer-head">
      <p>${side === "old" ? "Original" : "New"}${src ? ` · ${plural(src.lines.length, "line")}` : ""} · unchanged in this diff</p>
    </div>
    <div class="viewer-bar">${backButton()}<nav class="tabs" aria-label="File version">${tabs}</nav></div>
    ${navHint()}
    ${error ? `<div class="empty"><p>${esc(error)}</p></div>`
      : `<section class="file viewer"><table class="diff">${plainRows(src.lines, { path, side: s })}</table></section>`}`;
  $("#back-btn").addEventListener("click", () => history.back());
  showTarget(content, s, Number(tok.slice(1)));
}

async function renderLibraryView(path, line) {
  const content = $("#content");
  const hash = location.hash;
  content.innerHTML = `<div class="empty"><span class="spinner"></span> Loading ${esc(shortPath(path))}…</div>`;
  let src;
  try {
    src = await api(`/api/library?path=${encodeURIComponent(path)}`);
  } catch (e) {
    showError(e.message);
    return;
  }
  if (location.hash !== hash) return;
  content.innerHTML = `
    <div class="page-head viewer-head">
      <p><span class="lib-badge">library</span> ${esc(path)} · read-only; navigation isn't available in library files</p>
    </div>
    <div class="viewer-bar">${backButton()}</div>
    <section class="file viewer"><table class="diff">${plainRows(src.lines, { side: "n", lang: path })}</table></section>`;
  $("#back-btn").addEventListener("click", () => history.back());
  showTarget(content, "n", line);
}

// The command strip and the stream toolbar wrap onto more lines in narrower windows; the
// sticky panes below them need their actual heights rather than a fixed guess.
function trackTopbarHeight() {
  const bar = $("#topbar"), toolbar = $("#toolbar"), status = $("#status");
  const update = () => {
    const sticky = getComputedStyle(bar).position === "sticky";
    const top = sticky ? bar.offsetHeight : 0;
    const tool = toolbar.hidden ? 0 : toolbar.offsetHeight;
    const root = document.documentElement.style;
    root.setProperty("--topbar-h", `${top}px`);
    root.setProperty("--toolbar-h", `${tool}px`);
    root.setProperty("--stick-h", `${top + tool}px`);
    root.setProperty("--status-h", `${status.hidden ? 0 : status.offsetHeight}px`);
  };
  const ro = new ResizeObserver(update);
  ro.observe(bar);
  ro.observe(toolbar);
  ro.observe(status);
  window.addEventListener("resize", update);
  update();
}

// ---------- boot ----------

async function init() {
  for (const b of document.querySelectorAll(".segmented button")) {
    b.addEventListener("click", () => setMode(b.dataset.mode));
  }
  $("#source-form").addEventListener("submit", runAnalysis);
  $("#help-btn").addEventListener("click", () => showHelp(true));
  for (const b of document.querySelectorAll(".pane-toggle")) {
    b.addEventListener("click", () => setPane(b.dataset.pane, !state.panes[b.dataset.pane]));
  }
  for (const b of document.querySelectorAll("#explorer-tabs button")) {
    b.addEventListener("click", () => { state.explorerTab = b.dataset.tab; renderExplorer(); });
  }
  $("#explorer-list").addEventListener("click", onExplorerClick);
  $("#inspector").addEventListener("click", onInspectorClick);
  $("#content").addEventListener("click", onContentClick);
  $("#content").addEventListener("change", onContentChange);
  $("#content").addEventListener("mousedown", (e) => {
    const el = e.target.closest(".hunk, .occurrence");
    if (!el) return;
    const i = focusables().indexOf(el);
    if (i >= 0 && i !== state.focus) { state.focus = i; applyFocus(false); }
  });
  $("#content").addEventListener("click", onCodeClick, true);
  $("#content").addEventListener("mousedown", onCodeMouseDown);
  $("#content").addEventListener("mousemove", onCodeMouseMove);
  document.addEventListener("keyup", (e) => {
    if (e.key === "Meta" || e.key === "Control") { document.body.classList.remove("nav-armed"); setHoverWord(null); }
  });
  loadLayout();
  loadSyntax();
  loadPanes();
  trackTopbarHeight();
  narrowQuery.addEventListener("change", () => { if (state.report) rerender(); });
  window.addEventListener("hashchange", route);

  let defaults = {};
  try {
    const cfg = await api("/api/config");
    $("#repo").textContent = cfg.repo;
    $("#repo").title = cfg.repo;
    defaults = cfg.defaults || {};
    state.repo = cfg.repo;
    state.editor = defaults.editor || null;
  } catch {}
  setMode(defaults.mode || "refs");
  const form = $("#source-form");
  if (defaults.base) form.base.value = defaults.base;
  if (defaults.head) form.head.value = defaults.head;
  if (defaults.pr) form.pr.value = defaults.pr;
  loadFilters(defaults.filters);
  await loadSources(defaults);
  if (defaults.mode) runAnalysis();
}

init();
