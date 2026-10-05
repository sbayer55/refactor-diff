"use strict";

const $ = (sel, root = document) => root.querySelector(sel);
const PAGE = 300; // occurrences rendered before "show more"

const state = {
  mode: "refs",
  sources: null,
  report: null,
  groupsByKey: new Map(),
  reviewed: new Set(),
  shown: PAGE,
  filters: { hidden: new Set(), hideDocs: false, exclude: [], nearOnly: false },
  view: null, // the report as filtered by state.filters; see applyFilters()
  fileDiffs: new Map(), // path -> Promise of the whole-file diff (see /api/report/{id}/file)
  split: false, // side-by-side diffs (preference; see splitActive())
  syntax: false, // syntax-color changed lines too (unchanged lines always are)
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

// Reviewed marks are per-viewer conveniences, so localStorage is fine (and may be unavailable).
function storageKey() {
  const s = state.report.source;
  return `refactor-diff:reviewed:${s.base_sha}..${s.head_sha || "worktree"}`;
}
function loadReviewed() {
  try {
    state.reviewed = new Set(JSON.parse(localStorage.getItem(storageKey()) || "[]"));
  } catch { state.reviewed = new Set(); }
}
function saveReviewed() {
  try { localStorage.setItem(storageKey(), JSON.stringify([...state.reviewed])); } catch {}
}
function toggleReviewed(id, on) {
  if (on) state.reviewed.add(id); else state.reviewed.delete(id);
  saveReviewed();
  renderSidebar();
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
    exclude: f.exclude || [],
    nearOnly: false,
  };
}
function saveFilters() {
  const f = state.filters;
  try {
    localStorage.setItem(filtersKey(), JSON.stringify({
      hidden: [...f.hidden], hideDocs: f.hideDocs, exclude: f.exclude,
    }));
  } catch {}
}
function filtersActive() {
  const f = state.filters;
  return f.hidden.size > 0 || f.hideDocs || f.exclude.length > 0 || f.nearOnly;
}

function applyFilters() {
  const r = state.report;
  const f = state.filters;
  const excludes = f.exclude.map(globToRegex);
  const fileOf = new Map(r.files.map((x) => [x.path, x]));
  const fileVisible = (path) => {
    const file = fileOf.get(path);
    if (file && f.hidden.has(file.category)) return false;
    return !excludes.some((re) => re.test(path));
  };
  const docsOnly = (u) => u.signatures.length > 0 && u.signatures.every((k) => k === "docs");
  const unitVisible = (u) => fileVisible(u.path) && !(f.hideDocs && docsOnly(u))
    && !(f.nearOnly && !u.explained && !(u.near && u.near.length));

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
    },
  };
}

function rerender() {
  applyFilters();
  renderSummary();
  renderFilters();
  route({ keepScroll: true });
}

function renderFilters() {
  const r = state.report;
  const f = state.filters;
  const el = $("#filters");
  el.hidden = false;
  const counts = {};
  for (const file of r.files) counts[file.category] = (counts[file.category] || 0) + 1;
  const chips = CATEGORIES.filter(([cat]) => counts[cat]).map(([cat, label]) => `
    <button type="button" class="filter-chip" data-cat="${cat}" aria-pressed="${!f.hidden.has(cat)}">
      ${label} <span class="n">${counts[cat]}</span></button>`).join("");
  const docsTotal = Object.values(r.units)
    .filter((u) => u.signatures.length && u.signatures.every((k) => k === "docs")).length;
  const v = state.view;
  el.innerHTML = `
    <span class="filter-label">Show files</span>
    <div class="chip-row">${chips}</div>
    <button type="button" class="filter-chip" id="docs-toggle" aria-pressed="${!f.hideDocs}"
      title="Changes that only touch comments or docstrings" ${docsTotal ? "" : "disabled"}>
      Comment &amp; docstring edits <span class="n">${docsTotal}</span></button>
    ${v.nearUnits ? `<button type="button" class="filter-chip mode" id="near-toggle" aria-pressed="${f.nearOnly}"
      title="Only leftover changes that almost match a mechanical pattern (likely typos)">
      Only near misses <span class="n">${v.nearUnits}</span></button>` : ""}
    <label class="exclude">
      <span class="filter-label">Exclude</span>
      <input id="exclude-input" type="text" spellcheck="false" value="${esc(f.exclude.join(", "))}"
        placeholder="globs, e.g. migrations, *_pb2.py, src/legacy/**">
    </label>
    <span class="spacer"></span>
    ${highlightToggle()}
    ${layoutToggle()}
    ${filtersActive() ? `<span class="hidden-note">${plural(v.hiddenUnits, "change")} hidden</span>
      <button type="button" class="link" id="reset-filters">Reset</button>` : ""}`;

  for (const b of el.querySelectorAll("[data-cat]")) {
    b.addEventListener("click", () => {
      const cat = b.dataset.cat;
      if (f.hidden.has(cat)) f.hidden.delete(cat); else f.hidden.add(cat);
      saveFilters();
      rerender();
    });
  }
  bindLayoutToggle(el);
  $("#docs-toggle").addEventListener("click", () => {
    f.hideDocs = !f.hideDocs;
    saveFilters();
    rerender();
  });
  $("#near-toggle")?.addEventListener("click", () => {
    f.nearOnly = !f.nearOnly;
    rerender();
  });
  const input = $("#exclude-input");
  input.addEventListener("change", () => {
    f.exclude = input.value.split(",").map((s) => s.trim()).filter(Boolean);
    saveFilters();
    rerender();
  });
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") input.blur(); });
  $("#reset-filters")?.addEventListener("click", () => {
    state.filters = { hidden: new Set(), hideDocs: false, exclude: [], nearOnly: false };
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
  try {
    const report = await api("/api/analyze", body);
    state.report = report;
    state.groupsByKey = new Map(report.groups.map((g) => [g.key, g]));
    state.shown = PAGE;
    state.fileDiffs = new Map();
    loadReviewed();
    applyFilters();
    renderSummary();
    renderFilters();
    if (!location.hash || location.hash === "#") location.hash = "#review";
    else route();
  } catch (e) {
    showError(e.message);
  } finally {
    btn.disabled = false;
    btn.textContent = "Analyze";
  }
}

function showError(msg) {
  $("#content").innerHTML = `<div class="error">${esc(msg)}</div>`;
}

// ---------- summary + sidebar ----------

function renderSummary() {
  const { source } = state.report;
  const { stats } = state.view;
  const el = $("#summary");
  el.hidden = false;
  const sha = (s) => (s ? s.slice(0, 8) : "working tree");
  el.innerHTML = `
    <div class="metric">
      <span class="big">${stats.collapsed_pct}%</span>
      <span>of changed lines collapsed</span>
      <div class="meter"><div style="width:${stats.collapsed_pct}%"></div></div>
      ${stats.verified_units ? `<span class="sub" title="${esc(VERIFIED_TITLE)}">✓ ${plural(stats.verified_units, "change")} verified by AST</span>` : ""}
    </div>
    <div class="metric"><span class="big">${stats.residual_units}</span>
      <span>${stats.residual_units === 1 ? "change" : "changes"} to review</span></div>
    <div class="metric"><span class="big">${stats.mechanical_groups}</span>
      <span>mechanical ${stats.mechanical_groups === 1 ? "pattern" : "patterns"}</span></div>
    <div class="metric"><span class="big">${stats.files_analyzed}<span style="font-size:16px;color:var(--muted)">/${stats.files_changed}</span></span>
      <span>files analyzed</span></div>
    <div class="source">
      <div><strong>${esc(source.label)}</strong></div>
      <div class="code">${sha(source.base_sha)} → ${sha(source.head_sha)}</div>
    </div>`;
}

function renderSidebar() {
  const r = state.report;
  if (!r) return;
  const v = state.view;
  const mech = v.groups.filter((g) => g.mechanical);
  const done = mech.filter((g) => state.reviewed.has(g.id)).length;
  const route = location.hash;
  const active = (h) => (route === h ? " active" : "");
  const skipped = v.files.filter((f) => !f.analyzed).length;

  let html = `
    <div class="nav-section">
      <a class="nav-item${active("#review")}" href="#review">
        <span class="label"><strong>Needs review</strong></span>
        <span class="pill ${v.stats.residual_units ? "attention" : "ok"}">${v.stats.residual_units}</span>
      </a>
      <a class="nav-item${active("#warnings")}" href="#warnings">
        <span class="label">Warnings</span>
        <span class="pill ${v.warnings.length ? "warn" : ""}">${v.warnings.length}</span>
      </a>
      <a class="nav-item${active("#files")}" href="#files">
        <span class="label">Files</span>
        <span class="count">${v.files.length}${skipped ? ` · ${skipped} not analyzed` : ""}</span>
      </a>
    </div>
    <div class="nav-section">
      <h4><span>Mechanical patterns</span><span>${done}/${mech.length} reviewed</span></h4>`;
  if (!mech.length) {
    html += `<div class="nav-item"><span class="label" style="color:var(--muted)">${
      filtersActive() ? "No patterns in the visible files" : "No repeated patterns found"}</span></div>`;
  }
  for (const g of mech) {
    const isDone = state.reviewed.has(g.id);
    html += `
      <a class="nav-item${active("#group/" + g.id)}${isDone ? " done" : ""}" href="#group/${g.id}" title="${esc(g.label)}">
        <input type="checkbox" data-review="${g.id}" ${isDone ? "checked" : ""} aria-label="Mark reviewed">
        <span class="kind ${g.kind}">${g.kind}</span>
        <span class="label code">${esc(g.label)}</span>
        <span class="count">×${g.visible.length}</span>
      </a>`;
  }
  html += "</div>";
  const sb = $("#sidebar");
  sb.innerHTML = html;
  for (const cb of sb.querySelectorAll("[data-review]")) {
    cb.addEventListener("click", (e) => {
      e.stopPropagation();
      toggleReviewed(cb.dataset.review, cb.checked);
      if (location.hash === "#group/" + cb.dataset.review) route();
    });
  }
}

// ---------- views ----------

function route(opts) {
  if (!state.report) return;
  const [view, id, ...rest] = location.hash.replace(/^#/, "").split("/");
  renderSidebar();
  if (view === "file") {
    renderFileView(decodeURIComponent(id || ""), rest[0] || "diff", rest[1] || "");
    return;
  }
  if (view === "lib") {
    renderLibraryView(decodeURIComponent(id || ""), Number(rest[0]) || 0);
    return;
  }
  if (view === "group") renderGroup(id);
  else if (view === "warnings") renderWarnings();
  else if (view === "files") renderFiles();
  else renderReview();
  if (!opts?.keepScroll) window.scrollTo({ top: 0 });
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
    content.innerHTML = `<div class="empty"><h2>Nothing left to review</h2>
      <p>Every changed line in the ${filtersActive() ? "visible" : "analyzed"} files matched a
      mechanical pattern. Skim the patterns in the sidebar and check the warnings.</p></div>`;
    return;
  }
  const byFile = new Map();
  for (const hid of v.residualHunks) {
    const h = r.hunks[hid];
    if (!byFile.has(h.path)) byFile.set(h.path, []);
    byFile.get(h.path).push(h);
  }
  let html = `<div class="page-head"><h2>Needs review</h2>
    <p>Changes that don't belong to a repeated pattern. Lines already explained by a pattern are dimmed.</p></div>
    ${navHint()}`;
  for (const [path, hunks] of byFile) {
    const residual = hunks.reduce((n, h) =>
      n + h.unit_ids.filter((u) => v.units.has(u) && !r.units[u].explained).length, 0);
    html += `<section class="file"><header><span class="path">${esc(path)}</span>
      <span class="meta">${plural(residual, "change")}</span>${fileLinks(path)}</header>`;
    for (const h of hunks) {
      html += `<div class="hunk" data-hunk="${h.id}">
        <div class="hunk-bar"><span>@@ line ${h.new_start} @@</span>
          <button type="button" class="link" data-hunk-context>Show context</button></div>
        <table class="diff">${hunkRows(h)}</table></div>`;
    }
    html += "</section>";
  }
  content.innerHTML = html;
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
  return `<span class="file-links">${links.join("")}</span>`;
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

  const transform = g.kind === "formatting" || g.kind === "docs" || g.kind === "move"
    ? `<span class="transform">${esc(g.label)}</span>`
    : `<span class="transform"><span class="old">${esc(g.old || "∅")}</span> → <span class="new">${esc(g.new || "∅")}</span></span>`;
  const isDone = state.reviewed.has(g.id);
  const warnCount = state.view.warnings.filter((w) => w.group_id === g.id).length;
  const files = new Set(g.visible.map((uid) => r.units[uid].path));
  const hiddenHere = g.unit_ids.length - g.visible.length;
  let units = g.visible.map((uid) => r.units[uid]);
  const verifiedCount = units.filter((u) => u.verified).length;
  // A move is one occurrence shown as old block → new block; its deleted half isn't listed.
  const moved = g.kind === "move" ? units.find((u) => u.new.length && u.partner) : null;
  if (moved) units = units.filter((u) => u.id !== moved.partner);
  const count = g.kind === "move" ? `moved block${units.length > 1 ? ` + ${plural(units.length - 1, "import edit")}` : ""}`
    : `${plural(g.visible.length, "occurrence")} in ${plural(files.size, "file")}`;
  let html = `<div class="page-head">
      <h2><span class="kind ${g.kind}">${g.kind}</span>${transform}</h2>
      <span class="spacer"></span>
      <label class="toggle"><input type="checkbox" id="group-reviewed" ${isDone ? "checked" : ""}> Reviewed</label>
      <p>${count}${hiddenHere ? ` (${hiddenHere} hidden by filters)` : ""}${
        verifiedCount ? ` · <span class="tag verified" title="${esc(VERIFIED_TITLE)}">✓ ${g.kind === "move" ? "verified" : `${verifiedCount} of ${g.visible.length} verified`}</span>` : ""}${
        warnCount ? ` · <a href="#warnings">${plural(warnCount, "warning")}</a>` : ""}</p>
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
  let html = `<div class="page-head"><h2>Warnings</h2>
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
      locs += `<tr><td class="where"><a href="${fileHref(loc.path, "new", "n" + loc.line)}">${esc(loc.path)}:${loc.line}</a></td>
        <td class="text">${highlight(text, ranges)}</td></tr>`;
    }
    const unlisted = w.total - w.locations.length - (w.filteredOut || 0);
    const notes = [
      unlisted > 0 ? `…and ${unlisted} more` : "",
      w.filteredOut ? `${w.filteredOut} in files hidden by filters` : "",
    ].filter(Boolean).join(" · ");
    const more = notes ? `<div class="where">${notes}</div>` : "";
    html += `<div class="warning">
      <div>${esc(w.message)}${g && v.groupsById.has(g.id) ? ` · <a href="#group/${g.id}">view pattern</a>` : ""}</div>
      ${locs ? `<table class="locations">${locs}</table>${more}` : ""}</div>`;
  }
  content.innerHTML = html;
}

function renderFiles() {
  const r = state.report;
  const label = { A: "added", M: "modified", D: "deleted", R: "renamed" };
  const v = state.view;
  let html = `<div class="page-head"><h2>Files</h2>
    <p>Only Python files are analyzed for now; other files are listed but not collapsed.
    Files hidden by filters are dimmed.</p></div>
    <table class="files"><thead><tr><th>Status</th><th>Path</th><th>Kind</th><th>+/−</th><th>To review</th><th>Analyzed</th></tr></thead><tbody>`;
  for (const f of r.files) {
    html += `<tr class="${v.fileVisible(f.path) ? "" : "filtered"}">
      <td><span class="status" title="${label[f.status] || f.status}">${esc(f.status)}</span></td>
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
      <h2 class="code">${fd.old_path ? esc(fd.old_path) + " → " : ""}${esc(path)}</h2>
      <span class="spacer"></span>
      <p>${title} · ${counts} · <span class="cat">${esc(fd.category)}</span></p>
    </div>
    <div class="viewer-bar">
      ${backButton()}
      <nav class="tabs" aria-label="File version">${tabs}</nav>
      ${highlightToggle()}
      ${mode === "diff" ? layoutToggle(path) : ""}
      <span class="spacer"></span>
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

document.addEventListener("keydown", (e) => {
  if (!location.hash.startsWith("#file/") || e.metaKey || e.ctrlKey || e.altKey) return;
  if (e.target.closest?.("input, textarea, select")) return;
  if (e.key === "n") jumpChange(1);
  if (e.key === "p") jumpChange(-1);
});

// ---------- code navigation (go to definition / find references) ----------

const IS_MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const MOD_KEY = IS_MAC ? "⌘" : "Ctrl";
const modDown = (e) => (IS_MAC ? e.metaKey : e.ctrlKey);
const ID_CHAR = /[\p{L}\p{N}_]/u;

function navHint() {
  return `<p class="nav-hint"><kbd>${MOD_KEY}</kbd>-click a name to go to its definition ·
    <kbd>${MOD_KEY}</kbd><kbd>⇧</kbd>-click to find references</p>`;
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
  const i = p.lastIndexOf("/site-packages/");
  if (i >= 0) return p.slice(i + "/site-packages/".length);
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
      <h2 class="code">${esc(path)}</h2>
      <span class="spacer"></span>
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
      <h2 class="code">${esc(shortPath(path))}</h2>
      <span class="spacer"></span>
      <p><span class="lib-badge">library</span> ${esc(path)} · read-only; navigation isn't available in library files</p>
    </div>
    <div class="viewer-bar">${backButton()}</div>
    <section class="file viewer"><table class="diff">${plainRows(src.lines, { side: "n", lang: path })}</table></section>`;
  $("#back-btn").addEventListener("click", () => history.back());
  showTarget(content, "n", line);
}

// The top bar wraps onto more lines in narrower windows; sticky elements below it need its
// actual height rather than a fixed guess.
function trackTopbarHeight() {
  const bar = $(".topbar");
  const update = () => {
    const sticky = getComputedStyle(bar).position === "sticky";
    document.documentElement.style.setProperty("--topbar-h", `${sticky ? bar.offsetHeight : 0}px`);
  };
  new ResizeObserver(update).observe(bar);
  window.addEventListener("resize", update);
  update();
}

// ---------- boot ----------

async function init() {
  for (const b of document.querySelectorAll(".segmented button")) {
    b.addEventListener("click", () => setMode(b.dataset.mode));
  }
  $("#source-form").addEventListener("submit", runAnalysis);
  $("#content").addEventListener("click", onContentClick);
  $("#content").addEventListener("click", onCodeClick, true);
  $("#content").addEventListener("mousedown", onCodeMouseDown);
  $("#content").addEventListener("mousemove", onCodeMouseMove);
  document.addEventListener("keyup", (e) => {
    if (e.key === "Meta" || e.key === "Control") { document.body.classList.remove("nav-armed"); setHoverWord(null); }
  });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") hideNavPanel(); });
  loadLayout();
  loadSyntax();
  trackTopbarHeight();
  narrowQuery.addEventListener("change", () => { if (state.report) rerender(); });
  window.addEventListener("hashchange", route);

  let defaults = {};
  try {
    const cfg = await api("/api/config");
    $("#repo").textContent = cfg.repo;
    $("#repo").title = cfg.repo;
    defaults = cfg.defaults || {};
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
