// ---------- concept lens ----------
// On a "remove concept X" change most leftover units only add or remove X; the real review is
// the collateral around it. The engine finds the concept (report.concepts, strongest first:
// auto-detected stems, or the one asked for with --concept / the picker here) and lists the
// units it fully explains, the units that mention it but still differ, its patterns by role,
// and per file how many changed lines are concept vs collateral.
//
// This module is an overlay on the report: it never changes `unit.explained`, the stats or the
// warnings. It adds a picker under the explorer, a concept row on top of the Patterns tab, the
// #concept/<stem> page, the "concept-only edits" filter (CHANGE_FILTERS in app.js), tags on
// units, a collateral count in the status bar, a concept/collateral heat bar per file, and the
// inspector's Cross-references section (code issues from /api/report/{id}/xref, --issue-*
// colors only, never counted as warnings).
//
// Runs after app.js and uses its helpers (state, esc, api, prefGet/prefSet, plural, rerender,
// renderInspector, renderExplorer, runAnalysis, fileHref, kindChip, toggleFilter, setPane,
// basename, ISSUE_ICONS, KEY_HELP, toast).

(() => {
  "use strict";

  const ROLE_ORDER = ["field", "prop", "type", "import", "deps", "i18n", "fixture", "test", "other"];
  const ROLE_LABELS = {
    field: "fields", prop: "props", type: "types", import: "imports", deps: "dependencies",
    i18n: "translations", fixture: "fixtures", test: "tests", other: "other",
  };
  const XREF_LABELS = {
    "xref-dangling": "still referenced",
    "xref-i18n-orphan": "translation keys",
    "xref-storage-shape": "stored payload shape",
    "xref-payload-drop": "request payload",
  };
  const XREF_ROWS = 40; // rows rendered per code before "… and N more"

  // The lens the user picked: undefined = the strongest detected concept, "" = off, else a
  // stem. `explicit`: typed by hand, so analyses must ask the engine for it.
  let chosen;
  let explicit = false;
  let expanded = false; // the concept row in the Patterns tab
  const xrefCache = new Map(); // "report|stem" -> {state, data?, error?}
  const xrefOpen = new Set(); // codes whose list is open in the inspector
  const setsCache = new WeakMap(); // concept -> {explained, touched}
  const groupsCache = new WeakMap(); // report -> Map(id -> group)

  const prefKey = () => `refactor-diff:concept:${document.getElementById("repo")?.title || ""}`;
  const normalize = (s) => String(s || "").trim().toLowerCase();

  function init(defaults) {
    if (defaults && defaults.concept) {
      // --concept: this session's lens, not saved over the user's choice.
      chosen = normalize(defaults.concept);
      explicit = true;
      return;
    }
    try {
      const saved = JSON.parse(prefGet(prefKey()) || "null");
      if (saved && typeof saved.stem === "string") {
        chosen = saved.stem;
        explicit = Boolean(saved.explicit) && Boolean(saved.stem);
      }
    } catch {}
  }

  function concepts() {
    const list = state.report && state.report.concepts;
    return Array.isArray(list) ? list : [];
  }

  function active() {
    if (chosen === "") return null;
    const list = concepts();
    if (chosen === undefined) return list[0] || null;
    return list.find((c) => c.stem === chosen) || null;
  }

  function sets(c) {
    let s = setsCache.get(c);
    if (!s) {
      s = { explained: new Set(c.explained_unit_ids || []), touched: new Set(c.touched_unit_ids || []) };
      setsCache.set(c, s);
    }
    return s;
  }

  const explains = (uid) => { const c = active(); return Boolean(c) && sets(c).explained.has(uid); };
  const touches = (uid) => { const c = active(); return Boolean(c) && sets(c).touched.has(uid); };
  const explainedCount = () => { const c = active(); return c ? (c.explained_unit_ids || []).length : 0; };

  // The stem to send with POST /api/analyze: only one typed by hand (detected ones come anyway).
  const requestStem = () => (explicit && chosen ? chosen : null);

  function groupById(id) {
    const r = state.report;
    let m = groupsCache.get(r);
    if (!m) { m = new Map(r.groups.map((g) => [g.id, g])); groupsCache.set(r, m); }
    return m.get(id) || null;
  }

  // Pick a lens. A stem the report doesn't have is analyzed for, keeping the page and scroll.
  function choose(stem) {
    stem = normalize(stem);
    const found = concepts().find((c) => c.stem === stem);
    const inReport = !stem || Boolean(found);
    chosen = stem;
    // Keep asking for a concept the engine didn't detect by itself.
    explicit = Boolean(stem) && (!found || found.auto === false);
    prefSet(prefKey(), JSON.stringify({ stem: chosen, explicit }));
    if (!state.report) return;
    if (inReport) { rerender(); return; }
    state.keepViewOnLoad = true;
    toast(`Analyzing with the concept “${stem}”…`, { busy: true });
    runAnalysis().then(() => {
      if (active()) toast(`Concept lens: ${stem}.`);
      else toast(`No changes mention “${stem}”.`, { error: true });
    });
  }

  // ---- explorer foot: the picker ----

  function pickerHtml() {
    if (!state.report) return "";
    const list = concepts();
    const a = active();
    const chips = list.map((c) => `<button type="button" class="filter-chip concept-chip" data-concept="${esc(c.stem)}"
      aria-pressed="${a === c}" title="${c.auto ? `Detected: ${Math.round((c.score || 0) * 100)}% of the changed identifiers contain “${esc(c.stem)}”` : "Asked for"}">${
      esc(c.stem)} <span class="n">${c.auto ? `${Math.round((c.score || 0) * 100)}%` : "asked"}</span></button>`).join("");
    const missing = chosen && !list.some((c) => c.stem === chosen)
      ? `<button type="button" class="filter-chip concept-chip" data-concept-analyze="${esc(chosen)}" aria-pressed="false"
          title="Not in this report: analyze again with it">${esc(chosen)} <span class="n">analyze</span></button>` : "";
    const off = list.length || chosen ? `<button type="button" class="filter-chip concept-chip" data-concept="" aria-pressed="${!a}"
      title="Turn the concept lens off">off</button>` : "";
    return `<div class="row concept-picker"><span class="filter-label">concept</span>${chips}${missing}${off}
      <input id="concept-input" type="text" spellcheck="false" placeholder="${list.length ? "another…" : "e.g. program"}"
        title="Analyze with another concept: a stem like “program” or “site”" aria-label="Analyze with another concept"></div>`;
  }

  function bindPicker(root) {
    for (const b of root.querySelectorAll("[data-concept]")) {
      b.addEventListener("click", () => choose(b.dataset.concept));
    }
    for (const b of root.querySelectorAll("[data-concept-analyze]")) {
      b.addEventListener("click", () => choose(b.dataset.conceptAnalyze, { typed: true }));
    }
    const input = root.querySelector("#concept-input");
    if (!input) return;
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && normalize(input.value)) { e.preventDefault(); choose(input.value, { typed: true }); }
      if (e.key === "Escape") { input.value = ""; input.blur(); }
    });
  }

  function focusPicker() {
    if (!state.panes.explorer) setPane("explorer", true);
    const input = document.getElementById("concept-input");
    if (input) input.focus();
  }

  // ---- patterns, by role ----

  // "label (+2 variants)" -> {base, variants}
  function splitVariants(label) {
    const m = /^(.*) \(\+(\d+) variants?\)$/.exec(label || "");
    return m ? { base: m[1], variants: Number(m[2]) } : { base: label || "", variants: 0 };
  }

  function byRole(c) {
    const out = new Map();
    for (const p of c.patterns || []) {
      const role = ROLE_ORDER.includes(p.role) ? p.role : "other";
      if (!out.has(role)) out.set(role, []);
      out.get(role).push(p);
    }
    return ROLE_ORDER.filter((r) => out.has(r)).map((r) => [r, out.get(r)]);
  }

  // A pattern's groups as they stand under the filters: [{g, visible}]
  function patternGroups(p) {
    const v = state.view;
    return (p.group_ids || []).map((id) => {
      const g = groupById(id);
      if (!g) return null;
      const shown = v && v.groupsById.get(id);
      return { g, visible: shown ? shown.visible.length : 0 };
    }).filter(Boolean);
  }

  function variantChip(n) {
    return n ? `<span class="variants" title="${plural(n, "variant")} folded into this pattern">+${n}</span>` : "";
  }

  function supergroupHtml(isActive) {
    const c = active();
    if (!c) return "";
    const href = pageHref(c.stem);
    let html = `<div class="hd"><span>concept</span><span>${c.auto ? "detected" : "asked for"}</span></div>
      <a class="node nav concept-node${isActive(href)}" href="${href}" title="Every pattern that only adds or removes “${esc(c.stem)}”, by role">
        <span class="caret" role="button" tabindex="0" data-concept-expand aria-expanded="${expanded}" aria-label="${expanded ? "Collapse" : "Expand"}">${expanded ? "▾" : "▸"}</span>
        <span class="kind concept">concept</span>
        <span class="label"><b>${esc(c.stem)}</b> · ${plural((c.patterns || []).length, "pattern")} · ${plural((c.explained_unit_ids || []).length, "unit")}</span></a>`;
    if (!expanded) return html;
    for (const [role, pats] of byRole(c)) {
      html += `<div class="dir concept-role">${esc(ROLE_LABELS[role] || role)} <span class="n">${pats.length}</span></div>`;
      for (const p of pats) {
        const gs = patternGroups(p);
        const first = gs[0];
        const n = gs.reduce((k, x) => k + x.visible, 0);
        const { base, variants } = splitVariants(p.label);
        html += `<a class="node concept-pattern${n ? "" : " dim"}" href="${first ? `#group/${first.g.id}` : href}" title="${esc(p.label)}">
          ${first ? kindChip(first.g.kind) : ""}<span class="label">${esc(base)}</span>${variantChip(variants)}<span class="count">×${n}</span></a>`;
      }
    }
    return html;
  }

  function onExplorerClick(e) {
    const t = e.target.closest("[data-concept-expand]");
    if (!t) return false;
    e.preventDefault();
    e.stopPropagation();
    expanded = !expanded;
    renderExplorer();
    return true;
  }

  const pageHref = (stem) => `#concept/${encodeURIComponent(stem || (active() || {}).stem || "")}`;

  function title(stem) {
    const c = concepts().find((x) => x.stem === stem);
    return `<span class="kind concept">concept</span><span class="transform">${esc(stem)}</span>${
      c ? `<span class="meta">${plural((c.patterns || []).length, "pattern")} · ${plural((c.explained_unit_ids || []).length, "unit")}</span>` : ""}`;
  }

  // #concept/<stem>: the concept's patterns by role, and where the collateral is.
  function renderPage(stem) {
    const content = document.getElementById("content");
    const c = concepts().find((x) => x.stem === stem);
    if (!c) {
      content.innerHTML = `<div class="empty"><h2>No concept “${esc(stem)}” in this report</h2>
        <p>Analyze again with it to see which changes only add or remove it.</p>
        <button type="button" class="toggle" data-concept-page-analyze="${esc(stem)}">Analyze with “${esc(stem)}”</button></div>`;
      content.querySelector("[data-concept-page-analyze]")?.addEventListener("click", () => choose(stem, { typed: true }));
      return;
    }
    const isActive = active() === c;
    const s = sets(c);
    const v = state.view;
    const r = state.report;
    const collateral = [...v.units].filter((id) => !r.units[id].explained && !s.explained.has(id)).length;
    const hide = state.filters.hideConceptExplained;
    let html = `<div class="page-head concept-head">
      <p>Patterns that only add or remove <span class="code">${esc(c.stem)}</span>, by what they touch. Units the concept
      fully explains (the diff is empty once it is stripped) can be hidden, leaving the collateral changes to read.</p>
      <div class="concept-facts">
        <span class="chip"><b>${(c.explained_unit_ids || []).length}</b> units only ${esc(c.stem)}</span>
        <span class="chip"><b>${(c.touched_unit_ids || []).length}</b> differ beyond it</span>
        <span class="chip collateral"><b>${collateral}</b> collateral to review</span>
        ${c.auto ? `<span class="chip">detected · ${Math.round((c.score || 0) * 100)}% of changed identifiers</span>` : `<span class="chip">asked for</span>`}
        <span class="spacer"></span>
        ${isActive ? `<label class="toggle"><input type="checkbox" data-concept-hide ${hide ? "checked" : ""}> Hide concept-only edits</label>`
          : `<button type="button" class="toggle" data-concept-use>Use as the lens</button>`}
      </div></div>`;
    for (const [role, pats] of byRole(c)) {
      html += `<h3 class="sub-head">${esc(ROLE_LABELS[role] || role)} <span class="meta">${plural(pats.length, "pattern")}</span></h3>
        <div class="concept-patterns">`;
      for (const p of pats) {
        const gs = patternGroups(p);
        const n = gs.reduce((k, x) => k + x.visible, 0);
        const all = gs.reduce((k, x) => k + x.g.unit_ids.length, 0);
        const { base, variants } = splitVariants(p.label);
        const first = gs[0];
        const more = gs.slice(1).map((x, i) => `<a href="#group/${x.g.id}" title="${esc(x.g.label)}">${i + 2}</a>`).join("");
        html += `<div class="concept-pattern-row${n ? "" : " dim"}">
          ${first ? kindChip(first.g.kind) : ""}
          <a class="label code" href="${first ? `#group/${first.g.id}` : "#"}" title="${esc(p.label)}">${esc(base)}</a>${variantChip(variants)}
          ${more ? `<span class="more-groups">also ${more}</span>` : ""}
          <span class="count" title="${n === all ? "" : `${all - n} hidden by filters`}">×${n}${n === all ? "" : ` <span class="dim">/ ${all}</span>`}</span></div>`;
      }
      html += "</div>";
    }
    const files = Object.entries(c.files || {})
      .filter(([, f]) => f.collateral_lines > 0)
      .sort((a, b) => b[1].collateral_lines - a[1].collateral_lines || a[0].localeCompare(b[0]))
      .slice(0, 20);
    if (files.length) {
      html += `<h3 class="sub-head">Where the collateral is <span class="meta">top ${files.length} files by collateral lines</span></h3>
        <div class="concept-files">${files.map(([path, f]) => `<a class="concept-file" href="${fileHref(path, "diff")}" title="${esc(path)}">
          <span class="path">${esc(path)}</span>${heatHtml(path, c)}<span class="count">${f.collateral_lines} collateral · ${f.concept_lines} ${esc(c.stem)}</span></a>`).join("")}</div>`;
    }
    content.innerHTML = html;
    content.querySelector("[data-concept-hide]")?.addEventListener("change", () => toggleFilter("hideConceptExplained"));
    content.querySelector("[data-concept-use]")?.addEventListener("click", () => choose(c.stem));
  }

  // ---- overlays on the review ----

  function heatHtml(path, c = active()) {
    const f = c && c.files ? c.files[path] : null;
    if (!f) return "";
    const total = (f.concept_lines || 0) + (f.collateral_lines || 0);
    if (!total) return "";
    const pct = Math.round((100 * (f.concept_lines || 0)) / total);
    return `<span class="heat" title="${esc(c.stem)}: ${plural(f.concept_lines || 0, "line")} · collateral: ${plural(f.collateral_lines || 0, "line")}"><i style="width:${pct}%"></i></span>`;
  }

  function unitTagsHtml(unit) {
    const c = active();
    if (!c) return "";
    const s = sets(c);
    if (s.touched.has(unit.id)) {
      return `<span class="tag concept touched" title="Mentions “${esc(c.stem)}” but still differs once it is stripped: read this one">differs beyond concept</span>`;
    }
    if (s.explained.has(unit.id) && !unit.explained) {
      return `<span class="tag concept only" title="Only adds or removes “${esc(c.stem)}”: the diff is empty once it is stripped">concept only</span>`;
    }
    return "";
  }

  // 0: a hunk with a change that doesn't mention the concept, 1: one that touches it but
  // differs beyond it, 2: only the concept.
  function hunkRank(h) {
    const c = active();
    if (!c) return 0;
    const s = sets(c);
    const r = state.report;
    const v = state.view;
    const units = h.unit_ids.filter((id) => v.units.has(id) && !r.units[id].explained);
    if (units.some((id) => !s.explained.has(id) && !s.touched.has(id))) return 0;
    if (units.some((id) => s.touched.has(id))) return 1;
    return 2;
  }

  // Residual units (under the filters) the concept doesn't explain.
  function collateralUnits() {
    const c = active();
    if (!c || !state.view) return 0;
    const s = sets(c);
    const r = state.report;
    let n = 0;
    for (const id of state.view.units) if (!r.units[id].explained && !s.explained.has(id)) n++;
    return n;
  }

  function statusHtml() {
    const c = active();
    if (!c) return "";
    const on = state.filters.hideConceptExplained;
    return `<button type="button" class="link concept-status" id="status-collateral" aria-pressed="${on}"
      title="Changes left once everything that only adds or removes “${esc(c.stem)}” is set apart. Click to ${on ? "show" : "hide"} the concept-only edits">collateral ${collateralUnits()}</button>`;
  }

  function statsRows(row, dim) {
    const c = active();
    if (!c) return "";
    return row("Concept", `<b>${esc(c.stem)}</b> ${dim(`· ${(c.explained_unit_ids || []).length} units only it · ${(c.touched_unit_ids || []).length} differ beyond it`)}`)
      + row("Collateral", `${collateralUnits()} ${dim("residual changes the concept doesn't explain")}`);
  }

  // ---- inspector: cross-references (code issues) ----

  function xrefKey() {
    const c = active();
    return `${state.report.id}|${c ? c.stem : ""}`;
  }

  function loadXref() {
    const key = xrefKey();
    if (xrefCache.has(key)) return xrefCache.get(key);
    const entry = { state: "loading" };
    xrefCache.set(key, entry);
    const c = active();
    const id = state.report.id;
    api(`/api/report/${id}/xref${c ? `?concept=${encodeURIComponent(c.stem)}` : ""}`)
      .then((data) => { entry.state = "done"; entry.data = data || {}; })
      .catch((e) => { entry.state = "error"; entry.error = e.message; })
      .finally(() => { if (state.report && state.report.id === id) renderInspector(); });
    return entry;
  }

  function inspectorXref() {
    if (!state.report) return "";
    const entry = loadXref();
    const muted = (html) => `<div class="prose" style="color: var(--muted)">${html}</div>`;
    if (entry.state === "loading") return `<h2>Cross-references</h2>${muted("Checking what the diff left behind…")}`;
    if (entry.state === "error") return `<h2>Cross-references</h2>${muted(`Couldn't check: ${esc(entry.error)}`)}`;
    const d = entry.data;
    const checks = Array.isArray(d.checks) ? d.checks : [];
    const counts = d.counts || {};
    const total = Number(d.total) || checks.length;
    if (!checks.length) return `<h2>Cross-references <span>0</span></h2>${muted("Nothing the diff left behind.")}`;
    const byCode = new Map();
    for (const ch of checks) {
      if (!byCode.has(ch.code)) byCode.set(ch.code, []);
      byCode.get(ch.code).push(ch);
    }
    const warnings = (list) => list.filter((x) => x.severity === "error" || x.severity === "warning").length;
    // Codes with warnings first, then the biggest.
    const codes = [...byCode.entries()].sort((a, b) =>
      (warnings(b[1]) > 0) - (warnings(a[1]) > 0) || (counts[b[0]] || b[1].length) - (counts[a[0]] || a[1].length));
    const nWarn = checks.filter((x) => x.severity === "warning" || x.severity === "error").length;
    let html = `<h2>Cross-references <span class="xref-total" title="Code issues: what still refers to what the diff removed. Not refactor warnings.">${total}</span></h2>
      <div class="card xref">`;
    for (const [code, list] of codes) {
      const n = counts[code] || list.length;
      const w = warnings(list);
      const open = xrefOpen.has(code);
      const rows = list.slice(0, XREF_ROWS).map((ch) => {
        const sev = ["error", "warning", "info"].includes(ch.severity) ? ch.severity : "info";
        return `<a class="xref-row ${sev}" href="${fileHref(ch.path, "diff", ch.line ? "n" + ch.line : "")}" title="${esc(ch.path)}:${ch.line}\n${esc(ch.message)}">
          <span class="sev" aria-label="${sev}">${ISSUE_ICONS[sev] || "ℹ"}</span><span class="where">${esc(basename(ch.path))}:${ch.line}</span><span class="msg">${esc(ch.message)}</span></a>`;
      }).join("");
      const rest = n - Math.min(list.length, XREF_ROWS);
      html += `<details class="xref-group" data-xref-code="${esc(code)}"${open ? " open" : ""}>
        <summary><span class="code">${esc(XREF_LABELS[code] || code)}</span><span class="xref-n">${n}${w ? ` · <span class="issue-warning">${w} ⚠</span>` : ""}</span></summary>
        ${rows}${rest > 0 ? `<div class="xref-more">…and ${rest} more</div>` : ""}</details>`;
    }
    html += `</div>`;
    if (d.truncated) html += muted(`Showing the first ${checks.length} of ${total} (${nWarn} warnings among them).`);
    return html;
  }

  // <details> open state survives the inspector's re-renders.
  document.addEventListener("toggle", (e) => {
    const el = e.target;
    if (!(el instanceof HTMLElement) || !el.matches("details[data-xref-code]")) return;
    if (el.open) xrefOpen.add(el.dataset.xrefCode); else xrefOpen.delete(el.dataset.xrefCode);
  }, true);

  // ---- palette and keys ----

  function commands() {
    if (!state.report) return [];
    const a = active();
    const cmds = concepts().filter((c) => c !== a).map((c) => ({
      id: `concept.use.${c.stem}`, group: "Concept", title: `Concept lens: ${c.stem}`, run: () => choose(c.stem),
    }));
    if (a) {
      cmds.push(
        { id: "concept.off", group: "Concept", title: "Concept lens: off", run: () => choose("") },
        { id: "concept.page", group: "View", title: `Go to concept: ${a.stem}`, keys: "g o", run: () => { location.hash = pageHref(a.stem); } },
      );
    }
    cmds.push({ id: "concept.other", group: "Concept", title: "Analyze with another concept…", run: focusPicker });
    return cmds;
  }

  const HELP = ["Concept lens", [
    ["g o", "", "go to the active concept's patterns, by role"],
  ]];
  try { KEY_HELP.push(HELP); } catch {}

  window.Concepts = {
    init, active, explains, touches, explainedCount, requestStem, choose,
    pickerHtml, bindPicker, supergroupHtml, onExplorerClick, renderPage, title, pageHref,
    heatHtml, unitTagsHtml, hunkRank, statusHtml, statsRows, inspectorXref, commands, collateralUnits,
  };
})();
