"use strict";

// The pull request's conversation on GitHub: review threads inline under the lines they are
// on, the Comments explorer tab, the #conversation view with the timeline, and the pending
// review (inline comments collected locally, then submitted as Comment / Approve / Request
// changes). Loaded after app.js and uses its helpers (state, api, esc, toast, confirmDialog...).

const PR = (() => {
  // {conv, draft, error, loading, ver}; conv is /api/report/{id}/pr/conversation.
  let S = { conv: null, draft: { comments: [] }, error: null, loading: false, ver: 0 };
  let ver = 0;

  const url = (route) => `/api/report/${state.report.id}/pr/${route}`;
  const active = () => Boolean(state.report && prNumber());
  const EVENTS = {
    COMMENT: { label: "Comment", verb: "Submit review", done: "Review submitted" },
    APPROVE: { label: "Approve", verb: "Approve", done: "Approved" },
    REQUEST_CHANGES: { label: "Request changes", verb: "Request changes", done: "Changes requested" },
  };
  const STATE_LABEL = {
    APPROVED: ["approved", "ok"],
    CHANGES_REQUESTED: ["requested changes", "attention"],
    COMMENTED: ["commented", ""],
    DISMISSED: ["dismissed", ""],
  };

  function rel(d) {
    if (!d) return "";
    const mins = Math.round((Date.now() - new Date(d)) / 6e4);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    if (mins < 60 * 24) return `${Math.round(mins / 60)}h ago`;
    const days = Math.round(mins / 1440);
    if (days < 30) return `${days}d ago`;
    return new Date(d).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
  }

  // GitHub's bodyHTML is already sanitized; this keeps only plain formatting anyway, since it
  // goes straight into the page.
  const DROP = "script,style,iframe,frame,object,embed,form,input,button,textarea,select,link,meta,base,svg,math,template,video,audio";
  const KEEP_ATTRS = new Set(["href", "src", "alt", "title", "colspan", "rowspan", "open", "align", "lang", "width", "height"]);
  function safeHtml(html) {
    const doc = new DOMParser().parseFromString(`<div>${html || ""}</div>`, "text/html");
    const root = doc.body.firstElementChild;
    for (const el of root.querySelectorAll(DROP)) el.remove();
    for (const el of root.querySelectorAll("*")) {
      for (const a of [...el.attributes]) {
        const v = a.value.trim();
        const okUrl = /^(https?:|mailto:|#)/i.test(v);
        if (!KEEP_ATTRS.has(a.name) || ((a.name === "href" || a.name === "src") && !okUrl)) el.removeAttribute(a.name);
      }
      if (el.tagName === "A") { el.target = "_blank"; el.rel = "noopener noreferrer"; }
    }
    return root.innerHTML;
  }

  // One line of a comment's text, for snippets (from the rendered HTML, so no Markdown syntax).
  function snippet(c, n = 120) {
    const text = c.body_html ? new DOMParser().parseFromString(c.body_html, "text/html").body.textContent : c.body;
    return (text || "").replace(/\s+/g, " ").trim().slice(0, n);
  }

  function bodyHtml(c) {
    return c.body_html ? safeHtml(c.body_html) : `<p>${esc(c.body)}</p>`;
  }

  // ---------- loading ----------

  async function load() {
    if (!active()) { S = { conv: null, draft: { comments: [] }, error: null, loading: false, ver: ++ver }; refresh(); return; }
    const id = state.report.id;
    S.loading = true;
    try {
      const conv = await api(url("conversation"));
      if (state.report?.id !== id) return;
      S = { conv, draft: conv.draft || { comments: [] }, error: null, loading: false, ver: ++ver };
    } catch (e) {
      if (state.report?.id !== id) return;
      // The draft is local: still show it when GitHub can't be reached.
      const draft = await api(url("draft")).catch(() => ({ comments: [] }));
      S = { conv: null, draft, error: e.message, loading: false, ver: ++ver };
    }
    refresh();
  }

  function setDraft(draft) {
    S = { ...S, draft, ver: ++ver };
    refresh();
  }

  // Re-render everything that shows PR state.
  function refresh() {
    renderExplorer();
    applyThreads(true);
    if (location.hash === "#conversation") render();
    if (location.hash === "#conversation") setViewTitle("conversation");
    updateCommentBoxMeta();
  }

  // ---------- where threads go ----------

  // A thread's file as the UI names it (the new path), for a LEFT thread on a renamed file too.
  function uiPath(path) {
    const f = state.report.files.find((x) => x.path === path || x.old_path === path);
    return f ? f.path : path;
  }

  // "path|o|12" / "path|n|12" -> {threads, drafts} for the lines threads sit on.
  function index() {
    const out = new Map();
    const add = (path, side, line, kind, item) => {
      if (!line) return;
      const key = `${uiPath(path)}|${side === "LEFT" ? "o" : "n"}|${line}`;
      if (!out.has(key)) out.set(key, { threads: [], drafts: [] });
      out.get(key)[kind].push(item);
    };
    for (const t of S.conv?.threads || []) if (!t.outdated) add(t.path, t.side, t.line, "threads", t);
    for (const d of S.draft?.comments || []) add(d.path, d.side, d.line, "drafts", d);
    return out;
  }

  // Insert a full-width row of thread cards under each diff row that has threads. Idempotent
  // (a row whose cards are current is left alone) so the MutationObserver that calls it
  // settles; `force` rebuilds every row after the threads changed.
  function applyThreads(force = false) {
    const content = $("#content");
    if (force) for (const r of content.querySelectorAll("tr.thread-row")) r.remove();
    if (!active()) return;
    const idx = index();
    if (!idx.size) return;
    const rows = new Map();
    for (const cell of content.querySelectorAll("td[data-p][data-s][data-l]")) {
      const key = `${cell.dataset.p}|${cell.dataset.s}|${cell.dataset.l}`;
      if (!idx.has(key)) continue;
      const tr = cell.closest("tr");
      if (!tr || tr.closest(".gh-band")) continue;
      if (!rows.has(tr)) rows.set(tr, []);
      rows.get(tr).push(key);
    }
    for (const [tr, keys] of rows) {
      const sig = `${S.ver}:${keys.join(",")}`;
      const next = tr.nextElementSibling;
      if (next?.classList.contains("thread-row")) {
        if (next.dataset.sig === sig) continue;
        next.remove();
      }
      const threads = keys.flatMap((k) => idx.get(k).threads);
      const drafts = keys.flatMap((k) => idx.get(k).drafts);
      const cols = [...tr.cells].reduce((n, td) => n + td.colSpan, 0);
      tr.insertAdjacentHTML("afterend", `<tr class="thread-row" data-sig="${esc(sig)}"><td colspan="${cols}"><div class="gh-band">${
        threads.map((t) => threadHtml(t)).join("")}${drafts.map(draftHtml).join("")}</div></td></tr>`);
    }
  }

  // ---------- cards ----------

  function commentHtml(c, kind) {
    const actions = [
      c.can_edit ? `<button type="button" class="link muted" data-pr="edit" data-kind="${kind}" data-id="${c.database_id}">Edit</button>` : "",
      c.can_delete ? `<button type="button" class="link muted" data-pr="delete" data-kind="${kind}" data-id="${c.database_id}">Delete</button>` : "",
      c.url ? `<a class="link muted" href="${esc(c.url)}" target="_blank" rel="noopener">GitHub ↗</a>` : "",
    ].join("");
    return `<div class="gh-comment" data-comment="${c.database_id}">
      <div class="gh-meta">${avatar(c)}<b>${esc(c.author || "ghost")}</b><span class="when" title="${esc(c.created_at)}">${rel(c.created_at)}</span>${
        c.pending ? `<span class="badge warn">pending on GitHub</span>` : ""}<span class="spacer"></span>${actions}</div>
      <div class="gh-body">${bodyHtml(c)}</div>
    </div>`;
  }

  function avatar(c) {
    return c.avatar_url ? `<img class="gh-avatar" src="${esc(c.avatar_url)}&s=40" alt="" loading="lazy">` : `<span class="gh-avatar"></span>`;
  }

  function threadHtml(t, { where = false } = {}) {
    const first = t.comments[0];
    const line = t.line || t.original_line;
    const head = `<div class="gh-thread-head">
      <span class="gh-icon">💬</span>
      ${where ? `<span class="code">${esc(t.path)}:${line || "?"}</span>` : ""}
      <span>${plural(t.comments.length, "comment")}</span>
      ${t.resolved ? `<span class="badge ok">resolved${t.resolved_by ? ` by ${esc(t.resolved_by)}` : ""}</span>` : ""}
      ${t.outdated ? `<span class="badge">outdated</span>` : ""}
      ${t.resolved && first ? `<span class="gh-snippet">${esc(`${first.author || "ghost"}: ${snippet(first)}`)}</span>` : ""}
      <span class="spacer"></span>
      ${t.resolved ? `<button type="button" class="link" data-pr="toggle">Show</button>` : ""}
      ${!t.resolved && t.can_resolve ? `<button type="button" class="link" data-pr="resolve">Resolve</button>` : ""}
      ${t.resolved && t.can_unresolve ? `<button type="button" class="link" data-pr="unresolve">Unresolve</button>` : ""}
    </div>`;
    const last = t.comments[t.comments.length - 1];
    return `<div class="gh-thread${t.resolved ? " resolved collapsed" : ""}" data-thread="${esc(t.id)}" id="thread-${esc(t.id)}">
      ${head}
      <div class="gh-comments">${t.comments.map((c) => commentHtml(c, "review")).join("")}
        ${t.can_reply && last ? `<div class="gh-reply"><button type="button" class="link" data-pr="reply" data-id="${first.database_id}">Reply…</button></div>` : ""}
      </div>
    </div>`;
  }

  function draftHtml(d) {
    return `<div class="gh-thread draft" data-draft="${esc(d.id)}">
      <div class="gh-thread-head"><span class="badge warn">pending</span><span>Your review comment</span>
        <span class="meta">not posted yet</span><span class="spacer"></span>
        <button type="button" class="link" data-pr="draft-edit" data-id="${esc(d.id)}">Edit</button>
        <button type="button" class="link" data-pr="draft-remove" data-id="${esc(d.id)}">Remove</button></div>
      <div class="gh-comments"><div class="gh-comment"><div class="gh-body plain">${esc(d.body)}</div></div></div>
    </div>`;
  }

  // An inline editor in place of `el`: resolves to the text on Save, or null on Cancel.
  function editor(el, { text = "", placeholder = "", ok = "Save" } = {}) {
    return new Promise((resolve) => {
      const box = document.createElement("div");
      box.className = "gh-editor";
      box.innerHTML = `<textarea spellcheck="true" placeholder="${esc(placeholder)}"></textarea>
        <div class="comment-actions"><span class="meta" data-note></span><span class="spacer"></span>
          <button type="button" class="toggle" data-cancel>Cancel</button>
          <button type="button" class="primary" data-ok>${esc(ok)}</button></div>`;
      const ta = box.querySelector("textarea");
      ta.value = text;
      el.hidden = true;
      el.after(box);
      ta.focus();
      const done = (v) => { box.remove(); el.hidden = false; resolve(v); };
      box.querySelector("[data-cancel]").addEventListener("click", () => done(null));
      box.querySelector("[data-ok]").addEventListener("click", () => {
        const v = ta.value.trim();
        if (!v) { ta.focus(); return; }
        done(v);
      });
      ta.addEventListener("keydown", (e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") box.querySelector("[data-ok]").click();
        if (e.key === "Escape") { e.stopPropagation(); done(null); }
      });
    });
  }

  function findComment(kind, id) {
    const c = S.conv;
    if (!c) return null;
    const pool = kind === "issue" ? c.comments : c.threads.flatMap((t) => t.comments);
    return pool.find((x) => x.database_id === id) || null;
  }

  async function act(fn, ok) {
    try {
      const res = await fn();
      if (ok) toast(typeof ok === "function" ? ok(res) : ok);
      return res;
    } catch (e) {
      toast(e.message, { error: true });
      return null;
    }
  }

  // Clicks on [data-pr] buttons in the diff and the conversation view.
  async function onClick(e) {
    const btn = e.target.closest("[data-pr]");
    if (!btn || !active()) return;
    e.preventDefault();
    const thread = btn.closest("[data-thread]");
    const op = btn.dataset.pr;
    const id = Number(btn.dataset.id);
    switch (op) {
      case "toggle": {
        thread.classList.toggle("collapsed");
        btn.textContent = thread.classList.contains("collapsed") ? "Show" : "Hide";
        break;
      }
      case "resolve":
      case "unresolve": {
        if (!(await confirmPosting())) return;
        btn.disabled = true;
        const res = await act(() => api(url("resolve"), { thread_id: thread.dataset.thread, resolved: op === "resolve" }),
          op === "resolve" ? "Thread resolved." : "Thread reopened.");
        if (res) await load(); else btn.disabled = false;
        break;
      }
      case "reply": {
        const text = await editor(btn.parentElement, { placeholder: "Reply…", ok: "Reply" });
        if (!text || !(await confirmPosting())) return;
        if (await act(() => api(url("reply"), { comment_id: id, body: text }), "Reply posted.")) await load();
        break;
      }
      case "edit": {
        const c = findComment(btn.dataset.kind, id);
        const body = btn.closest(".gh-comment, .gh-entry").querySelector(".gh-body");
        const text = await editor(body, { text: c?.body || "" });
        if (!text || !(await confirmPosting())) return;
        if (await act(() => api(url("comment/edit"), { kind: btn.dataset.kind, id, body: text }), "Comment updated.")) await load();
        break;
      }
      case "delete": {
        const ok = await confirmDialog({ title: "Delete this comment?", message: "It is removed from the pull request on GitHub for everyone. This can't be undone.", ok: "Delete" });
        if (!ok) return;
        if (await act(() => api(url("comment/delete"), { kind: btn.dataset.kind, id }), "Comment deleted.")) await load();
        break;
      }
      case "draft-edit": {
        const d = S.draft.comments.find((x) => x.id === btn.dataset.id);
        const body = btn.closest("[data-draft]").querySelector(".gh-body");
        const text = await editor(body, { text: d?.body || "" });
        if (!text) return;
        const draft = await act(() => api(url("draft/comment"), { id: btn.dataset.id, body: text }));
        if (draft) setDraft(draft);
        break;
      }
      case "draft-remove": {
        const draft = await act(() => api(url("draft/comment/delete"), { id: btn.dataset.id }));
        if (draft) setDraft(draft);
        break;
      }
      case "submit": submit(btn.closest("form")); break;
      case "discard": {
        const n = S.draft.comments.length;
        const ok = await confirmDialog({ title: "Discard the pending review?", message: `${plural(n, "pending comment")} will be thrown away. Nothing was posted yet.`, ok: "Discard" });
        if (!ok) return;
        const draft = await act(() => api(url("draft/discard"), {}), "Pending review discarded.");
        if (draft) setDraft(draft);
        break;
      }
      case "new-comment": {
        const ta = $("#gh-new-comment");
        const text = ta.value.trim();
        if (!text) { ta.focus(); return; }
        if (!(await confirmPosting())) return;
        btn.disabled = true;
        if (await act(() => api(url("comment"), { body: text }), (r) => `Posted: ${r.url}`)) { ta.value = ""; await load(); }
        btn.disabled = false;
        break;
      }
      case "refresh": await load(); toast("Pull request comments refreshed."); break;
      case "goto-draft": {
        const d = S.draft.comments.find((x) => x.id === btn.dataset.id);
        if (d) location.hash = fileHref(uiPath(d.path), "diff", `${d.side === "LEFT" ? "o" : "n"}${d.line}`);
        break;
      }
    }
  }

  // ---------- adding to the review from the comment box ----------

  async function addToReview(hunkId, body) {
    const draft = await api(url("draft/comment"), { body, hunk_id: hunkId });
    setDraft(draft);
    return draft;
  }

  function pendingCount() {
    return S.draft?.comments?.length || 0;
  }

  function updateCommentBoxMeta() {
    const el = $("#comment-box [data-pending]");
    if (el) el.innerHTML = pendingSummary();
  }

  function pendingSummary() {
    const n = pendingCount();
    return n ? `${plural(n, "pending comment")} · <a href="#conversation">Submit review…</a>` : "";
  }

  // ---------- submitting ----------

  async function submit(form, preset) {
    const event = preset || form?.querySelector("[name=gh-event]:checked")?.value || "COMMENT";
    const body = (form?.querySelector("textarea")?.value || "").trim();
    const n = pendingCount();
    const what = EVENTS[event];
    const ok = await confirmDialog({
      title: `${what.verb} on pull request #${prNumber()}?`,
      message: `Posts your review${n ? ` with ${plural(n, "inline comment")}` : ""}${body ? " and its summary" : ""} to GitHub as ${S.conv?.viewer || "you"}.`,
      ok: what.verb,
    });
    if (!ok) return;
    const res = await act(() => api(url("review"), { event, body }), (r) => `${what.done}: ${r.url}`);
    if (res) {
      if (form) form.reset();
      await load();
    }
  }

  // Palette shortcuts: open the form with the event preselected.
  function startReview(event = "COMMENT") {
    if (location.hash !== "#conversation") location.hash = "#conversation";
    else render();
    requestAnimationFrame(() => {
      const r = $(`#content [name=gh-event][value="${event}"]`);
      if (r && !r.disabled) r.checked = true;
      $("#gh-review-body")?.focus();
    });
  }

  // ---------- explorer tab ----------

  function count() {
    const unresolved = (S.conv?.threads || []).filter((t) => !t.resolved).length;
    return unresolved + pendingCount();
  }

  function explorer(activeCls) {
    if (!active()) return `<div class="node dim"><span class="label sans">Not a pull request</span></div>`;
    let html = `<a class="node nav${activeCls("#conversation")}" href="#conversation"><span class="label sans"><strong>Conversation</strong></span>
      <span class="pill">${S.conv ? S.conv.comments.length + S.conv.reviews.filter((r) => r.body).length : "…"}</span></a>`;
    if (S.error) html += `<div class="node dim"><span class="label sans">Couldn't load comments: ${esc(S.error)}</span></div>`;
    else if (!S.conv) html += `<div class="node dim"><span class="label sans"><span class="spinner"></span> Loading comments…</span></div>`;
    const drafts = S.draft?.comments || [];
    if (drafts.length) {
      html += `<div class="hd"><span>pending review</span><span>${drafts.length}</span></div>`;
      for (const d of drafts) {
        html += `<a class="node nav" href="${fileHref(uiPath(d.path), "diff", `${d.side === "LEFT" ? "o" : "n"}${d.line}`)}" title="${esc(d.body)}">
          <span class="badge warn">✎</span><span class="label sans">${esc(basename(d.path))}:${d.line} · ${esc(d.body.slice(0, 60))}</span></a>`;
      }
    }
    const threads = [...(S.conv?.threads || [])].sort((a, b) => a.resolved - b.resolved || a.path.localeCompare(b.path) || (a.line || a.original_line || 0) - (b.line || b.original_line || 0));
    if (threads.length) {
      html += `<div class="hd"><span>threads</span><span>${threads.filter((t) => !t.resolved).length} open</span></div>`;
      for (const t of threads) {
        const c = t.comments[0];
        const line = t.line || t.original_line;
        const href = t.outdated || !t.line ? "#conversation" : fileHref(uiPath(t.path), "diff", `${t.side === "LEFT" ? "o" : "n"}${t.line}`);
        html += `<a class="node nav${t.resolved ? " dim" : ""}" href="${href}" ${t.outdated || !t.line ? `data-thread-scroll="${esc(t.id)}"` : ""} title="${esc(c ? snippet(c, 300) : "")}">
          <span class="badge${t.resolved ? " ok" : ""}">${t.comments.length}</span>
          <span class="label sans">${esc(basename(t.path))}:${line || "?"}${t.outdated ? " (outdated)" : ""} · ${esc(c?.author || "ghost")}: ${esc(c ? snippet(c, 60) : "")}</span></a>`;
      }
    } else if (S.conv) {
      html += `<div class="node dim"><span class="label sans">No review threads yet</span></div>`;
    }
    return html;
  }

  // ---------- the #conversation view ----------

  function reviewersHtml(c) {
    // Each reviewer's latest decisive state (a later comment doesn't undo an approval).
    const latest = new Map();
    for (const r of c.reviews) {
      if (!r.author || r.author === c.author) continue;
      const prev = latest.get(r.author);
      if (r.state !== "COMMENTED" || !prev) latest.set(r.author, r);
    }
    if (!latest.size) return "";
    return `<div class="gh-reviewers">${[...latest.values()].map((r) => {
      const [label, cls] = STATE_LABEL[r.state] || [r.state.toLowerCase(), ""];
      return `<span class="gh-reviewer">${avatar(r)}<b>${esc(r.author)}</b> <span class="pill ${cls}">${label}</span></span>`;
    }).join("")}</div>`;
  }

  function decisionPill(c) {
    const d = { APPROVED: ["Approved", "ok"], CHANGES_REQUESTED: ["Changes requested", "attention"], REVIEW_REQUIRED: ["Review required", "warn"] }[c.review_decision];
    return d ? `<span class="pill ${d[1]}">${d[0]}</span>` : "";
  }

  function reviewFormHtml(c) {
    const n = pendingCount();
    const own = c?.viewer_is_author;
    const radio = (ev, label, hint) => `<label class="gh-radio${own && ev !== "COMMENT" ? " disabled" : ""}" title="${esc(own && ev !== "COMMENT" ? "You can't approve or request changes on your own pull request." : hint)}">
      <input type="radio" name="gh-event" value="${ev}" ${ev === "COMMENT" ? "checked" : ""} ${own && ev !== "COMMENT" ? "disabled" : ""}> ${label}</label>`;
    return `<form class="card gh-review-form" onsubmit="return false">
      <h3>Finish your review</h3>
      ${c?.pending_review ? `<p class="gh-warn">You started a review on GitHub that isn't submitted yet. GitHub allows one pending review at a time: submit or discard it there before submitting this one.</p>` : ""}
      <textarea id="gh-review-body" placeholder="Leave a summary (required to request changes)"></textarea>
      <div class="gh-radios">
        ${radio("COMMENT", "Comment", "General feedback without explicit approval.")}
        ${radio("APPROVE", "Approve", "Give your approval to merge these changes.")}
        ${radio("REQUEST_CHANGES", "Request changes", "Feedback that must be addressed before merging.")}
      </div>
      <div class="comment-actions">
        <span class="meta">${n ? `${plural(n, "pending inline comment")} will be posted with it` : "No inline comments pending: press <kbd>c</kbd> on a hunk to add one"}${own ? " · you're the author, so only Comment is available" : ""}</span>
        <span class="spacer"></span>
        ${n ? `<button type="button" class="toggle" data-pr="discard">Discard pending</button>` : ""}
        <button type="button" class="primary" data-pr="submit">Submit review</button>
      </div>
    </form>`;
  }

  function timeline(c) {
    const items = [
      ...c.comments.map((x) => ({ at: x.created_at, html: entryHtml(x, "issue") })),
      ...c.reviews.filter((r) => r.body || r.state !== "COMMENTED").map((r) => ({ at: r.submitted_at, html: reviewEntryHtml(r) })),
    ].sort((a, b) => String(a.at).localeCompare(String(b.at)));
    return items.length ? items.map((x) => x.html).join("") : `<p class="dim">No comments on the conversation yet.</p>`;
  }

  function entryHtml(x, kind) {
    return `<div class="gh-entry card">
      <div class="gh-meta">${avatar(x)}<b>${esc(x.author || "ghost")}</b><span class="when" title="${esc(x.created_at)}">commented ${rel(x.created_at)}</span><span class="spacer"></span>
        ${x.can_edit ? `<button type="button" class="link muted" data-pr="edit" data-kind="${kind}" data-id="${x.database_id}">Edit</button>` : ""}
        ${x.can_delete ? `<button type="button" class="link muted" data-pr="delete" data-kind="${kind}" data-id="${x.database_id}">Delete</button>` : ""}
        ${x.url ? `<a class="link muted" href="${esc(x.url)}" target="_blank" rel="noopener">GitHub ↗</a>` : ""}</div>
      <div class="gh-body">${bodyHtml(x)}</div>
    </div>`;
  }

  function reviewEntryHtml(r) {
    const [label, cls] = STATE_LABEL[r.state] || [r.state.toLowerCase(), ""];
    return `<div class="gh-entry card review">
      <div class="gh-meta">${avatar(r)}<b>${esc(r.author || "ghost")}</b> <span class="pill ${cls}">${label}</span><span class="when" title="${esc(r.submitted_at || "")}">${rel(r.submitted_at)}</span><span class="spacer"></span>
        ${r.url ? `<a class="link muted" href="${esc(r.url)}" target="_blank" rel="noopener">GitHub ↗</a>` : ""}</div>
      ${r.body ? `<div class="gh-body">${bodyHtml(r)}</div>` : ""}
    </div>`;
  }

  function render() {
    const content = $("#content");
    if (!active()) {
      content.innerHTML = `<div class="empty"><p>The conversation is available when reviewing a pull request.</p></div>`;
      return;
    }
    const c = S.conv;
    if (!c && !S.error) {
      content.innerHTML = `<div class="empty"><span class="spinner"></span> Loading the conversation…</div>`;
      if (!S.loading) load();
      return;
    }
    const pr = state.report.source.pr;
    const drafts = S.draft?.comments || [];
    const loose = (c?.threads || []).filter((t) => !t.hunk_id);
    content.innerHTML = `<div class="gh-conv">
      <div class="page-head">
        <h2>${esc(pr.title || "")} <span class="meta">#${pr.number}</span></h2>
        <p>${c ? `${c.author ? `by <b>${esc(c.author)}</b> · ` : ""}${decisionPill(c)} <a href="${esc(c.url)}" target="_blank" rel="noopener">Open on GitHub ↗</a>` : ""}
          <button type="button" class="link" data-pr="refresh">Refresh</button></p>
        ${c ? reviewersHtml(c) : ""}
      </div>
      ${S.error ? `<div class="banner error">Couldn't load the conversation from GitHub: ${esc(S.error)}</div>` : ""}
      ${reviewFormHtml(c)}
      ${drafts.length ? `<h3 class="gh-h">Pending review <span class="meta">${drafts.length}</span></h3>
        ${drafts.map((d) => `<div class="gh-loose"><button type="button" class="link code" data-pr="goto-draft" data-id="${esc(d.id)}">${esc(d.path)}:${d.line}</button>${draftHtml(d)}</div>`).join("")}` : ""}
      ${c ? `<h3 class="gh-h">Conversation</h3>${timeline(c)}
        <div class="card gh-new"><textarea id="gh-new-comment" placeholder="Add a comment to the conversation…"></textarea>
          <div class="comment-actions"><span class="spacer"></span><button type="button" class="primary" data-pr="new-comment">Comment</button></div></div>` : ""}
      ${loose.length ? `<h3 class="gh-h">Threads not shown in this diff <span class="meta">${loose.length}</span></h3>
        <p class="dim">Outdated threads, and threads on lines outside the analyzed hunks.</p>
        ${loose.map((t) => threadHtml(t, { where: true })).join("")}` : ""}
    </div>`;
    if (state.pendingScroll) {
      const sel = state.pendingScroll;
      state.pendingScroll = null;
      scrollToSelector(sel);
    }
  }

  function threadsForHunk(hunkId) {
    return (S.conv?.threads || []).filter((t) => t.hunk_id === hunkId);
  }

  function clear() {
    S = { conv: null, draft: { comments: [] }, error: null, loading: false, ver: ++ver };
  }

  return {
    load, clear, applyThreads, onClick, addToReview, pendingSummary, explorer, count, render,
    startReview, submit, threadsForHunk, active, get state() { return S; },
  };
})();
