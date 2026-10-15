// ---------- file history ----------
// The History tab of the file viewer: every commit that touched the file in a list beside the
// code (newest first), ghost previews of any commit in place, `p` to pin a version as A and
// split the view (older on the left) with ribbons tying the blocks that differ, folds for
// unchanged runs and single changes, blame heat in the gutter, and checkout. Ported from
// docs/mockups/file-history/8-ribbons.html onto the viewer's <table class="diff"> rows.
//
// Runs after app.js and uses its helpers (esc, api, prefGet/prefSet, toast, confirmDialog,
// navAttrs, fileHref, editorHref, paint, Syntax, state, backButton). It owns #content while
// open and touches no other app state.

(() => {
  "use strict";

  const PREF = "refactor-diff:history.";
  const HOVER_MS = 80;
  const PAGE = 200;
  const q = (sel, root = document) => root.querySelector(sel);
  const qa = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  const clamp = (n, lo, hi) => Math.max(lo, Math.min(hi, n));

  const pref = {
    get(k, d) { const v = prefGet(PREF + k); return v === null || v === undefined ? d : v; },
    bool(k, d) { const v = pref.get(k, null); return v === null ? d : v === "1"; },
    set(k, v) { prefSet(PREF + k, typeof v === "boolean" ? (v ? "1" : "0") : String(v)); },
  };

  let H = null; // the open view, or null

  // ---- small formatters ----
  const fmtDate = (d) => d ? new Date(d).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" }) : "";
  function rel(d) {
    if (!d) return "";
    const days = Math.round((Date.now() - new Date(d)) / 864e5);
    if (days < 1) return "today";
    if (days < 30) return `${days}d ago`;
    if (days < 365) return `${Math.round(days / 30)}mo ago`;
    return `${(days / 365).toFixed(1)}y ago`;
  }
  const initials = (name) => (name || "?").split(/\s+/).map((w) => w[0] || "").join("").slice(0, 2).toUpperCase();

  // ---- icons (inline, for the collapsed toolbar) ----
  const svg = (body) => `<svg class="ico" viewBox="0 0 16 16" aria-hidden="true">${body}</svg>`;
  const ICON = {
    diff: svg('<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M8 2.5v11M3.8 8h2.4M9.8 8h2.4M11 6.8v2.4"/>'),
    old: svg('<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M3 2.5h5v11H3a1.5 1.5 0 0 1-1.5-1.5V4A1.5 1.5 0 0 1 3 2.5z" fill="currentColor" stroke="none"/>'),
    new: svg('<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><path d="M8 2.5h5A1.5 1.5 0 0 1 14.5 4v8a1.5 1.5 0 0 1-1.5 1.5H8z" fill="currentColor" stroke="none"/>'),
    history: svg('<path d="M2.6 8a5.4 5.4 0 1 0 1.6-3.8"/><path d="M2.4 2.6v3.2h3.2"/><path d="M8 5v3.4l2.2 1.3"/>'),
    sync: svg('<path d="M5 2.5v11M2.5 11 5 13.5 7.5 11"/><path d="M11 13.5v-11M8.5 5 11 2.5 13.5 5"/>'),
    fold: svg('<path d="M2 8h12"/><path d="M5.5 4.5 8 2l2.5 2.5M5.5 11.5 8 14l2.5-2.5"/>'),
    list: svg('<path d="M2 4h12M2 8h12M2 12h12"/>'),
    ribbons: svg('<path d="M2 4c6 0 6 8 12 8M2 12c6 0 6-8 12-8"/>'),
    wrap: svg('<path d="M2 4h12M2 8h9a2.5 2.5 0 0 1 0 5H8"/><path d="M9.5 11.5 8 13l1.5 1.5"/>'),
    gear: svg('<circle cx="8" cy="8" r="2.2"/><path d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6l1.4 1.4M11 11l1.4 1.4M3.6 12.4 5 11M11 5l1.4-1.4"/>'),
    open: svg('<path d="M6 3H3.5A1.5 1.5 0 0 0 2 4.5v8A1.5 1.5 0 0 0 3.5 14h8a1.5 1.5 0 0 0 1.5-1.5V10"/><path d="M9 2h5v5M14 2 7.5 8.5"/>'),
  };

  // ============================================================ open / close

  function open(content, path, opts = {}) {
    if (H && H.path === path && H.content === content) { applyHashState(opts); return; }
    if (H) close();
    const report = state.report;
    H = {
      content, path, id: report.id,
      file: report.files.find((f) => f.path === path) || null,
      commits: [], bySha: new Map(), more: false, total: 0,
      cur: -1, pinned: null, ghost: null, sticky: false,
      foldAll: false, folded: new Set(),
      s: {
        side: pref.bool("side", true),
        sideWidth: clamp(Number(pref.get("side-width", 236)) || 236, 150, 460),
        sync: pref.bool("sync", true),
        ribbons: pref.get("ribbons", null) === null ? window.innerWidth >= 900 : pref.bool("ribbons", true),
        wrap: pref.bool("wrap", false),
        labels: pref.get("labels", "auto") === "icons" ? "icons" : "auto",
        tips: pref.bool("tips", true),
      },
      cache: { content: new Map(), diff: new Map(), diffs: new Map(), blame: new Map(), syntax: new Map() },
      panes: {}, lock: false, raf: 0, hoverTimer: 0, token: 0, cleanup: [], prof: {}, redrawFull: false,
    };
    content.classList.add("history-mode");
    renderShell();
    loadHistory(opts);
  }

  function close() {
    if (!H) return;
    for (const fn of H.cleanup) { try { fn(); } catch {} }
    cancelAnimationFrame(H.raf);
    clearTimeout(H.hoverTimer);
    H.content.classList.remove("history-mode");
    H.content.innerHTML = "";
    q(".blame-tip")?.remove();
    q("#status .hist-status")?.remove();
    q("#status .hist-spacer")?.remove();
    H = null;
  }

  const isOpen = () => Boolean(H);

  // `#file/<path>/history[/<sha>[/<a>]][/<line>]`: shas by shape, then an optional o12/n12.
  function parseHash(rest) {
    const out = { sha: null, a: null, line: null };
    for (const seg of rest || []) {
      if (/^[0-9a-f]{7,40}$/i.test(seg) || seg === ":worktree:") {
        if (!out.sha) out.sha = seg; else if (!out.a) out.a = seg;
      } else if (/^[on]\d+$/.test(seg)) out.line = seg;
    }
    return out;
  }

  function applyHashState(opts) {
    if (!H || !H.commits.length) return;
    const i = opts.sha ? indexOfSha(opts.sha) : -1;
    if (i >= 0 && i !== H.cur) select(i);
  }

  function indexOfSha(sha) {
    if (H.bySha.has(sha)) return H.bySha.get(sha);
    const i = H.commits.findIndex((c) => c.sha.startsWith(sha));
    return i;
  }

  function writeHash() {
    if (!H || H.cur < 0) return;
    const parts = [`#file/${encodeURIComponent(H.path)}/history`, shaOf(H.cur)];
    if (H.pinned !== null && H.pinned !== H.cur) parts.push(shaOf(H.pinned));
    const hash = parts.join("/");
    if (location.hash !== hash) history.replaceState(null, "", hash);
  }

  // ============================================================ shell

  function tabsHtml() {
    const f = H.file;
    const line = H.openLine ? `/${H.openLine}` : "";
    const modes = [["diff", "Diff", ICON.diff], ["old", "Original", ICON.old], ["new", "New", ICON.new], ["history", "History", ICON.history]];
    return modes.map(([m, label, ico]) => {
      const disabled = (m === "diff" && !f) || (m === "old" && f?.status === "A") || (m === "new" && f?.status === "D");
      if (disabled) return `<span class="tab disabled" data-tip="${m === "diff" ? "This file has no changes in this diff" : label}">${ico}<span class="long">${label}</span></span>`;
      const href = m === "history" ? "#" : `${fileHref(H.path, m)}${line}`;
      return `<a class="tab" href="${href}" ${m === "history" ? 'aria-current="page"' : ""} data-tip="${label}">${ico}<span class="long">${label}</span></a>`;
    }).join("");
  }

  function renderShell() {
    const s = H.s;
    const btn = (id, ico, label, key, tip, pressed) =>
      `<button type="button" class="toggle" id="${id}" aria-pressed="${pressed ? "true" : "false"}" data-tip="${esc(tip)}${key ? " · " + key : ""}">${ico}<span class="long">${label}</span>${key ? ` <kbd>${key}</kbd>` : ""}</button>`;
    const editor = editorHref(H.path, 1);
    H.content.innerHTML = `<div class="history" id="history">
      <div class="viewer-bar ${s.labels}">
        ${backButton()}
        <nav class="tabs" aria-label="File version">${tabsHtml()}</nav>
        <span class="spacer"></span>
        ${btn("h-side", ICON.list, "commits", "h", "Show or hide the commit list", s.side)}
        ${btn("h-sync", ICON.sync, "sync scroll", "s", "Scroll both sides together, following the ribbons", s.sync)}
        ${btn("h-fold", ICON.fold, "fold unchanged", "f", "Fold or unfold every unchanged region", false)}
        ${btn("h-ribbons", ICON.ribbons, "ribbons", "r", "Show or hide the ribbons between the panes", s.ribbons)}
        ${btn("h-wrap", ICON.wrap, "wrap", "w", "Soft-wrap long lines", s.wrap)}
        ${editor ? `<a class="toggle" id="h-editor" href="${esc(editor)}" data-tip="${esc(EDITOR_TITLE)}">${ICON.open}<span class="long">Open in editor</span></a>` : ""}
        <button type="button" class="toggle tip-left" id="h-gear" data-tip="History settings" aria-haspopup="true">${ICON.gear}</button>
      </div>
      <div class="tips" id="h-tips" ${s.tips ? "" : "hidden"}>
        <span><kbd>↑</kbd><kbd>↓</kbd> newer / older</span>
        <span>hover a commit to <b>ghost</b> its diff where you are · <kbd>space</kbd> keeps it after the mouse leaves</span>
        <span><kbd>p</kbd> pin A: the pane splits, older on the left, ribbons tie each changed block to its counterpart</span>
        <span>hover a line number or stripe for who wrote it · <kbd>?</kbd> all keys</span>
        <button type="button" class="close tip-left" id="h-tips-close" data-tip="Hide these tips (saved in settings)">×</button>
      </div>
      <div class="pin-hint" id="h-pinhint" hidden></div>
      <div class="hmain">
        <div class="hside" id="h-sidebar" style="--sw:${s.sideWidth}px">
          <div class="clist" id="h-list"><div class="side-head" id="h-list-head">Loading history…</div><div id="h-items"></div></div>
          <div class="resize" id="h-resize" title="Drag to resize"></div>
        </div>
        <div class="hboard" id="h-board">
          <div class="hpane"><div class="col"><div class="pane-head" id="h-head-l"></div><div class="hscroll" id="h-scroll-l"><table class="diff foldable" id="h-table-l"></table></div></div><div class="hmini" id="h-mini-l"><canvas></canvas><b class="vp"></b></div></div>
          <div class="hgutter" id="h-gutter"><button type="button" class="toggle lock" id="h-lock" data-tip="Sync scroll · s" aria-pressed="${s.sync}">⇅</button><svg id="h-svg"></svg></div>
          <div class="hpane"><div class="col"><div class="pane-head" id="h-head-r"></div><div class="hscroll" id="h-scroll-r"><table class="diff foldable" id="h-table-r"></table></div></div><div class="hmini" id="h-mini-r"><canvas></canvas><b class="vp"></b></div></div>
          <div class="ghost-banner" id="h-banner"></div>
          <div class="hpop menu" id="h-comenu" hidden></div>
          <div class="hpop menu" id="h-settings" hidden>
            <div class="group"><h5>Toolbar labels</h5>
              <label><input type="radio" name="h-labels" value="auto" id="h-labels-auto"> Auto <span class="sub">icons only when narrow</span></label>
              <label><input type="radio" name="h-labels" value="icons" id="h-labels-icons"> Always icons</label></div>
            <div class="group"><h5>Help</h5><label><input type="checkbox" id="h-tips-on"> Show the tips row</label></div>
            <div class="sub" style="font-size:11px">Saved in settings and kept between sessions.</div>
          </div>
        </div>
      </div>
    </div>`;
    H.root = q("#history");
    H.board = q("#h-board");
    H.side = q("#h-sidebar");
    for (const side of ["l", "r"]) {
      H.panes[side] = {
        side, head: q(`#h-head-${side}`), scroller: q(`#h-scroll-${side}`), table: q(`#h-table-${side}`),
        mini: q(`#h-mini-${side}`), version: null, mode: null, segs: new Map(), blame: null, token: 0,
      };
    }
    bindShell();
    applySide(); applyRibbons(); applyWrap(); applyLabels();
    ensureStatusSlot();
  }

  function bindShell() {
    const on = (el, ev, fn, opts) => { el.addEventListener(ev, fn, opts); H.cleanup.push(() => el.removeEventListener(ev, fn, opts)); };
    q("#back-btn").addEventListener("click", () => history.back());
    q("#h-side").addEventListener("click", () => { H.s.side = !H.s.side; pref.set("side", H.s.side); applySide(); });
    q("#h-sync").addEventListener("click", toggleSync);
    q("#h-lock").addEventListener("click", toggleSync);
    q("#h-fold").addEventListener("click", () => foldAllRuns(!H.foldAll));
    q("#h-ribbons").addEventListener("click", () => { H.s.ribbons = !H.s.ribbons; pref.set("ribbons", H.s.ribbons); applyRibbons(); });
    q("#h-wrap").addEventListener("click", () => { H.s.wrap = !H.s.wrap; pref.set("wrap", H.s.wrap); applyWrap(); });
    q("#h-tips-close").addEventListener("click", () => { H.s.tips = false; pref.set("tips", false); q("#h-tips").hidden = true; q("#h-tips-on").checked = false; toast("Tips hidden. ⚙ › Show the tips row brings them back."); });
    q("#h-tips-on").addEventListener("change", (e) => { H.s.tips = e.target.checked; pref.set("tips", H.s.tips); q("#h-tips").hidden = !H.s.tips; });
    for (const r of qa('input[name="h-labels"]')) r.addEventListener("change", () => { H.s.labels = r.value; pref.set("labels", r.value); applyLabels(); });
    q("#h-gear").addEventListener("click", (e) => { e.stopPropagation(); togglePop("#h-settings"); });
    on(document, "click", (e) => { if (!e.target.closest(".hpop, #h-gear, [data-act=co]")) hidePops(); });
    // Resizable commit list.
    q("#h-resize").addEventListener("pointerdown", (e) => {
      e.preventDefault();
      const handle = e.currentTarget;
      handle.setPointerCapture(e.pointerId);
      H.side.classList.add("resizing");
      const x0 = e.clientX, w0 = H.s.sideWidth;
      const mv = (ev) => { H.s.sideWidth = clamp(w0 + ev.clientX - x0, 150, 460); H.side.style.setProperty("--sw", H.s.sideWidth + "px"); };
      const up = () => { H.side.classList.remove("resizing"); pref.set("side-width", H.s.sideWidth); handle.removeEventListener("pointermove", mv); scheduleRedraw(); };
      handle.addEventListener("pointermove", mv);
      handle.addEventListener("pointerup", up, { once: true });
    });
    // Panes: scroll (sync + redraw), hover (blame), clicks (folds, blame jumps).
    for (const side of ["l", "r"]) {
      const p = H.panes[side];
      p.scroller.addEventListener("scroll", () => { const t0 = performance.now(); follow(p); H.prof["scroll.follow"] = Math.round((performance.now() - t0) * 10) / 10; scheduleRedraw(false); }, { passive: true });
      p.mini.addEventListener("pointerdown", (e) => {
        const go = (ev) => { const r = p.mini.getBoundingClientRect(); p.scroller.scrollTop = ((ev.clientY - r.top) / r.height) * p.scroller.scrollHeight - p.scroller.clientHeight / 2; };
        go(e); p.mini.setPointerCapture(e.pointerId);
        const mv = (ev) => go(ev);
        p.mini.addEventListener("pointermove", mv);
        p.mini.addEventListener("pointerup", () => p.mini.removeEventListener("pointermove", mv), { once: true });
      });
    }
    H.board.addEventListener("click", onBoardClick);
    H.board.addEventListener("mouseover", onBoardOver);
    H.board.addEventListener("mouseout", onBoardOut);
    q("#h-svg").addEventListener("click", (e) => {
      const id = e.target.dataset?.id; if (!id) return;
      for (const side of ["l", "r"]) { const p = H.panes[side], tr = q(`tr[data-seg="${id}"]`, p.table); if (tr) p.scroller.scrollTop = rowTop(p.scroller, tr) - p.scroller.clientHeight / 3; }
    });
    const ro = new ResizeObserver(() => scheduleRedraw());
    ro.observe(H.board);
    H.cleanup.push(() => ro.disconnect());
    on(window, "resize", scheduleRedraw);
  }

  function togglePop(sel) { const el = q(sel); const show = el.hidden; hidePops(); el.hidden = !show; }
  function hidePops() { for (const el of qa(".hpop", H.root)) el.hidden = true; }

  function toggleSync() {
    H.s.sync = !H.s.sync; pref.set("sync", H.s.sync);
    q("#h-sync").setAttribute("aria-pressed", H.s.sync); q("#h-lock").setAttribute("aria-pressed", H.s.sync);
    if (H.s.sync && isSplit()) follow(curPane());
  }
  function applySide() { H.side.classList.toggle("hidden", !H.s.side); q("#h-side").setAttribute("aria-pressed", H.s.side); setTimeout(scheduleRedraw, 400); }
  function applyRibbons() { H.board.classList.toggle("noribbons", !H.s.ribbons); q("#h-ribbons").setAttribute("aria-pressed", H.s.ribbons); setTimeout(scheduleRedraw, 450); }
  function applyWrap() { H.root.classList.toggle("wrap", H.s.wrap); q("#h-wrap").setAttribute("aria-pressed", H.s.wrap); scheduleRedraw(); }
  function applyLabels() {
    const bar = q(".viewer-bar", H.root);
    bar.classList.toggle("auto", H.s.labels === "auto"); bar.classList.toggle("icons", H.s.labels === "icons");
    q(`#h-labels-${H.s.labels}`).checked = true;
    q("#h-tips-on").checked = H.s.tips;
  }

  // ============================================================ data

  const shaOf = (i) => H.commits[i].sha;
  const pathAt = (i) => H.commits[i].path || H.path;
  const url = (what, params) => `/api/report/${H.id}/history/${what}?` + Object.entries(params).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join("&");

  async function loadHistory(opts) {
    H.openLine = opts.line || null;
    let data;
    try {
      data = await api(url("commits", { path: H.path, limit: PAGE }));
    } catch (e) {
      if (!H) return;
      q("#h-list-head").textContent = "No history";
      H.board.innerHTML = `<div class="empty"><p>${esc(e.message)}</p></div>`;
      return;
    }
    if (!H) return;
    H.commits = data.commits.slice().reverse(); // oldest → newest
    H.more = data.more; H.total = data.total;
    H.bySha = new Map(H.commits.map((c, i) => [c.sha, i]));
    if (!H.commits.length) { q("#h-list-head").textContent = "No history"; H.board.innerHTML = `<div class="empty"><p>No commit touched ${esc(H.path)}.</p></div>`; return; }
    const want = opts.sha ? indexOfSha(opts.sha) : -1;
    H.cur = want >= 0 ? want : H.commits.length - 1;
    if (opts.a) { const a = indexOfSha(opts.a); if (a >= 0 && a !== H.cur) H.pinned = a; }
    renderList();
    await show();
    if (H && opts.line) {
      const n = Number(opts.line.slice(1)), p = curPane();
      const tr = q(`tr[data-n="${n}"]`, p.table) || nearestRowIn(p.table, n);
      if (tr) { p.scroller.scrollTop = Math.max(0, rowTop(p.scroller, tr) - p.scroller.clientHeight / 2); flashRow(tr); }
    }
    prefetchAround();
  }

  function nearestRowIn(table, n) {
    let best = null, dist = Infinity;
    for (const tr of qa("tr[data-n]", table)) { const d = Math.abs(Number(tr.dataset.n) - n); if (d < dist) { best = tr; dist = d; } }
    return best;
  }

  async function loadOlder() {
    const el = q("#h-more"); if (el) el.textContent = "Loading…";
    let data;
    try { data = await api(url("commits", { path: H.path, limit: PAGE, offset: H.commits.length })); } catch (e) { toast(e.message, { error: true }); return; }
    if (!H) return;
    const older = data.commits.slice().reverse();
    H.commits = older.concat(H.commits);
    H.more = data.more;
    H.bySha = new Map(H.commits.map((c, i) => [c.sha, i]));
    const n = older.length;
    H.cur += n; if (H.pinned !== null) H.pinned += n; if (H.ghost !== null) H.ghost += n;
    // Indices moved: caches keyed by sha stay valid, blame indexes do not.
    H.cache.blame.clear();
    for (const p of Object.values(H.panes)) if (p.version !== null) p.version += n;
    renderList();
  }

  function linesOf(i) {
    const sha = shaOf(i);
    if (!H.cache.content.has(sha)) {
      const p = api(url("content", { sha, path: pathAt(i) })).then((d) => d.lines).catch((e) => { H.cache.content.delete(sha); throw e; });
      H.cache.content.set(sha, p);
    }
    return H.cache.content.get(sha);
  }

  // Per line: the index of the commit (in H.commits) that last wrote it, −1 when older than the
  // listed history, plus the raw commit table for tooltips.
  function blameOf(i) {
    const sha = shaOf(i);
    if (!H.cache.blame.has(sha)) {
      const p = api(url("blame", { sha, path: pathAt(i) })).then((d) => {
        const map = d.commits.map((c) => (H.bySha.has(c.sha) ? H.bySha.get(c.sha) : -1));
        const idx = new Int32Array(d.lines.length);
        const raw = new Int32Array(d.lines.length);
        for (let k = 0; k < d.lines.length; k++) { idx[k] = map[d.lines[k]]; raw[k] = d.lines[k]; }
        return { idx, raw, commits: d.commits, origin: d.origin };
      }).catch((e) => { H.cache.blame.delete(sha); throw e; });
      H.cache.blame.set(sha, p);
    }
    return H.cache.blame.get(sha);
  }

  // The diff from the older index `lo` to the newer `hi`, expanded to per-line ops plus two
  // line maps (nearest surviving line at or after), so following a line across versions is O(1).
  function diffOf(lo, hi) {
    const key = `${shaOf(lo)}..${shaOf(hi)}`;
    if (!H.cache.diff.has(key)) {
      const p = api(url("diff", { path: H.path, from: shaOf(lo), to: shaOf(hi), from_path: pathAt(lo), to_path: pathAt(hi) })).then((d) => {
        const ops = [];
        for (const [tag, i1, i2, j1, j2] of d.ops) {
          if (tag === "=") { for (let k = 0; k < i2 - i1; k++) ops.push({ t: "eq", a: i1 + k, b: j1 + k }); continue; }
          for (let a = i1; a < i2; a++) ops.push({ t: "del", a });
          for (let b = j1; b < j2; b++) ops.push({ t: "add", b });
        }
        const a2b = new Int32Array(d.from_lines + 1).fill(-1), b2a = new Int32Array(d.to_lines + 1).fill(-1);
        for (const o of ops) if (o.t === "eq") { a2b[o.a] = o.b; b2a[o.b] = o.a; }
        a2b[d.from_lines] = d.to_lines; b2a[d.to_lines] = d.from_lines;
        for (let k = d.from_lines - 1, last = d.to_lines; k >= 0; k--) { if (a2b[k] < 0) a2b[k] = last; else last = a2b[k]; }
        for (let k = d.to_lines - 1, last = d.from_lines; k >= 0; k--) { if (b2a[k] < 0) b2a[k] = last; else last = b2a[k]; }
        const out = { ops, hunks: d.hunks, stats: d.stats, a2b, b2a, lenA: d.from_lines, lenB: d.to_lines };
        H.cache.diffs.set(key, out);
        return out;
      }).catch((e) => { H.cache.diff.delete(key); throw e; });
      H.cache.diff.set(key, p);
    }
    return H.cache.diff.get(key);
  }
  const loadedDiff = (lo, hi) => H.cache.diffs.get(`${shaOf(lo)}..${shaOf(hi)}`) || null;

  function mapLine(from, to, line) {
    if (from === to) return line;
    const d = loadedDiff(Math.min(from, to), Math.max(from, to));
    if (!d) return line;
    const m = from < to ? d.a2b : d.b2a;
    return m[clamp(line, 0, m.length - 1)];
  }

  function syntaxOf(i, lines) {
    const sha = shaOf(i);
    if (!H.cache.syntax.has(sha)) {
      const lang = Syntax.forPath(H.path);
      if (!lang) H.cache.syntax.set(sha, null);
      else {
        let st = lang.start();
        H.cache.syntax.set(sha, lines.map((text) => { const r = lang.line(text, st); st = r.state; return r.spans; }));
      }
    }
    return H.cache.syntax.get(sha);
  }

  function prefetchAround() {
    const run = () => {
      if (!H) return;
      const c = H.cur, n = H.commits.length;
      for (const i of [c - 1, c + 1, c - 2, c + 2]) if (i >= 0 && i < n) linesOf(i).catch(() => {});
      for (const i of [c - 1, c + 1]) if (i >= 0 && i < n) { blameOf(i).catch(() => {}); diffOf(Math.min(i, c), Math.max(i, c)).catch(() => {}); }
    };
    (window.requestIdleCallback || ((f) => setTimeout(f, 120)))(run);
  }

  // ============================================================ heat

  const heatPct = (origin, cur) => (origin < 0 ? 0 : Math.round((1 - (cur - origin) / Math.max(1, cur)) * 100));
  const heat = (origin, cur) => `color-mix(in oklab, var(--heat-new) ${heatPct(origin, cur)}%, var(--heat-old))`;
  const colorCache = new Map();
  function resolveColor(css) {
    if (!colorCache.has(css)) {
      let probe = q("#h-probe");
      if (!probe) { probe = document.createElement("span"); probe.id = "h-probe"; probe.style.cssText = "position:absolute;visibility:hidden"; document.body.appendChild(probe); }
      probe.style.color = css;
      colorCache.set(css, getComputedStyle(probe).color);
    }
    return colorCache.get(css);
  }
  const themeKey = () => (matchMedia("(prefers-color-scheme: dark)").matches ? "d" : "l");

  // ============================================================ rows

  // Row descriptor → <tr>. `version` names which commit's line numbering `line` follows; Cmd-click
  // navigation and issue underlines only work on the report's own two versions.
  function rowHtml(r) {
    const nav = r.line != null && r.version != null ? navSide(r.version, r.line + 1) : "";
    const cls = [r.kind, r.head && r.foldable ? "seg-head" : "", r.head && r.type === "hunk" ? "hunk-head" : ""].filter(Boolean).join(" ");
    const btn = r.head && r.foldable ? `<button type="button" class="fold-toggle" data-fold="${r.seg}" aria-label="Fold" title="${r.type === "run" ? "Fold these unchanged lines" : "Collapse this change"}"></button>` : "";
    const lineAttrs = r.line != null ? ` data-line="${r.line}" data-n="${r.line + 1}"` : "";
    const hs = r.origin === undefined ? "" : ` style="--heat:${heat(r.origin, r.version)}"`;
    return `<tr class="${cls}"${lineAttrs} data-seg="${r.seg}"${r.head ? ` data-seg-head="${r.seg}"` : ""}><td class="age"${hs}></td><td class="no">${btn}${r.num ?? ""}</td><td class="text"${nav}>${r.spans ? paint(r.text, r.spans) : esc(r.text)}</td></tr>`;
  }
  const markerHtml = (seg) => `<tr class="marker" data-seg="${seg}" title="lines exist only on the other side"><td colspan="3"></td></tr>`;

  function navSide(version, num) {
    const c = H.commits[version];
    if (c.is_head && state.report.source.head_sha) return navAttrs(pathAt(version), "n", num);
    if (c.is_base) return navAttrs(pathAt(version), "o", num);
    return "";
  }

  // Group consecutive rows into runs (unchanged) and hunks; a run shorter than 4 lines is not
  // foldable. Returns the segment table for the fold labels.
  function segmentRows(rows, isChange) {
    const segs = new Map();
    let cur = null, ri = 0, hi = 0;
    for (const r of rows) {
      const type = isChange(r) ? "hunk" : "run";
      if (!cur || cur.type !== type) { cur = { id: (type === "run" ? "r" : "h") + (type === "run" ? ri++ : hi++), type, count: 0, add: 0, del: 0 }; segs.set(cur.id, cur); r.head = true; }
      cur.count++;
      if (/add/.test(r.kind)) cur.add++; else if (/del/.test(r.kind)) cur.del++;
      r.seg = cur.id; r.type = type; r.segRef = cur;
    }
    for (const r of rows) r.foldable = r.type === "hunk" || r.segRef.count >= 4;
    return segs;
  }

  function setTable(pane, rows, segs, version, mode, blame) {
    const t0 = performance.now();
    const html = rows.map((r) => (r.kind === "marker" ? markerHtml(r.seg) : rowHtml(r))).join("");
    const t1 = performance.now();
    pane.table.innerHTML = html;
    const t2 = performance.now();
    pane.version = version; pane.mode = mode; pane.segs = segs; pane.blame = blame || null;
    pane.miniSig = null;
    paintFolds(pane);
    const t3 = performance.now();
    H.prof["table.build"] = Math.round(t1 - t0); H.prof["table.dom"] = Math.round(t2 - t1); H.prof["table.folds"] = Math.round(t3 - t2);
  }

  // ---- folds: tr.folded-away for hidden rows, the head row carries the label ----
  function paintFolds(pane) {
    let headRow = null, folded = false;
    for (const tr of qa("tr[data-seg]", pane.table)) {
      if (tr.dataset.segHead !== undefined) {
        headRow = tr; folded = H.folded.has(tr.dataset.seg);
        tr.classList.toggle("folded", folded);
        const td = tr.querySelector("td.text"), seg = pane.segs.get(tr.dataset.seg);
        if (folded && seg) td.dataset.foldLabel = seg.type === "run" ? `⋯ ${seg.count} unchanged lines` : `collapsed change · +${seg.add} −${seg.del}`;
        else delete td.dataset.foldLabel;
        tr.classList.toggle("folded-away", false);
        continue;
      }
      tr.classList.toggle("folded-away", folded && headRow !== null);
    }
  }
  function toggleFold(id) {
    if (H.folded.has(id)) H.folded.delete(id); else H.folded.add(id);
    for (const p of Object.values(H.panes)) if (p.segs.has(id)) paintFolds(p);
    scheduleRedraw();
  }
  function foldAllRuns(on) {
    H.foldAll = on;
    q("#h-fold").setAttribute("aria-pressed", on);
    for (const p of Object.values(H.panes)) {
      for (const [id, seg] of p.segs) if (seg.type === "run" && seg.count >= 4) { if (on) H.folded.add(id); else H.folded.delete(id); }
      paintFolds(p);
    }
    scheduleRedraw();
  }
  function applyFoldAll(pane) {
    if (!H.foldAll) return;
    for (const [id, seg] of pane.segs) if (seg.type === "run" && seg.count >= 4) H.folded.add(id);
    paintFolds(pane);
  }

  // ---- scroll anchoring: keep the line at the top of the viewport across re-renders ----
  // Rows sit in a positioned scroller, so offsetTop is their content-coordinate top: no layout
  // reads beyond the first after a render.
  const rowTop = (scroller, el) => el.offsetTop;
  const hidden = (tr) => tr.offsetParent === null;
  // The first visible row whose bottom is below the viewport's top edge (binary search; folded
  // rows have no box and are skipped), then the next row that carries a line number.
  function anchorOf(pane) {
    const rows = pane.table.rows, top = pane.scroller.scrollTop;
    let lo = 0, hi = rows.length - 1, found = null;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      let m = mid, r = rows[m];
      while (r && hidden(r) && m < hi) r = rows[++m];
      if (!r || hidden(r)) { hi = mid - 1; continue; }
      if (r.offsetTop + r.offsetHeight > top + 1) { found = r; hi = mid - 1; } else lo = m + 1;
    }
    let r = found;
    while (r && (r.dataset.line === undefined || hidden(r))) r = r.nextElementSibling;
    if (!r) return { line: 0, offset: 0 };
    return { line: Number(r.dataset.line), offset: r.offsetTop - top };
  }
  function restoreAnchor(pane, line, offset) {
    let tr = q(`tr[data-line="${line}"]`, pane.table);
    if (!tr) { const rows = qa("tr[data-line]", pane.table); tr = rows.find((x) => Number(x.dataset.line) >= line) || rows.at(-1); }
    if (tr) pane.scroller.scrollTop = Math.max(0, tr.offsetTop - offset);
    return tr;
  }
  function holdAnchor(pane, tr, offset, ms) {
    const t0 = performance.now();
    const tick = () => { if (!H || !tr.isConnected) return; pane.scroller.scrollTop = Math.max(0, rowTop(pane.scroller, tr) - offset); if (performance.now() - t0 < ms) requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
  }

  // ============================================================ renders

  const isSplit = () => H.pinned !== null && H.pinned !== H.cur;
  const lo = () => Math.min(H.pinned, H.cur), hi = () => Math.max(H.pinned, H.cur);
  const curPane = () => (!isSplit() || H.cur === lo() ? H.panes.l : H.panes.r);

  async function renderSingle(pane, i, anchor) {
    const tok = ++pane.token;
    const t0 = performance.now();
    const [lines, blame] = await Promise.all([linesOf(i), blameOf(i)]);
    if (!H || pane.token !== tok) return;
    const t1 = performance.now();
    const spans = syntaxOf(i, lines);
    const rows = lines.map((text, k) => ({ kind: "ctx", changed: blame.idx[k] === i, line: k, num: k + 1, text, spans: spans && spans[k], origin: blame.idx[k], version: i }));
    const segs = segmentRows(rows, (r) => r.changed);
    setTable(pane, rows, segs, i, "single", blame);
    applyFoldAll(pane);
    if (anchor) restoreAnchor(pane, anchor.line, anchor.offset);
    H.prof["single.fetch"] = Math.round(t1 - t0); H.prof["single.render"] = Math.round(performance.now() - t1);
    scheduleRedraw();
  }

  // What commit `target` would do to the version you are reading, in place.
  async function renderGhost(pane, cur, target, anchor) {
    const tok = ++pane.token;
    const a = Math.min(cur, target), b = Math.max(cur, target), forward = target > cur;
    const t0 = performance.now();
    const [d, lines, blame, other] = await Promise.all([diffOf(a, b), linesOf(cur), blameOf(cur), linesOf(target)]);
    if (!H || pane.token !== tok) return;
    const t1 = performance.now();
    const spans = syntaxOf(cur, lines), ospans = syntaxOf(target, other);
    const rows = d.ops.map((o) => {
      if (o.t === "eq") { const line = forward ? o.a : o.b; return { kind: "ctx", line, num: line + 1, text: lines[line], spans: spans && spans[line], origin: blame.idx[line], version: cur }; }
      const leaving = forward ? o.t === "del" : o.t === "add";
      if (leaving) { const line = forward ? o.a : o.b; return { kind: "ghost-del", line, num: line + 1, text: lines[line], spans: spans && spans[line], origin: blame.idx[line], version: cur }; }
      const oline = forward ? o.b : o.a;
      return { kind: "ghost-add", line: null, num: "", text: other[oline], spans: ospans && ospans[oline], origin: undefined, version: null };
    });
    const segs = segmentRows(rows, (r) => r.kind !== "ctx");
    setTable(pane, rows, segs, cur, "ghost", blame);
    applyFoldAll(pane);
    if (anchor) restoreAnchor(pane, anchor.line, anchor.offset);
    H.prof["ghost.fetch"] = Math.round(t1 - t0); H.prof["ghost.render"] = Math.round(performance.now() - t1);
    scheduleRedraw();
  }

  // Side by side: each side at its true length, hunks on both sides sharing a segment id.
  async function renderSplit(anchor) {
    const a = lo(), b = hi(), L = H.panes.l, R = H.panes.r;
    const tok = ++L.token; R.token = tok;
    const t0 = performance.now();
    const [d, la, lb, ba, bb] = await Promise.all([diffOf(a, b), linesOf(a), linesOf(b), blameOf(a), blameOf(b)]);
    if (!H || L.token !== tok) return;
    const t1 = performance.now();
    const sa = syntaxOf(a, la), sb = syntaxOf(b, lb);
    const left = [], right = [];
    let seg = null, ri = 0, hi_ = 0;
    const segs = new Map();
    const begin = (type) => { seg = { id: (type === "run" ? "r" : "h") + (type === "run" ? ri++ : hi_++), type, count: 0, add: 0, del: 0, l: 0, r: 0 }; segs.set(seg.id, seg); };
    for (const o of d.ops) {
      const type = o.t === "eq" ? "run" : "hunk";
      if (!seg || seg.type !== type) begin(type);
      seg.count++;
      if (o.t !== "add") { left.push({ kind: o.t === "del" ? "del" : "ctx", line: o.a, num: o.a + 1, text: la[o.a], spans: sa && sa[o.a], origin: ba.idx[o.a], version: a, seg: seg.id, type, segRef: seg, head: seg.l === 0 }); seg.l++; if (o.t === "del") seg.del++; }
      if (o.t !== "del") { right.push({ kind: o.t === "add" ? "add" : "ctx", line: o.b, num: o.b + 1, text: lb[o.b], spans: sb && sb[o.b], origin: bb.idx[o.b], version: b, seg: seg.id, type, segRef: seg, head: seg.r === 0 }); seg.r++; if (o.t === "add") seg.add++; }
    }
    // A hunk with nothing on one side still needs an anchor row there.
    for (const s of segs.values()) {
      if (s.type !== "hunk") continue;
      if (!s.l) left.push({ kind: "marker", seg: s.id, type: "hunk", segRef: s, head: true, sortAfter: true });
      if (!s.r) right.push({ kind: "marker", seg: s.id, type: "hunk", segRef: s, head: true, sortAfter: true });
    }
    for (const rows of [left, right]) {
      rows.sort((x, y) => segOrder(x.seg) - segOrder(y.seg) || 0);
      for (const r of rows) r.foldable = r.type === "hunk" || r.segRef.count >= 4;
    }
    setTable(L, left, segs, a, "split", ba);
    setTable(R, right, segs, b, "split", bb);
    applyFoldAll(L); applyFoldAll(R);
    const pane = H.cur === a ? L : R, other = pane === L ? R : L;
    if (anchor) restoreAnchor(pane, mapLine(anchor.version, H.cur, anchor.line), anchor.offset);
    H.lock = false; follow(pane, other, true);
    H.prof["split.fetch"] = Math.round(t1 - t0); H.prof["split.render"] = Math.round(performance.now() - t1);
    scheduleRedraw();
  }
  // Segment ids are numbered in order of appearance per type; marker rows need the interleaved
  // order, which the op walk fixes by giving every row the same monotonically growing key.
  function segOrder(id) { return Number(id.slice(1)) * 2 + (id[0] === "h" ? 1 : 0); }

  // Mapped sync scroll: the line at the top of one side finds its counterpart on the other.
  function follow(src, dst, force) {
    if (!H || !isSplit() || (!H.s.sync && !force) || H.lock) return;
    dst = dst || (src === H.panes.l ? H.panes.r : H.panes.l);
    H.lock = true;
    const a = anchorOf(src);
    restoreAnchor(dst, mapLine(src.version, dst.version, a.line), a.offset);
    dst.scroller.scrollLeft = src.scroller.scrollLeft;
    requestAnimationFrame(() => { if (H) H.lock = false; });
  }

  // Step to a neighbouring version in single mode: kept lines stay, removed lines flash and
  // collapse, added lines expand; the line at the top of the viewport stays where it is.
  async function morphTo(pane, to) {
    const from = pane.version;
    const tok = ++pane.token;
    const [d, lines, blame] = await Promise.all([diffOf(Math.min(from, to), Math.max(from, to)), linesOf(to), blameOf(to)]);
    if (!H || pane.token !== tok) return;
    const spans = syntaxOf(to, lines);
    const a = anchorOf(pane), target = mapLine(from, to, a.line);
    const forward = to > from;
    const old = new Map(qa("tr[data-line]", pane.table).map((tr) => [Number(tr.dataset.line), tr]));
    const tbody = document.createElement("tbody");
    const tmpl = document.createElement("template");
    const leaving = [], entering = [];
    for (const o of d.ops) {
      const fromLine = forward ? o.a : o.b, toLine = forward ? o.b : o.a;
      if (o.t === "eq") {
        const tr = old.get(fromLine); if (!tr) continue;
        tr.className = "ctx"; tr.dataset.line = toLine; tr.dataset.n = toLine + 1; delete tr.dataset.seg; delete tr.dataset.segHead;
        tr.querySelector("td.no").textContent = toLine + 1;
        tr.querySelector("td.age").style.setProperty("--heat", heat(blame.idx[toLine], to));
        tr.querySelector("td.text").removeAttribute("data-fold-label");
        tbody.appendChild(tr);
      } else if ((forward && o.t === "del") || (!forward && o.t === "add")) {
        const tr = old.get(fromLine); if (!tr) continue;
        tr.className = "ctx flash"; delete tr.dataset.line; delete tr.dataset.n; leaving.push(tr); tbody.appendChild(tr);
      } else {
        tmpl.innerHTML = rowHtml({ kind: "ctx entering", line: toLine, num: toLine + 1, text: lines[toLine], spans: spans && spans[toLine], origin: blame.idx[toLine], version: to, seg: "x" });
        const tr = tmpl.content.firstElementChild; entering.push(tr); tbody.appendChild(tr);
      }
    }
    pane.table.replaceChildren(tbody);
    pane.version = to; pane.mode = "single";
    const anchorRow = restoreAnchor(pane, target, a.offset);
    if (anchorRow) holdAnchor(pane, anchorRow, a.offset, 620);
    const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
    const settle = () => { if (H && pane.token === tok) renderSingle(pane, to, anchorOf(pane)); };
    if (reduced) { settle(); return; }
    requestAnimationFrame(() => {
      for (const tr of entering) tr.classList.remove("entering");
      setTimeout(() => { for (const tr of leaving) { tr.classList.remove("flash"); tr.classList.add("leaving"); } }, 240);
      setTimeout(() => {
        for (const tr of leaving) tr.remove();
        for (const tr of entering) { tr.classList.add("hl-add"); }
        setTimeout(settle, 300);
      }, 560);
    });
  }

  // ============================================================ state changes

  async function show(anchor) {
    if (!isSplit()) { await renderSingle(H.panes.l, H.cur, anchor); } else await renderSplit(anchor);
    paintState();
  }

  function paintState() {
    const split = isSplit();
    H.board.classList.toggle("split", split);
    H.side.classList.toggle("tucked", split && H.s.side);
    if (split) { headHtml(H.panes.l, lo()); headHtml(H.panes.r, hi()); } else headHtml(H.panes.l, H.cur);
    paintList();
    const hint = q("#h-pinhint");
    hint.hidden = !(H.pinned !== null && H.pinned === H.cur);
    if (!hint.hidden) hint.innerHTML = `<b>${esc(H.commits[H.cur].short)}</b> pinned as A. Move to any other commit to compare against it; older is always on the left.`;
    writeHash();
    setTimeout(scheduleRedraw, 450);
  }

  function headHtml(pane, i) {
    const c = H.commits[i];
    const tag = H.pinned === null ? "" : i === H.pinned ? '<span class="ab a">A</span>' : '<span class="ab b">B</span>';
    const chips = `${c.is_base ? '<span class="chip base">base</span>' : ""}${c.is_head ? '<span class="chip head">head</span>' : ""}${c.in_range && !c.is_base ? '<span class="chip pr">range</span>' : ""}`;
    pane.head.innerHTML = `${tag}<span class="sha">${esc(c.short)}${i === H.cur ? ' <i class="this">(this commit)</i>' : ""}</span>${chips}<span class="msg" title="${esc(c.subject)}">${esc(c.subject)}</span>
      <span class="who"><span class="avatar">${esc(initials(c.author))}</span> ${esc(c.author)} · ${fmtDate(c.date)}</span>
      <span class="acts"><button type="button" class="toggle" data-act="pin" data-i="${i}">${i === H.pinned ? "Unpin A" : "Pin as A"} <kbd>p</kbd></button><button type="button" class="toggle" data-act="co" data-i="${i}" ${c.sha === ":worktree:" ? "disabled" : ""}>Checkout</button></span>`;
  }

  function select(i) {
    i = clamp(i, 0, H.commits.length - 1);
    if (i === H.cur && H.ghost === null) return;
    const pane = curPane(), a = anchorOf(pane), prev = H.cur, wasGhost = H.ghost !== null;
    clearGhost();
    H.cur = i;
    const anchor = { version: prev, line: a.line, offset: a.offset };
    if (!isSplit() && H.pinned !== i) {
      if (wasGhost || pane !== H.panes.l || Math.abs(i - prev) !== 1) {
        diffOf(Math.min(prev, i), Math.max(prev, i)).catch(() => null).then(() => H && H.cur === i && renderSingle(H.panes.l, i, { line: mapLine(prev, i, a.line), offset: a.offset }));
      } else morphTo(H.panes.l, i);
    } else if (H.pinned === i) {
      diffOf(Math.min(prev, i), Math.max(prev, i)).catch(() => null).then(() => H && H.cur === i && renderSingle(H.panes.l, i, { line: mapLine(prev, i, a.line), offset: a.offset }));
    } else renderSplit(anchor);
    paintState();
    prefetchAround();
  }

  function pin(i) {
    const pane = curPane(), a = anchorOf(pane);
    clearGhost();
    if (H.pinned === i || (i === undefined && H.pinned !== null)) {
      H.pinned = null;
      renderSingle(H.panes.l, H.cur, { line: a.line, offset: a.offset });
      paintState();
      return;
    }
    H.pinned = i === undefined ? H.cur : i;
    if (isSplit()) renderSplit({ version: H.cur, line: a.line, offset: a.offset });
    else if (H.panes.l.mode !== "single") renderSingle(H.panes.l, H.cur, { line: a.line, offset: a.offset });
    paintState();
  }

  // ---- ghost: hover a commit; space makes it sticky ----
  function onHover(i) {
    clearTimeout(H.hoverTimer);
    if (H.pinned !== null || i === H.cur || H.sticky) return;
    H.hoverTimer = setTimeout(async () => {
      if (!H || H.pinned !== null || H.sticky) return;
      H.ghost = i;
      const pane = H.panes.l, a = anchorOf(pane);
      await renderGhost(pane, H.cur, i, { line: a.line, offset: a.offset });
      if (!H || H.ghost !== i) return;
      const d = loadedDiff(Math.min(H.cur, i), Math.max(H.cur, i));
      const fwd = i > H.cur, add = d ? (fwd ? d.stats.add : d.stats.del) : 0, del = d ? (fwd ? d.stats.del : d.stats.add) : 0;
      const b = q("#h-banner");
      b.innerHTML = `<span class="sticky">sticky</span><span>${fwd ? "Forward to" : "Back to"} <b>${esc(H.commits[i].short)}</b> · <span class="adds">+${add}</span> <span class="dels">−${del}</span> · <kbd>space</kbd> <span id="h-banner-hint">keeps it</span></span><button type="button" class="toggle" id="h-cmp">Side by side <kbd>⏎</kbd></button>`;
      q("#h-cmp").addEventListener("click", compareGhost);
      b.classList.add("show");
      paintStatusGhost();
    }, HOVER_MS);
  }
  function onLeave() {
    clearTimeout(H.hoverTimer);
    if (H.ghost === null || H.sticky) return;
    const a = anchorOf(H.panes.l);
    clearGhost();
    renderSingle(H.panes.l, H.cur, { line: a.line, offset: a.offset });
  }
  function clearGhost() {
    clearTimeout(H.hoverTimer);
    H.ghost = null; H.sticky = false;
    q("#h-banner")?.classList.remove("show", "is-sticky");
  }
  function toggleSticky() {
    if (H.ghost === null) return false;
    H.sticky = !H.sticky;
    q("#h-banner").classList.toggle("is-sticky", H.sticky);
    const hint = q("#h-banner-hint"); if (hint) hint.textContent = H.sticky ? "releases" : "keeps it";
    if (!H.sticky && !q("#h-list").matches(":hover")) onLeave();
    return true;
  }
  function compareGhost() {
    if (H.ghost === null) return;
    const target = H.ghost, a = anchorOf(H.panes.l);
    H.pinned = H.cur;
    clearGhost();
    H.cur = target;
    renderSplit({ version: H.pinned, line: a.line, offset: a.offset });
    paintState();
  }
  function paintStatusGhost() {}

  // ============================================================ commit list

  function renderList() {
    const maxChange = Math.max(1, ...H.commits.map((c) => c.insertions + c.deletions));
    const items = [];
    for (let i = H.commits.length - 1; i >= 0; i--) {
      const c = H.commits[i];
      items.push(`<div class="citem${c.in_range ? " pr" : ""}" data-i="${i}" tabindex="0">
        <span class="dot"></span>
        <span class="msg" title="${esc(c.subject)}">${esc(c.subject)}</span>
        <span class="meta"><span class="hash">${esc(c.short)}</span><span class="this-slot"></span> · ${esc(initials(c.author))} · ${rel(c.date)} ${c.is_base ? '<span class="chip base">base</span>' : ""}${c.is_head ? '<span class="chip head">head</span>' : ""}</span>
        <span class="delta"><span class="adds">+${c.insertions}</span> <span class="dels">−${c.deletions}</span></span>
        <span class="bar"><i class="d" style="width:${(c.deletions / maxChange) * 100}%"></i><i class="a" style="width:${(c.insertions / maxChange) * 100}%"></i></span>
        <span class="pinmark" title="Pinned as A">A</span>
        <button type="button" class="toggle pinbtn" data-pin="${i}">pin</button></div>`);
    }
    if (H.more) items.push(`<div class="cmore" id="h-more">Load older commits…</div>`);
    const list = q("#h-items");
    list.innerHTML = items.join("");
    q("#h-list-head").textContent = `${H.total || H.commits.length} commit${H.commits.length === 1 ? "" : "s"} · newest first`;
    if (!list.dataset.bound) {
      list.dataset.bound = "1";
      list.addEventListener("click", (e) => {
        const more = e.target.closest("#h-more"); if (more) { loadOlder(); return; }
        const item = e.target.closest(".citem"); if (!item) return;
        const i = Number(item.dataset.i);
        if (e.target.closest("[data-pin]")) pin(i); else select(i);
      });
      list.addEventListener("keydown", (e) => { const item = e.target.closest(".citem"); if (item && e.key === "Enter") { select(Number(item.dataset.i)); e.preventDefault(); } });
      list.addEventListener("mouseover", (e) => { const item = e.target.closest(".citem"); if (item && !item.contains(e.relatedTarget)) onHover(Number(item.dataset.i)); });
      list.addEventListener("mouseout", (e) => { const item = e.target.closest(".citem"); if (item && !item.contains(e.relatedTarget)) onLeave(); });
    }
    paintList();
  }

  function paintList() {
    for (const el of qa(".citem", q("#h-items"))) {
      const i = Number(el.dataset.i);
      el.classList.toggle("cur", i === H.cur);
      el.classList.toggle("pinned", i === H.pinned);
      el.classList.toggle("range", H.pinned !== null && i > lo() && i < hi());
      el.querySelector(".this-slot").innerHTML = i === H.cur ? ' <span class="this">(this commit)</span>' : "";
      el.querySelector(".pinbtn").textContent = i === H.pinned ? "unpin" : "pin";
    }
    q(".citem.cur", H.root)?.scrollIntoView({ block: "nearest" });
  }

  // ============================================================ board events: folds, blame, checkout

  function onBoardClick(e) {
    const fold = e.target.closest("[data-fold]");
    if (fold) { toggleFold(fold.dataset.fold); return; }
    const act = e.target.closest("[data-act]");
    if (act) {
      const i = Number(act.dataset.i);
      if (act.dataset.act === "pin") pin(i);
      else if (act.dataset.act === "co") { e.stopPropagation(); checkoutMenu(act, i); }
      return;
    }
    const cell = e.target.closest("td.age, td.no");
    if (cell && !e.target.closest(".fold-toggle")) {
      const tr = cell.parentElement, pane = paneOf(tr);
      const b = blameAt(pane, tr); if (!b) return;
      if (b.index === pane.version || b.index < 0) { flashRow(tr); return; }
      const text = tr.querySelector("td.text").textContent;
      select(b.index);
      setTimeout(() => {
        if (!H) return;
        const p = curPane(), hit = qa("tr[data-line]", p.table).find((x) => x.querySelector("td.text").textContent === text);
        if (hit) { p.scroller.scrollTop = Math.max(0, rowTop(p.scroller, hit) - p.scroller.clientHeight / 2); flashRow(hit); }
      }, 700);
    }
  }
  function paneOf(el) { return el.closest("#h-scroll-r") ? H.panes.r : H.panes.l; }
  function flashRow(tr) { tr.classList.add("focus"); setTimeout(() => tr.classList.remove("focus"), 1200); }

  // Blame for a row: the commit index (−1 = older than listed) and the raw commit record.
  function blameAt(pane, tr) {
    if (!pane.blame || tr.dataset.line === undefined) return null;
    const k = Number(tr.dataset.line);
    if (k >= pane.blame.idx.length) return null;
    return { index: pane.blame.idx[k], commit: pane.blame.commits[pane.blame.raw[k]], origin: pane.blame.origin[k] };
  }

  let lastRow = null;
  function onBoardOver(e) {
    const tr = e.target.closest("tr"); if (!tr || !tr.closest("table.diff")) return;
    const pane = paneOf(tr);
    if (tr !== lastRow) { lastRow = tr; statusLine(pane, tr); }
    const tip = blameTipEl();
    if (!e.target.closest("td.age, td.no")) { tip.classList.remove("show"); return; }
    const b = blameAt(pane, tr); if (!b) { tip.classList.remove("show"); return; }
    const c = b.commit, own = b.index === pane.version;
    tip.innerHTML = `<b>${esc(c.short)}${own ? " <i class=\"this\">(this commit)</i>" : ""}</b>${esc(c.subject)}<div class="muted">${esc(c.author)} · ${fmtDate(c.date)} · ${rel(c.date)}${!own && b.index >= 0 ? " · click to jump" : ""}${b.index < 0 ? " · older than the listed history" : ""}</div>`;
    const box = tr.getBoundingClientRect();
    tip.style.left = `${Math.min(box.left + 150, window.innerWidth - 20)}px`; tip.style.top = `${box.top - 6}px`;
    tip.classList.add("show");
  }
  function onBoardOut(e) {
    const to = e.relatedTarget;
    if (e.target.closest?.("td.age, td.no")) blameTipEl().classList.remove("show");
    if (!to || !to.closest?.("table.diff")) { lastRow = null; statusLine(null); }
  }
  function blameTipEl() {
    let tip = q(".blame-tip");
    if (!tip) { tip = document.createElement("div"); tip.className = "blame-tip"; document.body.appendChild(tip); }
    return tip;
  }
  function ensureStatusSlot() {
    const status = q("#status"); if (!status) return null;
    let slot = q(".hist-status", status);
    if (!slot) {
      const spacer = document.createElement("span"); spacer.className = "spacer hist-spacer";
      slot = document.createElement("span"); slot.className = "hist-status";
      status.append(spacer, slot);
    }
    return slot;
  }
  function statusLine(pane, tr) {
    const slot = ensureStatusSlot(); if (!slot) return;
    const b = pane && tr ? blameAt(pane, tr) : null;
    slot.innerHTML = b ? `<span class="ln">L${Number(tr.dataset.line) + 1}</span><b>${esc(b.commit.short)}${b.index === pane.version ? ' <span class="this">(this commit)</span>' : ""}</b>${esc(b.commit.author)} · ${rel(b.commit.date)}` : "";
  }

  // ---- checkout ----
  function checkoutMenu(anchor, i) {
    const c = H.commits[i];
    const menu = q("#h-comenu");
    menu.innerHTML = `<h5>${esc(c.short)} · ${esc(c.subject)}</h5>
      <button type="button" class="toggle" data-co="commit">Checkout commit <kbd>git checkout ${esc(c.short)}</kbd></button>
      <button type="button" class="toggle" data-co="file">Restore just this file <kbd>-- ${esc(pathAt(i))}</kbd></button>
      <button type="button" class="toggle" data-co="cancel">Cancel</button>`;
    hidePops();
    const r = anchor.getBoundingClientRect(), b = H.board.getBoundingClientRect();
    menu.style.top = `${r.bottom - b.top + 4}px`; menu.style.right = `${Math.max(8, b.right - r.right)}px`;
    menu.hidden = false;
    for (const btn of qa("[data-co]", menu)) btn.addEventListener("click", () => { menu.hidden = true; if (btn.dataset.co !== "cancel") runCheckout(i, btn.dataset.co); });
  }
  async function runCheckout(i, mode, force = false) {
    const c = H.commits[i], path = pathAt(i);
    const cmd = mode === "commit" ? `git checkout ${c.sha}` : `git checkout ${c.sha} -- ${path}`;
    const ok = await confirmDialog({
      title: mode === "commit" ? `Checkout ${c.short}?` : `Restore ${path} from ${c.short}?`,
      message: mode === "commit" ? `${cmd}\n\nHEAD will be detached; the working tree must be clean.` : `${cmd}\n\nThe file in your working tree is replaced (and staged).`,
      ok: mode === "commit" ? "Checkout" : "Restore",
    });
    if (!ok || !H) return;
    toast("Running git…", { busy: true, sticky: true });
    try {
      const res = await api(`/api/report/${H.id}/history/checkout`, { sha: c.sha, mode, path: mode === "file" ? path : undefined, force });
      toast(`${res.message}${res.stale ? " Re-run the analysis to see the new working tree." : ""}`);
    } catch (e) {
      toast(e.message, { error: true });
    }
  }

  // ============================================================ ribbons and minimaps

  // `full` re-measures rows (after a render, fold, wrap or resize); a scroll only moves things.
  function scheduleRedraw(full = true) {
    if (!H) return;
    if (full) H.redrawFull = true;
    if (H.raf) return;
    H.raf = requestAnimationFrame(() => { H.raf = 0; if (!H) return; const f = H.redrawFull; H.redrawFull = false; redraw(f); });
  }
  function redraw(full) {
    const t0 = performance.now();
    if (full) for (const side of ["l", "r"]) { measureHunks(H.panes[side]); measureMinimap(H.panes[side]); }
    const t1 = performance.now();
    drawRibbons();
    const t2 = performance.now();
    for (const side of ["l", "r"]) drawMinimap(H.panes[side]);
    const t3 = performance.now();
    H.prof[full ? "redraw.full" : "redraw.scroll"] = Math.round((t3 - t0) * 10) / 10;
    H.prof["redraw.measure"] = Math.round((t1 - t0) * 10) / 10; H.prof["redraw.ribbons"] = Math.round((t2 - t1) * 10) / 10; H.prof["redraw.mini"] = Math.round((t3 - t2) * 10) / 10;
  }

  function measureHunks(pane) {
    const out = new Map();
    for (const tr of qa("tr[data-seg^='h']", pane.table)) {
      if (hidden(tr)) continue;
      const id = tr.dataset.seg, top = tr.offsetTop, bottom = top + tr.offsetHeight;
      const r = out.get(id);
      if (!r) out.set(id, { top, bottom, l: tr.classList.contains("del"), r: tr.classList.contains("add") });
      else { r.bottom = bottom; r.l = r.l || tr.classList.contains("del"); r.r = r.r || tr.classList.contains("add"); }
    }
    pane.hunkRects = out;
  }
  function drawRibbons() {
    const svg = q("#h-svg");
    if (!isSplit() || !H.s.ribbons) { svg.innerHTML = ""; return; }
    const g = q("#h-gutter"), W = g.clientWidth, Hh = g.clientHeight, c = W / 2;
    const L = H.panes.l, R = H.panes.r, left = L.hunkRects || new Map(), right = R.hunkRects || new Map();
    const topL = L.scroller.scrollTop - headOffset(L), topR = R.scroller.scrollTop - headOffset(R);
    const out = [];
    for (const [id, a] of left) {
      const b = right.get(id); if (!b) continue;
      const a0 = a.top - topL, a1 = Math.max(a.bottom - topL, a0 + 3), b0 = b.top - topR, b1 = Math.max(b.bottom - topR, b0 + 3);
      if ((a1 < 0 && b1 < 0) || (a0 > Hh && b0 > Hh)) continue;
      const col = a.l && b.r ? "var(--accent)" : a.l ? "var(--del-fg)" : "var(--add-fg)";
      out.push(`<path data-id="${id}" d="M0,${a0} C${c},${a0} ${c},${b0} ${W},${b0} L${W},${b1} C${c},${b1} ${c},${a1} 0,${a1} Z" fill="${col}" fill-opacity=".26" stroke="${col}" stroke-opacity=".75" style="cursor:pointer"/>`);
    }
    svg.innerHTML = out.join("");
  }
  // The gutter spans the pane header too; ribbons are drawn in gutter coordinates.
  const headOffset = (pane) => pane.scroller.getBoundingClientRect().top - q("#h-gutter").getBoundingClientRect().top;

  function measureMinimap(pane) {
    const addC = resolveColor("var(--add-fg)"), delC = resolveColor("var(--del-fg)");
    const items = [];
    for (const tr of pane.table.rows) {
      if (hidden(tr)) continue;
      const cl = tr.classList;
      let color;
      if (cl.contains("add") || cl.contains("ghost-add") || cl.contains("hl-add")) color = addC;
      else if (cl.contains("del") || cl.contains("ghost-del")) color = delC;
      else if (cl.contains("marker")) continue;
      else color = resolveColor(tr.cells[0]?.style.getPropertyValue("--heat") || "var(--heat-old)");
      items.push(tr.offsetTop, tr.offsetHeight, color);
    }
    pane.miniRows = items;
    pane.miniTotal = pane.scroller.scrollHeight || 1;
  }
  function drawMinimap(pane) {
    const canvas = pane.mini.querySelector("canvas"), vp = pane.mini.querySelector(".vp");
    const w = pane.mini.clientWidth, h = pane.mini.clientHeight;
    if (!w || !h || !pane.miniRows || !pane.miniRows.length) { vp.style.height = "0"; return; }
    const scale = h / pane.miniTotal;
    const sig = `${w}x${h}@${pane.miniTotal}:${pane.miniRows.length}`;
    if (pane.miniSig !== sig) {
      pane.miniSig = sig;
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr);
      const ctx = canvas.getContext("2d");
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      const rows = pane.miniRows;
      for (let k = 0; k < rows.length; k += 3) { ctx.fillStyle = rows[k + 2]; ctx.fillRect(3, rows[k] * scale, w - 6, Math.max(1, rows[k + 1] * scale)); }
    }
    vp.style.top = `${pane.scroller.scrollTop * scale}px`; vp.style.height = `${pane.scroller.clientHeight * scale}px`;
  }

  // ============================================================ keyboard, palette, help

  function nextHunk(dir) {
    const p = curPane(), heads = qa("tr.hunk-head:not(.folded-away), tr.folded.hunk-head", p.table);
    if (!heads.length) return;
    const top = p.scroller.scrollTop + 2, tops = heads.map((tr) => rowTop(p.scroller, tr));
    let i = dir > 0 ? tops.findIndex((t) => t > top + 20) : tops.findLastIndex((t) => t < top - 20);
    if (i < 0) i = dir > 0 ? tops.length - 1 : 0;
    p.scroller.scrollTop = tops[i] - 20; flashRow(heads[i]);
  }

  function onKey(e) {
    if (!H || !H.commits.length) return false;
    switch (e.key) {
      case "ArrowUp": case "k": select(H.cur + 1); return true;
      case "ArrowDown": case "j": select(H.cur - 1); return true;
      case "Home": select(H.commits.length - 1); return true;
      case "End": select(0); return true;
      case " ": return toggleSticky();
      case "Enter": if (H.ghost !== null) { compareGhost(); return true; } return false;
      case "p": pin(H.cur); return true;
      case "Escape":
        if (H.ghost !== null) { H.sticky = false; onLeave(); return true; }
        if (H.pinned !== null) { pin(H.pinned); return true; }
        if (!q("#h-settings").hidden || !q("#h-comenu").hidden) { hidePops(); return true; }
        return false;
      case "c": { const btn = q(`#h-head-${curPane().side} [data-act="co"]`); if (btn && !btn.disabled) checkoutMenu(btn, H.cur); return true; }
      case "]": nextHunk(1); return true;
      case "[": nextHunk(-1); return true;
      case "s": toggleSync(); return true;
      case "f": case "z": foldAllRuns(!H.foldAll); return true;
      case "h": q("#h-side").click(); return true;
      case "r": q("#h-ribbons").click(); return true;
      case "w": q("#h-wrap").click(); return true;
      default: return false;
    }
  }

  const HELP = ["File history", [
    ["↑ / k", "↓ / j", "newer / older commit"],
    ["space", "", "keep the hovered ghost diff after the mouse leaves (sticky)"],
    ["⏎", "", "open the ghosted commit side by side"],
    ["p", "", "pin / unpin A: every later commit compares against it (older on the left)"],
    ["]", "[", "next / previous change block"],
    ["c", "", "checkout this commit, or restore just this file"],
    ["s", "f", "sync scroll · fold unchanged"],
    ["h", "r", "commit list · ribbons"],
    ["w", "esc", "wrap · release the ghost, unpin"],
  ]];

  function commands() {
    if (!H) return [];
    return [
      { id: "history.pin", group: "History", title: H.pinned === null ? "Pin this version as A" : "Unpin A", keys: "p", run: () => pin(H.cur) },
      { id: "history.newer", group: "History", title: "Newer commit", keys: "↑", run: () => select(H.cur + 1) },
      { id: "history.older", group: "History", title: "Older commit", keys: "↓", run: () => select(H.cur - 1) },
      { id: "history.fold", group: "History", title: H.foldAll ? "Unfold unchanged regions" : "Fold unchanged regions", keys: "f", run: () => foldAllRuns(!H.foldAll) },
      { id: "history.sync", group: "History", title: `${H.s.sync ? "Stop" : "Start"} syncing scroll`, keys: "s", run: toggleSync },
      { id: "history.ribbons", group: "History", title: `${H.s.ribbons ? "Hide" : "Show"} ribbons`, keys: "r", run: () => q("#h-ribbons").click() },
      { id: "history.wrap", group: "History", title: `${H.s.wrap ? "Unwrap" : "Wrap"} long lines`, keys: "w", run: () => q("#h-wrap").click() },
      { id: "history.side", group: "History", title: `${H.s.side ? "Hide" : "Show"} the commit list`, keys: "h", run: () => q("#h-side").click() },
      { id: "history.checkout", group: "History", title: `Checkout ${H.commits[H.cur]?.short || ""}`, keys: "c", run: () => onKey({ key: "c" }) },
    ];
  }

  try { KEY_HELP.push(HELP); } catch {}
  const profile = () => (H ? { ...H.prof } : null);
  window.History = { open, close, isOpen, parseHash, onKey, commands, KEY_HELP: HELP, select, pin, profile };
})();
