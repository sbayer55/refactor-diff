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
};

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
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  renderSidebar();
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
      <span class="meta">${plural(residual, "change")}</span></header><table class="diff">`;
    hunks.forEach((h, i) => {
      if (i > 0 || h.old_start > 1) html += `<tr class="sep"><td colspan="4">@@ line ${h.new_start} @@</td></tr>`;
      html += hunkRows(h);
    });
    html += "</table></section>";
  }
  content.innerHTML = html;
}

function hunkRows(h) {
  const r = state.report;
  const tagged = new Set();
  let rows = "";
  for (const ln of h.lines) {
    const unit = ln.unit ? r.units[ln.unit] : null;
    const hidden = unit && !state.view.units.has(unit.id);
    if (unit && !hidden && !tagged.has(unit.id)) {
      tagged.add(unit.id);
      const tags = unitTags(unit);
      if (tags) rows += `<tr class="tags"><td></td><td></td><td></td><td>${tags}</td></tr>`;
    }
    const cls = ln.type === "-" ? "del" : ln.type === "+" ? "add" : "ctx";
    const explained = unit && (unit.explained || hidden) ? " explained" : "";
    rows += `<tr class="${cls}${explained}">
      <td class="no">${ln.old_no ?? ""}</td><td class="no">${ln.new_no ?? ""}</td>
      <td class="sign">${ln.type === " " ? "" : ln.type}</td>
      <td class="text">${highlight(ln.text, ln.hl)}</td></tr>`;
  }
  return rows;
}

function unitRows(u) {
  let rows = "";
  u.old.forEach((ln, k) => {
    rows += `<tr class="del"><td class="no">${u.old_start + k}</td><td class="sign">-</td>
      <td class="text">${highlight(ln.text, ln.hl)}</td></tr>`;
  });
  u.new.forEach((ln, k) => {
    rows += `<tr class="add"><td class="no">${u.new_start + k}</td><td class="sign">+</td>
      <td class="text">${highlight(ln.text, ln.hl)}</td></tr>`;
  });
  return rows;
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
      <span class="meta">×${us.length}</span></header>`;
    for (const u of us) {
      html += `<div class="occurrence"><table class="diff">${unitRows(u)}</table>`;
      if (!u.explained) {
        html += `<div class="note"><a class="badge-link" href="#review">also has other changes — see Needs review</a></div>`;
      }
      html += "</div>";
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
      locs += `<tr><td class="where">${esc(loc.path)}:${loc.line}</td>
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
      <td class="path">${f.old_path ? esc(f.old_path) + " → " : ""}${esc(f.path)}</td>
      <td><span class="cat">${esc(f.category)}</span></td>
      <td class="num"><span class="adds">+${f.additions}</span> <span class="dels">−${f.deletions}</span></td>
      <td class="num">${f.analyzed ? `${f.residual_units} / ${f.units ?? ""}` : "—"}</td>
      <td>${f.analyzed ? (f.parse_ok ? "yes" : "partially (syntax error)") : "no"}</td></tr>`;
  }
  content.innerHTML = html + "</tbody></table>";
}

// ---------- boot ----------

async function init() {
  for (const b of document.querySelectorAll(".segmented button")) {
    b.addEventListener("click", () => setMode(b.dataset.mode));
  }
  $("#source-form").addEventListener("submit", runAnalysis);
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
