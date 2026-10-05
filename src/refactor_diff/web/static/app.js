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
  filters: { hidden: new Set(), hideDocs: false, exclude: [] },
  view: null, // the report as filtered by state.filters; see applyFilters()
  fileDiffs: new Map(), // path -> Promise of the whole-file diff (see /api/report/{id}/file)
};

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
  return f.hidden.size > 0 || f.hideDocs || f.exclude.length > 0;
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
  const unitVisible = (u) => fileVisible(u.path) && !(f.hideDocs && docsOnly(u));

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
    stats: {
      files_changed: files.length,
      files_analyzed: files.filter((x) => x.analyzed).length,
      units: units.size,
      residual_units: units.size - explained,
      collapsed_pct: units.size ? Math.round((100 * explained) / units.size) : 0,
      mechanical_groups: groups.filter((g) => g.mechanical).length,
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
    <label class="exclude">
      <span class="filter-label">Exclude</span>
      <input id="exclude-input" type="text" spellcheck="false" value="${esc(f.exclude.join(", "))}"
        placeholder="globs, e.g. migrations, *_pb2.py, src/legacy/**">
    </label>
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
  $("#docs-toggle").addEventListener("click", () => {
    f.hideDocs = !f.hideDocs;
    saveFilters();
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
    state.filters = { hidden: new Set(), hideDocs: false, exclude: [] };
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

function unitTags(unit) {
  return unit.signatures.map((key) => {
    const g = state.groupsByKey.get(key);
    if (!g || !state.view.groupsById.has(g.id)) return "";
    if (g.mechanical) {
      return `<a class="tag" href="#group/${g.id}">${esc(g.kind)}: <span class="code">${esc(shortLabel(g.label))}</span></a>`;
    }
    return `<span class="tag unique">unique ${esc(g.kind)}: <span class="code">${esc(shortLabel(g.label))}</span></span>`;
  }).join("");
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
    <p>Changes that don't belong to a repeated pattern. Lines already explained by a pattern are dimmed.</p></div>`;
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
  return diffRows(h.lines.map((ln) => ({
    t: ln.type, o: ln.old_no, n: ln.new_no, text: ln.text, unit: ln.unit, hl: ln.hl,
  })), { tags: true });
}

// Rows for a list of diff lines ({t, o, n, text, unit, hl}). Lines of units that a pattern
// explains (or that filters hide) are dimmed, except the units in `focus`.
function diffRows(lines, { tags = false, focus = null } = {}) {
  const r = state.report;
  const tagged = new Set();
  let rows = "";
  for (const ln of lines) {
    const unit = ln.unit ? r.units[ln.unit] : null;
    const hidden = unit && !state.view.units.has(unit.id);
    const focused = unit && focus && focus.has(unit.id);
    if (tags && unit && !hidden && !focused && !tagged.has(unit.id)) {
      tagged.add(unit.id);
      const t = unitTags(unit);
      if (t) rows += `<tr class="tags"><td></td><td></td><td></td><td>${t}</td></tr>`;
    }
    const cls = ln.t === "-" ? "del" : ln.t === "+" ? "add" : "ctx";
    const dim = unit && !focused && (unit.explained || hidden) ? " explained" : "";
    rows += `<tr class="${cls}${dim}${focused ? " focus" : ""}">
      <td class="no">${ln.o ?? ""}</td><td class="no">${ln.n ?? ""}</td>
      <td class="sign">${ln.t === " " ? "" : ln.t}</td>
      <td class="text">${highlight(ln.text, ln.hl)}</td></tr>`;
  }
  return rows;
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
  rows += diffRows(fd.lines.slice(lo, hi), { tags: !focus.size, focus });
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
  return diffRows([
    ...u.old.map((ln, k) => ({ t: "-", o: u.old_start + k, n: null, text: ln.text, unit: u.id, hl: ln.hl })),
    ...u.new.map((ln, k) => ({ t: "+", o: null, n: u.new_start + k, text: ln.text, unit: u.id, hl: ln.hl })),
  ], { focus: new Set([u.id]) });
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

  const transform = g.kind === "formatting" || g.kind === "docs"
    ? `<span class="transform">${esc(g.label)}</span>`
    : `<span class="transform"><span class="old">${esc(g.old || "∅")}</span> → <span class="new">${esc(g.new || "∅")}</span></span>`;
  const isDone = state.reviewed.has(g.id);
  const warnCount = state.view.warnings.filter((w) => w.group_id === g.id).length;
  const files = new Set(g.visible.map((uid) => r.units[uid].path));
  const hiddenHere = g.unit_ids.length - g.visible.length;
  let html = `<div class="page-head">
      <h2><span class="kind ${g.kind}">${g.kind}</span>${transform}</h2>
      <span class="spacer"></span>
      <label class="toggle"><input type="checkbox" id="group-reviewed" ${isDone ? "checked" : ""}> Reviewed</label>
      <p>${plural(g.visible.length, "occurrence")} in ${plural(files.size, "file")}${hiddenHere ? ` (${hiddenHere} hidden by filters)` : ""}${warnCount ? ` · <a href="#warnings">${plural(warnCount, "warning")}</a>` : ""}</p>
    </div>`;
  const details = Object.entries(g.details);
  if (details.length) {
    html += `<div class="chips">${details.map(([d, n]) => `<span class="chip">${esc(d)} <b>×${n}</b></span>`).join("")}</div>`;
  }

  const units = g.visible.map((uid) => r.units[uid]);
  const byFile = new Map();
  for (const u of units.slice(0, state.shown)) {
    if (!byFile.has(u.path)) byFile.set(u.path, []);
    byFile.get(u.path).push(u);
  }
  for (const [path, us] of byFile) {
    html += `<section class="file"><header><span class="path">${esc(path)}</span>
      <span class="meta">×${us.length}</span>${fileLinks(path)}</header>`;
    for (const u of us) {
      const where = { oldLine: u.old.length ? u.old_start : null, newLine: u.new.length ? u.new_start : null };
      html += `<div class="occurrence" data-unit="${u.id}"><table class="diff">${unitRows(u)}</table>
        <div class="occ-actions">
          ${u.explained ? "" : `<a class="badge-link" href="#review">also has other changes — see Needs review</a>`}
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
      <p>No references to renamed definitions are left behind, and no symbol was renamed two different ways.</p></div>`;
    return;
  }
  let html = `<div class="page-head"><h2>Warnings</h2>
    <p>Possible problems with the refactor: references to a renamed definition that no longer exists, or a symbol renamed two different ways.</p></div>`;
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

  let body = "";
  let prevChanged = false;
  for (const ln of rows) {
    const changed = ln.t !== " ";
    const num = mode === "old" ? ln.o : mode === "new" ? ln.n : null;
    const cls = ln.t === "-" ? "del" : ln.t === "+" ? "add" : "ctx";
    const unit = ln.unit ? state.report.units[ln.unit] : null;
    const dim = mode === "diff" && unit && (unit.explained || !state.view.units.has(unit.id)) ? " explained" : "";
    const anchor = `${ln.o ? ` data-o="${ln.o}"` : ""}${ln.n ? ` data-n="${ln.n}"` : ""}`;
    body += `<tr class="${cls}${dim}${changed && !prevChanged ? " chg-start" : ""}"${anchor}>
      ${mode === "diff" ? `<td class="no">${ln.o ?? ""}</td><td class="no">${ln.n ?? ""}</td>` : `<td class="no">${num}</td>`}
      <td class="sign">${ln.t === " " ? "" : ln.t}</td>
      <td class="text">${highlight(ln.text, ln.hl)}</td></tr>`;
    prevChanged = changed;
  }
  const empty = !rows.length
    ? `<div class="empty"><p>${mode === "old" ? "This file was added; there is no original version."
      : mode === "new" ? "This file was deleted; there is no new version." : "Empty file."}</p></div>` : "";

  content.innerHTML = `
    <div class="page-head viewer-head">
      <h2 class="code">${fd.old_path ? esc(fd.old_path) + " → " : ""}${esc(path)}</h2>
      <span class="spacer"></span>
      <button type="button" class="toggle" id="back-btn">← Back</button>
      <p>${title} · ${counts} · <span class="cat">${esc(fd.category)}</span></p>
    </div>
    <div class="viewer-bar">
      <nav class="tabs" aria-label="File version">${tabs}</nav>
      <span class="spacer"></span>
      <button type="button" class="toggle" data-jump="prev" title="Previous change (p)">↑ Prev change</button>
      <button type="button" class="toggle" data-jump="next" title="Next change (n)">↓ Next change</button>
    </div>
    ${empty || `<section class="file viewer"><table class="diff">${body}</table></section>`}`;

  $("#back-btn").addEventListener("click", () => history.back());
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

// ---------- boot ----------

async function init() {
  for (const b of document.querySelectorAll(".segmented button")) {
    b.addEventListener("click", () => setMode(b.dataset.mode));
  }
  $("#source-form").addEventListener("submit", runAnalysis);
  $("#content").addEventListener("click", onContentClick);
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
