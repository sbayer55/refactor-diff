/* Second-round shared engine. Builds on history.js (data, diff, blame, highlight) and adds the
   behaviours every round-two design shares: a vertical commit list, line-following scroll,
   aligned side-by-side rendering with the older version on the left, hunk and context folds,
   synchronized scrolling, ghost previews that keep your place, and pinning A. */
(function () {
  const { commits, lines, stats, diff, blame, heat, highlight, short, initials, fmtDate, rel } = FH;
  const N = commits.length;

  // ---------- line mapping (follow the line you are on) ----------
  // Map a line index in version `from` to the nearest surviving line at or after it in version `to`.
  function mapLine(from, to, line) {
    if (from === to) return line;
    let last = 0;
    for (const o of diff(from, to)) if (o.t === 'eq') { if (o.a >= line) return o.b; last = o.b; }
    return last;
  }
  // A row's top in the scroller's content coordinates (fold wrappers are positioned, so offsetTop is not enough).
  function rowTop(scroller, el) { return el.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop; }
  // The row at the top of the viewport and its offset, so it can be put back after a re-render.
  function anchorOf(pane, scroller = pane) {
    const top = scroller.scrollTop;
    for (const r of pane.querySelectorAll('.row[data-line]')) {
      const y = rowTop(scroller, r);
      if (y + r.offsetHeight > top + 1) return { line: +r.dataset.line, offset: y - top };
    }
    return { line: 0, offset: 0 };
  }
  function restoreAnchor(pane, line, offset, side, scroller = pane) {
    const sel = side ? `.row[data-${side}="${line}"]` : `.row[data-line="${line}"]`;
    let r = pane.querySelector(sel);
    if (!r) { // the line is gone: take the next one that exists
      const rows = Array.from(pane.querySelectorAll(side ? `.row[data-${side}]` : '.row[data-line]'));
      r = rows.find(x => +(side ? x.dataset[side] : x.dataset.line) >= line) || rows[rows.length - 1];
    }
    if (r) scroller.scrollTop = Math.max(0, rowTop(scroller, r) - offset);
    return r;
  }
  // Keep a row at a fixed viewport offset while rows above it animate.
  function holdAnchor(scroller, row, offset, ms = 700) {
    const t0 = performance.now();
    const tick = () => { if (row.isConnected) scroller.scrollTop = Math.max(0, rowTop(scroller, row) - offset); if (performance.now() - t0 < ms) requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
  }

  // ---------- rows ----------
  function row(text, num, origin, version, kind, data) {
    const r = document.createElement('div');
    r.className = 'row' + (kind ? ' ' + kind : '');
    if (origin != null) r.style.setProperty('--heat', heat(origin, version));
    for (const k in (data || {})) r.dataset[k] = data[k];
    r.innerHTML = `<span class="age"></span><span class="num">${num ?? ''}</span><span class="src">${kind === 'pad' ? '' : highlight(text)}</span>`;
    return r;
  }

  // ---------- fold segments ----------
  // A run (unchanged lines) or a hunk (a change) with its fold button and its folded summary row.
  function segWrapper(type, id, n, add, del, what) {
    const w = document.createElement('div'); w.className = type + (type === 'run' && n < 4 ? ' short' : ''); w.dataset.id = id;
    if (type === 'run') w.innerHTML = `<button class="fold-btn" data-fold="${id}" title="Fold these unchanged lines">⌃ fold ${n} unchanged</button><div class="fold-row" data-unfold="${id}"><span class="age"></span><span class="num">⋯</span><span class="src">${n} unchanged lines · click to expand</span></div>`;
    else w.innerHTML = `<button class="hunk-btn" data-fold="${id}" title="Collapse this change">▾</button><div class="fold-row hunk-row" data-unfold="${id}"><span class="age"></span><span class="num">▸</span><span class="src">collapsed change · ${what || `<span class="stat-add">+${add}</span> <span class="stat-del">−${del}</span>`} · click to expand</span></div>`;
    return w;
  }
  // Wrap a pane's flat rows into runs and hunks so folds work in single and ghost modes too.
  function segment(pane, isChange, what) {
    const rows = Array.from(pane.querySelectorAll('.row')).filter(r => !r.classList.contains('fold-row'));
    const segs = []; let cur = null;
    for (const r of rows) { const t = isChange(r) ? 'hunk' : 'run'; if (!cur || cur.type !== t) { cur = { type: t, rows: [] }; segs.push(cur); } cur.rows.push(r); }
    const folded = new Set(Array.from(pane.querySelectorAll('.folded')).map(w => w.dataset.id));
    pane.replaceChildren();
    let ri = 0, hi = 0;
    for (const sg of segs) {
      const id = sg.type === 'run' ? 'r' + (ri++) : 'h' + (hi++);
      const add = sg.rows.filter(r => /(^| )(add|hl-add|ghost-add)( |$)/.test(r.className)).length, del = sg.rows.filter(r => /(^| )(del|ghost-del)( |$)/.test(r.className)).length;
      const w = segWrapper(sg.type, id, sg.rows.length, add, del, what ? what(sg.rows, add, del) : null);
      if (folded.has(id)) w.classList.add('folded');
      sg.rows.forEach(r => w.appendChild(r)); pane.appendChild(w);
    }
  }
  const singleWhat = (rows) => `${rows.length} line${rows.length === 1 ? '' : 's'} written in this commit`;

  // ---------- single version with animated morph and anchoring ----------
  function renderSingle(pane, v) {
    const bl = blame(v);
    const run = document.createElement('div'); run.className = 'run'; run.dataset.id = 'all';
    lines(v).forEach((t, i) => run.appendChild(row(t, i + 1, bl[i], v, '', { line: i })));
    pane.replaceChildren(run); pane.dataset.version = v; pane.dataset.mode = 'single';
    segment(pane, r => bl[+r.dataset.line] === v, singleWhat);
  }
  function morphTo(pane, to, opts = {}) {
    const sc = opts.scroller || pane;
    const from = pane.dataset.mode === 'single' ? +pane.dataset.version : null;
    if (from === null || from === to || opts.instant) {
      const a = anchorOf(pane, sc); const target = from === null ? a.line : mapLine(from, to, a.line);
      renderSingle(pane, to); restoreAnchor(pane, target, a.offset, null, sc); return;
    }
    const a = anchorOf(pane, sc), target = mapLine(from, to, a.line);
    const bl = blame(to), ops = diff(from, to);
    pane.querySelectorAll('.row.del, .row.leaving').forEach(r => r.remove());   // leftovers of an unfinished morph
    const old = Array.from(pane.querySelectorAll('.row[data-line]'));
    const run = document.createElement('div'); run.className = 'run'; run.dataset.id = 'all';
    const leaving = [], entering = [];
    for (const o of ops) {
      if (o.t === 'eq') { const r = old[o.a]; r.className = 'row'; r.dataset.line = o.b; r.querySelector('.num').textContent = o.b + 1; r.style.setProperty('--heat', heat(bl[o.b], to)); run.appendChild(r); }
      else if (o.t === 'del') { const r = old[o.a]; r.className = 'row del'; delete r.dataset.line; leaving.push(r); run.appendChild(r); }
      else { const r = row(o.text, o.b + 1, bl[o.b], to, 'add', { line: o.b }); r.style.height = '0px'; r.style.opacity = '0'; entering.push(r); run.appendChild(r); }
    }
    pane.replaceChildren(run); pane.dataset.version = to;
    const anchorRow = restoreAnchor(pane, target, a.offset, null, sc);
    if (anchorRow) holdAnchor(sc, anchorRow, a.offset, 720);
    const hold = 240;
    requestAnimationFrame(() => {
      entering.forEach(r => { r.style.height = ''; r.style.opacity = ''; });
      setTimeout(() => leaving.forEach(r => r.classList.add('leaving')), hold);
      setTimeout(() => {
        leaving.forEach(r => r.remove()); entering.forEach(r => { r.classList.remove('add'); r.classList.add('hl-add'); setTimeout(() => r.classList.remove('hl-add'), 900); });
        if (pane.dataset.mode === 'single' && +pane.dataset.version === to) { const a2 = anchorOf(pane, sc); segment(pane, r => bl[+r.dataset.line] === to, singleWhat); restoreAnchor(pane, a2.line, a2.offset, null, sc); pane.dispatchEvent(new CustomEvent('rerender', { bubbles: true })); }
      }, hold + 320);
    });
  }

  // ---------- aligned side-by-side (older on the left) ----------
  // Items: {kind, l:{text,num,line,origin}|null, r:{...}|null}. Runs of eq items and hunks of changes are segmented.
  function aligned(a, b) {
    const ba = blame(a), bb = blame(b), ops = diff(a, b), items = [];
    let i = 0;
    while (i < ops.length) {
      if (ops[i].t === 'eq') { const o = ops[i]; items.push({ kind: 'eq', l: { text: o.text, line: o.a, origin: ba[o.a] }, r: { text: o.text, line: o.b, origin: bb[o.b] } }); i++; continue; }
      const dels = [], adds = [];
      while (i < ops.length && ops[i].t !== 'eq') { (ops[i].t === 'del' ? dels : adds).push(ops[i]); i++; }
      const n = Math.max(dels.length, adds.length);
      for (let k = 0; k < n; k++) {
        const d = dels[k], ad = adds[k];
        items.push({ kind: 'chg', l: d ? { text: d.text, line: d.a, origin: ba[d.a], del: true } : null, r: ad ? { text: ad.text, line: ad.b, origin: bb[ad.b], add: true } : null });
      }
    }
    // segments
    const segs = []; let cur = null, hid = 0, rid = 0;
    for (const it of items) {
      const type = it.kind === 'eq' ? 'run' : 'hunk';
      if (!cur || cur.type !== type) { cur = { type, id: type === 'run' ? 'r' + (rid++) : 'h' + (hid++), items: [] }; segs.push(cur); }
      cur.items.push(it);
    }
    for (const s of segs) if (s.type === 'hunk') { s.del = s.items.filter(x => x.l).length; s.add = s.items.filter(x => x.r).length; }
    return segs;
  }
  function renderSide(pane, segs, side, version, otherVersion) {
    pane.replaceChildren(); pane.dataset.version = version; pane.dataset.mode = 'split';
    for (const s of segs) {
      const w = segWrapper(s.type, s.id, s.items.length, s.add, s.del);
      for (const it of s.items) {
        const c = side === 'l' ? it.l : it.r;
        if (!c) { w.appendChild(row('', '', null, version, 'pad')); continue; }
        const kind = c.del ? 'del' : c.add ? 'add' : '';
        w.appendChild(row(c.text, c.line + 1, c.origin, version, kind, { line: c.line }));
      }
      pane.appendChild(w);
    }
  }
  // Fold/unfold by segment id across every pane in a board.
  function wireFolds(board) {
    board.addEventListener('click', (e) => {
      const f = e.target.closest('[data-fold]'), u = e.target.closest('[data-unfold]');
      if (!f && !u) return;
      const id = (f || u).dataset.fold ?? (f || u).dataset.unfold;
      board.querySelectorAll(`[data-id="${id}"]`).forEach(w => w.classList.toggle('folded', !!f));
      board.dispatchEvent(new CustomEvent('foldchange'));
    });
  }
  function foldAllRuns(board, fold) { board.querySelectorAll('.run:not(.short)').forEach(w => w.classList.toggle('folded', fold)); board.dispatchEvent(new CustomEvent('foldchange')); }

  function syncScroll(l, r) {
    let lock = false; const h = (src, dst) => () => { if (lock || !l.dataset.sync) return; lock = true; dst.scrollTop = src.scrollTop; dst.scrollLeft = src.scrollLeft; requestAnimationFrame(() => lock = false); };
    l.addEventListener('scroll', h(l, r)); r.addEventListener('scroll', h(r, l));
  }

  // ---------- ghost: what a commit would do to the version you are reading, in place ----------
  function renderGhost(pane, cur, target, scroller = pane) {
    const a = anchorOf(pane, scroller);
    const forward = target > cur, lo = Math.min(cur, target), hi = Math.max(cur, target);
    const ops = diff(lo, hi), bl = blame(cur);
    const run = document.createElement('div'); run.className = 'run ghost'; run.dataset.id = 'all';
    for (const o of ops) {
      if (o.t === 'eq') { const line = forward ? o.a : o.b; run.appendChild(row(o.text, line + 1, bl[line], cur, '', { line })); continue; }
      const leaving = forward ? o.t === 'del' : o.t === 'add';
      const line = leaving ? (forward ? o.a : o.b) : null;
      run.appendChild(row(o.text, leaving ? line + 1 : '', null, cur, leaving ? 'ghost-del' : 'ghost-add', leaving ? { line } : {}));
    }
    pane.replaceChildren(run); pane.dataset.mode = 'ghost';
    segment(pane, r => /ghost-/.test(r.className));
    restoreAnchor(pane, a.line, a.offset, null, scroller);
  }

  // ---------- commit list ----------
  const maxChange = Math.max(...commits.map((_, i) => i ? stats(i - 1, i).add + stats(i - 1, i).del : lines(0).length));
  function commitList(el, h) {
    el.classList.add('clist');
    el.innerHTML = '';
    for (let i = N - 1; i >= 0; i--) {
      const c = commits[i], s = i ? stats(i - 1, i) : { add: lines(0).length, del: 0 };
      const it = document.createElement('div'); it.className = 'citem' + (c.pr ? ' pr' : ''); it.dataset.i = i; it.tabIndex = 0;
      it.innerHTML = `<span class="dot"></span><span class="bar"><i class="d" style="width:${s.del / maxChange * 100}%"></i><i class="a" style="width:${s.add / maxChange * 100}%"></i></span>
        <span class="msg">${c.message}</span>
        <span class="meta"><span class="hash">${short(c.sha)}</span> · ${initials(c.author)} · ${rel(c.date)} ${c.tag ? `<span class="chip ${c.tag}">${c.tag}</span>` : ''}</span>
        <span class="delta"><span class="stat-add">+${s.add}</span> <span class="stat-del">−${s.del}</span></span>
        <span class="pinmark" title="Pinned as A">A</span>
        <button class="btn mini pinbtn" data-pin="${i}" title="Pin as A (p)">pin</button>`;
      it.addEventListener('mouseenter', () => h.hover?.(i)); it.addEventListener('mouseleave', () => h.leave?.(i));
      it.addEventListener('click', (e) => { if (e.target.closest('[data-pin]')) { h.pin?.(i); return; } h.select?.(i); });
      el.appendChild(it);
    }
    return {
      paint(state) {
        el.querySelectorAll('.citem').forEach(it => {
          const i = +it.dataset.i;
          it.classList.toggle('cur', i === state.cur); it.classList.toggle('pinned', i === state.pinned);
          it.classList.toggle('range', state.pinned !== null && i > Math.min(state.pinned, state.cur) && i < Math.max(state.pinned, state.cur));
          const mark = it.querySelector('.this'); if (i === state.cur && !mark) it.querySelector('.hash').insertAdjacentHTML('afterend', '<span class="this"> (this commit)</span>'); else if (i !== state.cur && mark) mark.remove();
          it.querySelector('.pinbtn').textContent = i === state.pinned ? 'unpin' : 'pin';
        });
        el.querySelector('.citem.cur')?.scrollIntoView({ block: 'nearest' });
      }
    };
  }

  // ---------- pane header: the version in view, checkout, pin ----------
  function header(el, i, state, h) {
    const c = commits[i];
    const label = state.pinned === null ? '' : i === state.pinned ? '<span class="ab a">A</span>' : '<span class="ab b">B</span>';
    el.innerHTML = `${label}<span class="sha">${short(c.sha)}${i === state.cur ? ' <i class="this">(this commit)</i>' : ''}</span>${c.tag ? `<span class="chip ${c.tag}">${c.tag}</span>` : ''}${c.pr ? '<span class="chip pr">PR</span>' : ''}<span class="msg" title="${c.message}">${c.message}</span><span class="who"><span class="avatar">${initials(c.author)}</span> ${c.author} · ${fmtDate(c.date)}</span>
      <span class="hdr-actions">${h.pin ? `<button class="btn" data-act="pin">${i === state.pinned ? 'Unpin A' : 'Pin as A'} <kbd>p</kbd></button>` : ''}<button class="btn primary" data-act="co">Checkout</button></span>`;
    el.querySelector('[data-act="co"]').onclick = () => FH.checkoutMenu(null, i);
    const pb = el.querySelector('[data-act="pin"]'); if (pb) pb.onclick = () => h.pin(i);
  }

  // ---------- minimap ----------
  function minimap(el, pane, opts = {}) {
    const draw = () => {
      const rows = Array.from(pane.querySelectorAll('.row:not(.fold-row)')).filter(r => r.offsetParent !== null || r.closest('.folded') === null);
      const total = pane.scrollHeight || 1, H = el.clientHeight;
      el.innerHTML = '';
      const frag = document.createDocumentFragment();
      for (const r of rows) {
        if (r.closest('.folded')) continue;
        const i = document.createElement('i');
        const kind = r.classList.contains('add') || r.classList.contains('hl-add') || r.classList.contains('ghost-add') ? 'add' : r.classList.contains('del') || r.classList.contains('hl-del') || r.classList.contains('ghost-del') ? 'del' : r.classList.contains('pad') ? 'pad' : '';
        i.className = kind; i.style.top = (rowTop(pane, r) / total * H) + 'px'; i.style.height = Math.max(1, r.offsetHeight / total * H) + 'px';
        if (!kind) i.style.background = r.style.getPropertyValue('--heat') || 'var(--heat-old)';
        frag.appendChild(i);
      }
      const vp = document.createElement('b'); vp.className = 'vp'; vp.style.top = (pane.scrollTop / total * H) + 'px'; vp.style.height = (pane.clientHeight / total * H) + 'px';
      frag.appendChild(vp); el.appendChild(frag);
    };
    pane.addEventListener('scroll', () => { const vp = el.querySelector('.vp'); if (!vp) return; const total = pane.scrollHeight || 1, H = el.clientHeight; vp.style.top = (pane.scrollTop / total * H) + 'px'; vp.style.height = (pane.clientHeight / total * H) + 'px'; });
    el.addEventListener('pointerdown', (e) => { const go = (ev) => { const r = el.getBoundingClientRect(); pane.scrollTop = (ev.clientY - r.top) / r.height * pane.scrollHeight - pane.clientHeight / 2; }; go(e); el.setPointerCapture(e.pointerId); el.onpointermove = go; el.onpointerup = () => el.onpointermove = null; });
    return { draw };
  }

  // ---------- blame tip over the gutter ----------
  // Rows carry their own version through the pane (dataset.version). In ghost mode the rows are the current version.
  // `onLine` (optional) hears the blame of whatever line is under the mouse, or null when the mouse leaves the code.
  function blameTips(root, versionOf, onJump, onLine) {
    let tip = document.querySelector('.blame-tip');
    if (!tip) { tip = document.createElement('div'); tip.className = 'tip blame-tip'; document.body.appendChild(tip); }
    const isGutter = (el) => el.classList.contains('age') || el.classList.contains('num');
    const originOf = (r) => { const pane = r.closest('.code-pane'); if (!pane || r.dataset.line === undefined) return null; const v = versionOf(pane); if (v == null) return null; const o = blame(v)[+r.dataset.line]; return o == null ? null : { o, v }; };
    let lastRow = null;
    root.addEventListener('mouseover', (e) => {
      const r = e.target.closest('.row'); if (!r || r.classList.contains('fold-row')) return;
      if (r !== lastRow) { lastRow = r; const b = originOf(r); onLine?.(b ? { line: +r.dataset.line + 1, commit: commits[b.o], index: b.o, own: b.o === b.v } : null); }
      if (!isGutter(e.target)) { tip.classList.remove('show'); return; }
      const b = originOf(r); if (!b) return;
      const c = commits[b.o];
      tip.innerHTML = `<b>${short(c.sha)}${b.o === b.v ? ' <i>(this commit)</i>' : ''}</b>${c.message}<div class="muted">${c.author} · ${fmtDate(c.date)} · ${rel(c.date)}${b.o !== b.v ? ' · click to jump' : ''}</div>`;
      const box = r.getBoundingClientRect(); tip.style.left = Math.min(box.left + 140, window.innerWidth - 20) + 'px'; tip.style.top = (box.top - 6) + 'px'; tip.classList.add('show');
    });
    root.addEventListener('mouseout', (e) => {
      if (isGutter(e.target)) tip.classList.remove('show');
      const to = e.relatedTarget; if (!to || !root.contains(to) || !to.closest?.('.row')) { lastRow = null; onLine?.(null); }
    });
    root.addEventListener('click', (e) => {
      if (!isGutter(e.target)) return;
      const r = e.target.parentElement, pane = r.closest('.code-pane'); if (!pane || r.dataset.line === undefined) return;
      const v = versionOf(pane); if (v == null) return;
      const o = blame(v)[+r.dataset.line]; tip.classList.remove('show');
      if (o === v) { r.classList.add('focus'); setTimeout(() => r.classList.remove('focus'), 900); return; }
      onJump(o, r.querySelector('.src').textContent);
    });
  }

  // ---------- keys shared by every design ----------
  function keys(h) {
    document.addEventListener('keydown', (e) => {
      if (e.target.matches?.('input, textarea')) return;
      const k = e.key;
      if (k === 'ArrowUp' || k === 'k') { e.preventDefault(); h.newer?.(); }
      else if (k === 'ArrowDown' || k === 'j') { e.preventDefault(); h.older?.(); }
      else if (k === 'p') h.pin?.(); else if (k === 'c') h.checkout?.(); else if (k === 'Enter') h.enter?.();
      else if (k === 's') h.sync?.(); else if (k === 'Escape') h.escape?.(); else if (k === 'f') h.fold?.();
      else if (k === 'Home') h.first?.(); else if (k === 'End') h.last?.();
    });
  }

  window.FH2 = { N, mapLine, rowTop, anchorOf, restoreAnchor, holdAnchor, row, segWrapper, segment, singleWhat, renderSingle, morphTo, aligned, renderSide, wireFolds, foldAllRuns, syncScroll, renderGhost, commitList, header, minimap, blameTips, keys };
})();
