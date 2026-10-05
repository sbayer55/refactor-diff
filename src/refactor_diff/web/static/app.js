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
};

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
    renderSummary();
    renderSidebar();
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
  const { stats, source } = state.report;
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
  const mech = r.groups.filter((g) => g.mechanical);
  const done = mech.filter((g) => state.reviewed.has(g.id)).length;
  const route = location.hash;
  const active = (h) => (route === h ? " active" : "");
  const skipped = r.files.filter((f) => !f.analyzed).length;

  let html = `
    <div class="nav-section">
      <a class="nav-item${active("#review")}" href="#review">
        <span class="label"><strong>Needs review</strong></span>
        <span class="pill ${r.stats.residual_units ? "attention" : "ok"}">${r.stats.residual_units}</span>
      </a>
      <a class="nav-item${active("#warnings")}" href="#warnings">
        <span class="label">Warnings</span>
        <span class="pill ${r.warnings.length ? "warn" : ""}">${r.warnings.length}</span>
      </a>
      <a class="nav-item${active("#files")}" href="#files">
        <span class="label">Files</span>
        <span class="count">${r.files.length}${skipped ? ` · ${skipped} not analyzed` : ""}</span>
      </a>
    </div>
    <div class="nav-section">
      <h4><span>Mechanical patterns</span><span>${done}/${mech.length} reviewed</span></h4>`;
  if (!mech.length) html += `<div class="nav-item"><span class="label" style="color:var(--muted)">No repeated patterns found</span></div>`;
  for (const g of mech) {
    const isDone = state.reviewed.has(g.id);
    html += `
      <a class="nav-item${active("#group/" + g.id)}${isDone ? " done" : ""}" href="#group/${g.id}" title="${esc(g.label)}">
        <input type="checkbox" data-review="${g.id}" ${isDone ? "checked" : ""} aria-label="Mark reviewed">
        <span class="kind ${g.kind}">${g.kind}</span>
        <span class="label code">${esc(g.label)}</span>
        <span class="count">×${g.unit_ids.length}</span>
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

function route() {
  if (!state.report) return;
  const [view, id] = location.hash.replace(/^#/, "").split("/");
  renderSidebar();
  if (view === "group") renderGroup(id);
  else if (view === "warnings") renderWarnings();
  else if (view === "files") renderFiles();
  else renderReview();
  window.scrollTo({ top: 0 });
}

function unitTags(unit) {
  return unit.signatures.map((key) => {
    const g = state.groupsByKey.get(key);
    if (!g) return "";
    if (g.mechanical) {
      return `<a class="tag" href="#group/${g.id}">${esc(g.kind)}: <span class="code">${esc(g.label)}</span></a>`;
    }
    return `<span class="tag unique">unique ${esc(g.kind)}: <span class="code">${esc(g.label)}</span></span>`;
  }).join("");
}

function renderReview() {
  const r = state.report;
  const content = $("#content");
  if (!r.residual_hunk_ids.length) {
    content.innerHTML = `<div class="empty"><h2>Nothing left to review</h2>
      <p>Every changed line in the analyzed files matched a mechanical pattern. Skim the
      patterns in the sidebar and check the warnings.</p></div>`;
    return;
  }
  const byFile = new Map();
  for (const hid of r.residual_hunk_ids) {
    const h = r.hunks[hid];
    if (!byFile.has(h.path)) byFile.set(h.path, []);
    byFile.get(h.path).push(h);
  }
  let html = `<div class="page-head"><h2>Needs review</h2>
    <p>Changes that don't belong to a repeated pattern. Lines already explained by a pattern are dimmed.</p></div>`;
  for (const [path, hunks] of byFile) {
    const residual = hunks.reduce((n, h) => n + h.unit_ids.filter((u) => !r.units[u].explained).length, 0);
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
    if (unit && !tagged.has(unit.id)) {
      tagged.add(unit.id);
      const tags = unitTags(unit);
      if (tags) rows += `<tr class="tags"><td></td><td></td><td></td><td>${tags}</td></tr>`;
    }
    const cls = ln.type === "-" ? "del" : ln.type === "+" ? "add" : "ctx";
    const explained = unit && unit.explained ? " explained" : "";
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
  const g = r.groups.find((x) => x.id === id);
  const content = $("#content");
  if (!g) { content.innerHTML = `<div class="empty"><h2>Pattern not found</h2></div>`; return; }

  const transform = g.kind === "formatting"
    ? `<span class="transform">${esc(g.label)}</span>`
    : `<span class="transform"><span class="old">${esc(g.old || "∅")}</span> → <span class="new">${esc(g.new || "∅")}</span></span>`;
  const isDone = state.reviewed.has(g.id);
  const warnCount = r.warnings.filter((w) => w.group_id === g.id).length;
  let html = `<div class="page-head">
      <h2><span class="kind ${g.kind}">${g.kind}</span>${transform}</h2>
      <span class="spacer"></span>
      <label class="toggle"><input type="checkbox" id="group-reviewed" ${isDone ? "checked" : ""}> Reviewed</label>
      <p>${plural(g.unit_ids.length, "occurrence")} in ${plural(g.files.length, "file")}${warnCount ? ` · <a href="#warnings">${plural(warnCount, "warning")}</a>` : ""}</p>
    </div>`;
  const details = Object.entries(g.details);
  if (details.length) {
    html += `<div class="chips">${details.map(([d, n]) => `<span class="chip">${esc(d)} <b>×${n}</b></span>`).join("")}</div>`;
  }

  const units = g.unit_ids.map((uid) => r.units[uid]);
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
  const content = $("#content");
  if (!r.warnings.length) {
    content.innerHTML = `<div class="empty"><h2>No warnings</h2>
      <p>No leftover old names after renames, and no identifier was renamed two different ways.</p></div>`;
    return;
  }
  let html = `<div class="page-head"><h2>Warnings</h2>
    <p>Possible problems with the refactor: old names that still appear, or inconsistent renames.</p></div>`;
  for (const w of r.warnings) {
    const g = r.groups.find((x) => x.id === w.group_id);
    let code = "";
    if (w.text != null && g) {
      const re = new RegExp(`\\b${g.old.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "g");
      const ranges = [...w.text.matchAll(re)].map((m) => [m.index, m.index + m[0].length]);
      code = `<pre>${highlight(w.text, ranges)}</pre>`;
    }
    html += `<div class="warning">
      <div>${esc(w.message)}${g ? ` · <a href="#group/${g.id}">view pattern</a>` : ""}</div>
      ${w.path ? `<div class="where">${esc(w.path)}${w.line ? ":" + w.line : ""}</div>` : ""}
      ${code}</div>`;
  }
  content.innerHTML = html;
}

function renderFiles() {
  const r = state.report;
  const label = { A: "added", M: "modified", D: "deleted", R: "renamed" };
  let html = `<div class="page-head"><h2>Files</h2>
    <p>Only Python files are analyzed for now; other files are listed but not collapsed.</p></div>
    <table class="files"><thead><tr><th>Status</th><th>Path</th><th>+/−</th><th>To review</th><th>Analyzed</th></tr></thead><tbody>`;
  for (const f of r.files) {
    html += `<tr>
      <td><span class="status" title="${label[f.status] || f.status}">${esc(f.status)}</span></td>
      <td class="path">${f.old_path ? esc(f.old_path) + " → " : ""}${esc(f.path)}</td>
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
  await loadSources(defaults);
  if (defaults.mode) runAnalysis();
}

init();
