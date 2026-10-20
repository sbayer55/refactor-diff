// ---------- story view ----------
// #story (g y): the needs-review hunks retold in the order a change usually flows through a
// front-end codebase: API and services first, then context/state, hooks, components and pages,
// mocks, tests, and docs/config last. Within a layer, files with the most collateral (lines
// the active concept doesn't explain) come first. Hunks render with hunkHtml inside
// section.file[data-file-section], so j/k/x/e/a and single-file mode work as on Needs review.
//
// Also here: the tests summary chip in the status bar ("−N tests · M modified · +K new") and
// its panel listing the removed tests.
//
// Runs after app.js (and concepts.js) and uses their helpers (state, esc, plural, hunkHtml,
// fileSectionHtml, renderAnswers, fileHref, basename, Concepts, KEY_HELP).

(() => {
  "use strict";

  const LAYERS = [
    { id: "api", label: "API and services", hint: "requests, endpoints, service clients" },
    { id: "state", label: "Context and state", hint: "providers, stores" },
    { id: "hooks", label: "Hooks", hint: "use… hooks" },
    { id: "ui", label: "Components and pages", hint: "what renders" },
    { id: "source", label: "Other source", hint: "helpers, libraries, types" },
    { id: "mocks", label: "Mocks and fixtures", hint: "mock data, fixtures" },
    { id: "tests", label: "Tests", hint: "test files" },
    { id: "docs", label: "Docs and config", hint: "documentation, configuration, the rest" },
  ];

  function layerOf(file) {
    const p = "/" + file.path.toLowerCase();
    const name = p.slice(p.lastIndexOf("/") + 1);
    if (/\/(mocks?|__mocks__|fixtures?)\//.test(p) || /fixture/.test(name)) return "mocks";
    if (file.category === "tests") return "tests";
    if (["docs", "config", "other"].includes(file.category)) return "docs";
    if (/\/(services|api)\//.test(p) || /-api\.[cm]?[jt]sx?$/.test(name)) return "api";
    if (/\/(context|contexts|store|stores)\//.test(p)) return "state";
    if (/\/hooks\//.test(p) || /^use[A-Z0-9_-]/.test(file.path.slice(file.path.lastIndexOf("/") + 1))) return "hooks";
    if (/\/(components|pages|modules)\//.test(p)) return "ui";
    return "source";
  }

  // Collateral share of a file under the active concept (1: nothing explained by it).
  function collateral(path) {
    const c = Concepts.active();
    const f = c && c.files ? c.files[path] : null;
    if (!f) return { share: 1, lines: 0 };
    const total = (f.concept_lines || 0) + (f.collateral_lines || 0);
    return { share: total ? (f.collateral_lines || 0) / total : 1, lines: f.collateral_lines || 0 };
  }

  function layers() {
    const r = state.report;
    const v = state.view;
    const fileOf = new Map(r.files.map((f) => [f.path, f]));
    const byFile = new Map();
    for (const hid of v.residualHunks) {
      const h = r.hunks[hid];
      if (!byFile.has(h.path)) byFile.set(h.path, []);
      byFile.get(h.path).push(h);
    }
    const out = new Map(LAYERS.map((l) => [l.id, []]));
    for (const [path, hunks] of byFile) {
      const f = fileOf.get(path) || { path, category: "other" };
      if (Concepts.active()) hunks.sort((a, b) => Concepts.hunkRank(a) - Concepts.hunkRank(b));
      out.get(layerOf(f)).push({ path, hunks, ...collateral(path) });
    }
    for (const files of out.values()) {
      files.sort((a, b) => b.share - a.share || b.lines - a.lines || a.path.localeCompare(b.path));
    }
    return LAYERS.map((l) => ({ ...l, files: out.get(l.id) })).filter((l) => l.files.length);
  }

  function title() {
    const s = state.view.stats;
    return `Story <span class="meta">${plural(s.hunks, "hunk")} by layer${Concepts.active() ? " · collateral first" : ""}</span>`;
  }

  function render() {
    const content = document.getElementById("content");
    const v = state.view;
    if (!v.residualHunks.length) {
      content.innerHTML = `<div class="empty"><h2>Nothing left to review</h2><p>Every change in the visible files matched a pattern.</p></div>`;
      return;
    }
    const ls = layers();
    const c = Concepts.active();
    let html = `<div class="page-head story-head">
      <p>The needs-review hunks in the order the change flows: services, state, hooks, components, then mocks, tests and docs.
      ${c ? `Within a layer, the files with the most changes beyond <span class="code">${esc(c.stem)}</span> come first.` : ""}</p>
      <nav class="story-toc">${ls.map((l) => `<a href="#story" data-story-layer="${l.id}">${esc(l.label)} <span class="n">${l.files.length}</span></a>`).join("")}</nav>
    </div>`;
    for (const l of ls) {
      const n = l.files.reduce((k, f) => k + f.hunks.length, 0);
      html += `<h3 class="sub-head story-layer" id="story-${l.id}" title="${esc(l.hint)}">${esc(l.label)}
        <span class="meta">${plural(l.files.length, "file")} · ${plural(n, "hunk")}</span></h3>`;
      for (const f of l.files) html += fileSectionHtml(f.path, f.hunks);
    }
    content.innerHTML = html;
    for (const a of content.querySelectorAll("[data-story-layer]")) {
      a.addEventListener("click", (e) => { e.preventDefault(); scrollToSelector(`#story-${a.dataset.storyLayer}`); });
    }
    for (const hunkId of state.answers.keys()) renderAnswers(hunkId);
  }

  function navRow(active) {
    return `<a class="node nav${active("#story")}" href="#story" title="Needs review by layer: services → state → hooks → components → mocks → tests (g y)">
      <span class="label sans">Story</span><span class="count">by layer</span></a>`;
  }

  // ---- tests summary ----

  const summaries = new WeakMap(); // report -> summary
  const TEST_CALL = /^\s*(?:it|test|describe)(?:\.\w+)*\s*\(\s*(['"`])(.*?)\1/;

  function testsSummary() {
    const r = state.report;
    let s = summaries.get(r);
    if (s) return s;
    const removed = [];
    const isTestGroup = (g) => g && g.kind === "test" && !g.new;
    for (const g of r.groups) {
      if (!isTestGroup(g)) continue;
      const m = /removed test: "(.*)"$/s.exec(g.old || "");
      const u = r.units[g.unit_ids[0]];
      // The block's first line: where its title is.
      const at = u ? u.old.findIndex((ln) => m && ln.text.includes(m[1])) : -1;
      removed.push({ title: m ? m[1] : g.label, path: u ? u.path : "", line: u ? u.old_start + Math.max(0, at) : 0, group: g.id });
    }
    // Tagged units without a test pattern (a deleted test file, say): count their blocks.
    for (const u of Object.values(r.units)) {
      if (!u.tags.includes("test-removed")) continue;
      if (u.signatures.some((k) => isTestGroup(state.groupsByKey.get(k)))) continue;
      u.old.forEach((ln, i) => {
        const m = TEST_CALL.exec(ln.text);
        if (m) removed.push({ title: m[2], path: u.path, line: u.old_start + i, group: null });
      });
    }
    const tests = r.files.filter((f) => f.category === "tests");
    s = {
      removed,
      modified: tests.filter((f) => f.status === "M" || f.status === "R").length,
      added: tests.filter((f) => f.status === "A").length,
      deletedFiles: tests.filter((f) => f.status === "D").length,
    };
    summaries.set(r, s);
    return s;
  }

  function testsChipHtml() {
    if (!state.report) return "";
    const s = testsSummary();
    if (!s.removed.length && !s.modified && !s.added) return "";
    const parts = [];
    if (s.removed.length) parts.push(`<span class="tests-removed">−${plural(s.removed.length, "test")}</span>`);
    if (s.modified) parts.push(`${s.modified} modified`);
    if (s.added) parts.push(`+${s.added} new`);
    return `<button type="button" class="link tests-chip" id="status-tests" aria-haspopup="dialog"
      title="Tests in this diff: deleted it/test/describe blocks, and test files modified or added. Click for the removed tests">${parts.join(" · ")}</button>`;
  }

  let pop = null;
  function closeTestsPanel() {
    if (pop) { pop.remove(); pop = null; }
  }

  function toggleTestsPanel(anchor) {
    if (pop) { closeTestsPanel(); return; }
    const s = testsSummary();
    pop = document.createElement("div");
    pop.className = "tests-pop";
    pop.setAttribute("role", "dialog");
    pop.setAttribute("aria-label", "Removed tests");
    const rows = s.removed.map((t) => `<a class="tests-row" href="${t.path ? fileHref(t.path, "diff", t.line ? "o" + t.line : "") : "#"}" title="${esc(t.path)}:${t.line}">
      <span class="t">${esc(t.title)}</span><span class="where">${esc(basename(t.path))}:${t.line}</span></a>`).join("");
    pop.innerHTML = `<header>Removed tests <span class="dim">${s.removed.length}</span></header>
      ${rows || `<div class="dim">No test blocks were deleted.</div>`}
      <footer class="dim">${plural(s.modified, "test file")} modified · ${s.added} added${s.deletedFiles ? ` · ${s.deletedFiles} deleted` : ""}</footer>`;
    document.body.append(pop);
    const r = anchor.getBoundingClientRect();
    pop.style.left = `${Math.max(8, Math.min(r.left, window.innerWidth - pop.offsetWidth - 8))}px`;
    pop.style.bottom = `${window.innerHeight - r.top + 6}px`;
    pop.addEventListener("click", (e) => { if (e.target.closest("a")) closeTestsPanel(); });
  }

  document.addEventListener("mousedown", (e) => {
    if (pop && !pop.contains(e.target) && !e.target.closest("#status-tests")) closeTestsPanel();
  });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") closeTestsPanel(); });

  function commands() {
    if (!state.report) return [];
    const cmds = [{ id: "view.story", group: "View", title: "Go to Story (needs review by layer)", keys: "g y", run: () => { location.hash = "#story"; } }];
    if (testsSummary().removed.length) {
      cmds.push({ id: "view.tests-removed", group: "View", title: "Show removed tests",
        run: () => { const b = document.getElementById("status-tests"); if (b) toggleTestsPanel(b); } });
    }
    return cmds;
  }

  const HELP = ["Story", [
    ["g y", "", "go to the Story: needs review by layer (services → state → hooks → components → mocks → tests)"],
  ]];
  try { KEY_HELP.push(HELP); } catch {}

  window.Story = { render, title, navRow, layerOf, testsSummary, testsChipHtml, toggleTestsPanel, commands };
})();
