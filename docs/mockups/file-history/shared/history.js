/* Shared sample data and helpers for the file-history mockups.
   One file, server/users.py, across nine commits. The last two are the PR under review. */
(function () {
  const file = 'server/users.py';
  const commits = [
    { sha: '4c1e9d2a7f30', author: 'Ada Okafor',     date: '2026-02-03', message: 'Add users module' },
    { sha: '9b7f02e1c4d8', author: 'Ada Okafor',     date: '2026-02-11', message: 'Validate email on create' },
    { sha: 'd30a5c8e92b1', author: 'Priya Nair',     date: '2026-03-02', message: 'Cache user lookups' },
    { sha: '71e4bb0f6a25', author: 'Ada Okafor',     date: '2026-03-20', message: 'Add delete_user with a soft-delete flag' },
    { sha: 'e85d2f19c7a3', author: 'Luis Ortega',    date: '2026-05-07', message: 'Switch to a User dataclass' },
    { sha: '2fa9c67d0e14', author: 'Priya Nair',     date: '2026-06-15', message: 'Raise UserNotFound instead of returning None' },
    { sha: 'b6d3e40a81f9', author: 'Luis Ortega',    date: '2026-07-22', message: 'Log slow lookups', tag: 'base' },
    { sha: '0c7a1e5fd2b6', author: 'Mara Lindqvist', date: '2026-10-09', message: 'Rename get_user → fetch_user', pr: true },
    { sha: 'f1e8b93c4a07', author: 'Mara Lindqvist', date: '2026-10-12', message: 'Add fetch_user_or_none for optional callers', pr: true, tag: 'head' },
  ];

  const v = [];
  v[0] = `"""User storage for the shop API."""

from server.db import connection


def get_user(user_id):
    """Return the user row for user_id, or None."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ?", (user_id,)
        ).fetchone()
    return row


def create_user(email, name):
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
`;
  v[1] = `"""User storage for the shop API."""

import re

from server.db import connection

EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


def get_user(user_id):
    """Return the user row for user_id, or None."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ?", (user_id,)
        ).fetchone()
    return row


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
`;
  v[2] = `"""User storage for the shop API."""

import re
from functools import lru_cache

from server.db import connection

EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


@lru_cache(maxsize=1024)
def get_user(user_id):
    """Return the user row for user_id, or None."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ?", (user_id,)
        ).fetchone()
    return row


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
    get_user.cache_clear()
`;
  v[3] = `"""User storage for the shop API."""

import re
from functools import lru_cache

from server.db import connection

EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


@lru_cache(maxsize=1024)
def get_user(user_id):
    """Return the user row for user_id, or None."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ? AND deleted = 0",
            (user_id,),
        ).fetchone()
    return row


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
    get_user.cache_clear()


def delete_user(user_id):
    """Soft-delete: the row stays for audit, lookups stop returning it."""
    with connection() as conn:
        conn.execute("UPDATE users SET deleted = 1 WHERE id = ?", (user_id,))
        conn.commit()
    get_user.cache_clear()
`;
  v[4] = `"""User storage for the shop API."""

import re
from dataclasses import dataclass
from functools import lru_cache

from server.db import connection

EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")


@dataclass(frozen=True)
class User:
    id: int
    email: str
    name: str


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


@lru_cache(maxsize=1024)
def get_user(user_id):
    """Return the User for user_id, or None."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ? AND deleted = 0",
            (user_id,),
        ).fetchone()
    if row is None:
        return None
    return User(*row)


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
    get_user.cache_clear()


def delete_user(user_id):
    """Soft-delete: the row stays for audit, lookups stop returning it."""
    with connection() as conn:
        conn.execute("UPDATE users SET deleted = 1 WHERE id = ?", (user_id,))
        conn.commit()
    get_user.cache_clear()
`;
  v[5] = `"""User storage for the shop API."""

import re
from dataclasses import dataclass
from functools import lru_cache

from server.db import connection

EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")


class UserNotFound(LookupError):
    """Raised when a user id does not exist or was soft-deleted."""


@dataclass(frozen=True)
class User:
    id: int
    email: str
    name: str


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


@lru_cache(maxsize=1024)
def get_user(user_id):
    """Return the User for user_id; raise UserNotFound otherwise."""
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ? AND deleted = 0",
            (user_id,),
        ).fetchone()
    if row is None:
        raise UserNotFound(user_id)
    return User(*row)


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
    get_user.cache_clear()


def delete_user(user_id):
    """Soft-delete: the row stays for audit, lookups stop returning it."""
    with connection() as conn:
        conn.execute("UPDATE users SET deleted = 1 WHERE id = ?", (user_id,))
        conn.commit()
    get_user.cache_clear()
`;
  v[6] = `"""User storage for the shop API."""

import logging
import re
import time
from dataclasses import dataclass
from functools import lru_cache

from server.db import connection

log = logging.getLogger(__name__)
EMAIL_RE = re.compile(r"^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")
SLOW_MS = 50


class UserNotFound(LookupError):
    """Raised when a user id does not exist or was soft-deleted."""


@dataclass(frozen=True)
class User:
    id: int
    email: str
    name: str


def validate_email(email):
    if not EMAIL_RE.match(email):
        raise ValueError(f"invalid email: {email!r}")
    return email.lower()


@lru_cache(maxsize=1024)
def get_user(user_id):
    """Return the User for user_id; raise UserNotFound otherwise."""
    started = time.perf_counter()
    with connection() as conn:
        row = conn.execute(
            "SELECT id, email, name FROM users WHERE id = ? AND deleted = 0",
            (user_id,),
        ).fetchone()
    elapsed_ms = (time.perf_counter() - started) * 1000
    if elapsed_ms > SLOW_MS:
        log.warning("slow user lookup %s: %.1fms", user_id, elapsed_ms)
    if row is None:
        raise UserNotFound(user_id)
    return User(*row)


def create_user(email, name):
    email = validate_email(email)
    with connection() as conn:
        conn.execute(
            "INSERT INTO users (email, name) VALUES (?, ?)", (email, name)
        )
        conn.commit()
    get_user.cache_clear()


def delete_user(user_id):
    """Soft-delete: the row stays for audit, lookups stop returning it."""
    with connection() as conn:
        conn.execute("UPDATE users SET deleted = 1 WHERE id = ?", (user_id,))
        conn.commit()
    get_user.cache_clear()
`;
  v[7] = v[6].split('get_user').join('fetch_user');
  v[8] = v[7].replace(
`    if row is None:
        raise UserNotFound(user_id)
    return User(*row)
`,
`    if row is None:
        raise UserNotFound(user_id)
    return User(*row)


def fetch_user_or_none(user_id):
    """Like fetch_user, for callers that treat a missing user as normal."""
    try:
        return fetch_user(user_id)
    except UserNotFound:
        return None
`);

  const versions = v;
  const lines = (i) => versions[i].replace(/\n$/, '').split('\n');

  // Line-level LCS diff. Files are small, so a plain DP table is fine.
  const diffCache = new Map();
  function diff(a, b) {
    const key = a + ':' + b;
    if (diffCache.has(key)) return diffCache.get(key);
    const A = lines(a), B = lines(b);
    const n = A.length, m = B.length;
    const L = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
    for (let i = n - 1; i >= 0; i--)
      for (let j = m - 1; j >= 0; j--)
        L[i][j] = A[i] === B[j] ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1]);
    const ops = [];
    let i = 0, j = 0;
    while (i < n || j < m) {
      if (i < n && j < m && A[i] === B[j]) { ops.push({ t: 'eq', a: i, b: j, text: A[i] }); i++; j++; }
      else if (i < n && (j === m || L[i + 1][j] >= L[i][j + 1])) { ops.push({ t: 'del', a: i, text: A[i] }); i++; }
      else { ops.push({ t: 'add', b: j, text: B[j] }); j++; }
    }
    diffCache.set(key, ops);
    return ops;
  }
  function stats(a, b) {
    const ops = diff(a, b);
    return { add: ops.filter(o => o.t === 'add').length, del: ops.filter(o => o.t === 'del').length };
  }
  // Group consecutive del/add runs into hunks (for ribbons and counts).
  function hunks(a, b) {
    const ops = diff(a, b), out = [];
    let cur = null;
    ops.forEach((o, idx) => {
      if (o.t === 'eq') { cur = null; return; }
      if (!cur) { cur = { aStart: null, aEnd: null, bStart: null, bEnd: null, del: 0, add: 0, at: idx }; out.push(cur); }
      if (o.t === 'del') { if (cur.aStart === null) cur.aStart = o.a; cur.aEnd = o.a + 1; cur.del++; }
      else { if (cur.bStart === null) cur.bStart = o.b; cur.bEnd = o.b + 1; cur.add++; }
    });
    // Pure inserts or deletes get a zero-height anchor on the other side.
    return out.map(h => ({ ...h, aStart: h.aStart ?? anchorA(ops, h.at), aEnd: h.aEnd ?? anchorA(ops, h.at), bStart: h.bStart ?? anchorB(ops, h.at), bEnd: h.bEnd ?? anchorB(ops, h.at) }));
  }
  function anchorA(ops, at) { for (let k = at - 1; k >= 0; k--) if (ops[k].t !== 'add') return (ops[k].a ?? 0) + 1; return 0; }
  function anchorB(ops, at) { for (let k = at - 1; k >= 0; k--) if (ops[k].t !== 'del') return (ops[k].b ?? 0) + 1; return 0; }

  // Blame: for each version, which commit index last wrote each line.
  const blames = [];
  blames[0] = lines(0).map(() => 0);
  for (let k = 1; k < versions.length; k++) {
    const prev = blames[k - 1], cur = [];
    for (const o of diff(k - 1, k)) {
      if (o.t === 'eq') cur.push(prev[o.a]);
      else if (o.t === 'add') cur.push(k);
    }
    blames[k] = cur;
  }
  const blame = (i) => blames[i];

  // Heat: how recently a line was written relative to the version being viewed.
  function heat(origin, current) {
    const span = Math.max(1, current);
    const t = (current - origin) / span;          // 0 = written in this commit, 1 = oldest
    const pct = Math.round((1 - t) * 100);
    return `color-mix(in oklab, var(--heat-new) ${pct}%, var(--heat-old))`;
  }

  // Tiny Python highlighter. Enough for the sample; not a real tokenizer.
  const KW = /\b(def|class|return|if|else|elif|with|as|import|from|raise|try|except|not|and|or|None|True|False|is|in|for|while|pass)\b/g;
  function esc(s) { return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;'); }
  function highlight(text) {
    let s = esc(text);
    if (/^\s*#/.test(s)) return `<span class="sx-com">${s}</span>`;
    if (/^\s*"""/.test(s)) return `<span class="sx-com">${s}</span>`;
    if (/^\s*@/.test(s)) return s.replace(/@[\w.]+/, m => `<span class="sx-deco">${m}</span>`).replace(/\((.*)\)$/, (m, inner) => '(' + inner.replace(/\d+/g, n => `<span class="sx-num">${n}</span>`) + ')');
    const parts = [];
    let re = /(r?f?"(?:[^"\\]|\\.)*")|(\b\d+\b)/g, last = 0, m;
    while ((m = re.exec(s))) {
      parts.push(plain(s.slice(last, m.index)));
      if (m[1]) parts.push(`<span class="sx-str">${m[1]}</span>`);
      else parts.push(`<span class="sx-num">${m[2]}</span>`);
      last = re.lastIndex;
    }
    parts.push(plain(s.slice(last)));
    return parts.join('');
  }
  function plain(s) {
    return s
      .replace(KW, '<span class="sx-kw">$1</span>')
      .replace(/(<span class="sx-kw">(?:def)<\/span> )(\w+)/, '$1<span class="sx-fn">$2</span>')
      .replace(/(<span class="sx-kw">(?:class)<\/span> )(\w+)/, '$1<span class="sx-type">$2</span>')
      .replace(/\b(ValueError|LookupError|int|str|print|len)\b/g, '<span class="sx-builtin">$1</span>');
  }

  const short = (sha) => sha.slice(0, 7);
  const initials = (name) => name.split(' ').map(w => w[0]).join('').slice(0, 2).toUpperCase();
  const fmtDate = (d) => new Date(d + 'T12:00:00').toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
  const rel = (d) => { const days = Math.round((new Date('2026-10-14') - new Date(d)) / 864e5); return days < 1 ? 'today' : days < 30 ? `${days}d ago` : days < 365 ? `${Math.round(days / 30)}mo ago` : `${(days / 365).toFixed(1)}y ago`; };

  // Row element for one line of code.
  function rowEl(text, num, origin, current, kind) {
    const r = document.createElement('div');
    r.className = 'row' + (kind ? ' ' + kind : '');
    if (origin != null) r.style.setProperty('--heat', heat(origin, current));
    r.innerHTML = `<span class="age"></span><span class="num">${num ?? ''}</span><span class="src">${highlight(text)}</span>`;
    return r;
  }

  // Morph a container from the version it shows to version `to`.
  // Kept lines stay in place; removed lines flash red and collapse; added lines expand with a green flash.
  function morph(container, to, opts = {}) {
    const from = container.dataset.version === undefined ? null : +container.dataset.version;
    const bl = blame(to);
    container.dataset.version = to;
    if (from === null || from === to || opts.instant) {
      container.replaceChildren(...lines(to).map((t, i) => rowEl(t, i + 1, bl[i], to)));
      return;
    }
    const ops = diff(from, to);
    const old = Array.from(container.children);
    const frag = document.createDocumentFragment();
    const leaving = [], entering = [];
    for (const o of ops) {
      if (o.t === 'eq') { const r = old[o.a]; r.className = 'row'; r.querySelector('.num').textContent = o.b + 1; r.style.setProperty('--heat', heat(bl[o.b], to)); frag.appendChild(r); }
      else if (o.t === 'del') { const r = old[o.a]; r.className = 'row del'; leaving.push(r); frag.appendChild(r); }
      else { const r = rowEl(o.text, o.b + 1, bl[o.b], to, 'add'); r.style.height = '0px'; r.style.opacity = '0'; entering.push(r); frag.appendChild(r); }
    }
    container.replaceChildren(frag);
    const hold = opts.hold ?? 260;
    requestAnimationFrame(() => {
      entering.forEach(r => { r.style.height = ''; r.style.opacity = ''; });
      setTimeout(() => leaving.forEach(r => r.classList.add('leaving')), hold);
      setTimeout(() => { leaving.forEach(r => r.remove()); entering.forEach(r => { r.classList.remove('add'); r.classList.add('hl-add'); setTimeout(() => r.classList.remove('hl-add'), 900); }); }, hold + 320);
    });
  }

  // Static unified rendering of a → b.
  function renderUnified(container, a, b) {
    const ba = blame(a), bb = blame(b);
    container.dataset.version = '';
    container.replaceChildren(...diff(a, b).map(o => {
      if (o.t === 'eq') return rowEl(o.text, o.b + 1, bb[o.b], b);
      if (o.t === 'add') return rowEl(o.text, o.b + 1, bb[o.b], b, 'add');
      return rowEl(o.text, o.a + 1, ba[o.a], a, 'del');
    }));
  }

  // Scroll a pane so its first changed row sits a third of the way down.
  function scrollToChange(pane) {
    const r = pane.querySelector('.row.add, .row.del, .row.hl-add, .row.hl-del, .row.ghost-add, .row.ghost-del');
    if (r) pane.scrollTo({ top: Math.max(0, r.offsetTop - pane.clientHeight / 3), behavior: 'smooth' });
  }

  // Shared app chrome.
  function topbar(el, label) {
    el.className = 'topbar';
    el.innerHTML = `<span class="brand">⌗ refactor-diff <span>~/code/shop</span></span>
      <span class="seg"><span>branches</span><span class="on">pr</span><span>worktree</span></span>
      <span class="kv">base<b>main</b></span><span class="kv">head<b>rename-user-api</b></span>
      <span class="mock">${label}</span>`;
  }
  function toast(html, ms = 4200) {
    let t = document.querySelector('.toast');
    if (!t) { t = document.createElement('div'); t.className = 'toast'; document.body.appendChild(t); }
    t.innerHTML = html; t.classList.add('show');
    clearTimeout(t._h); t._h = setTimeout(() => t.classList.remove('show'), ms);
    t.querySelectorAll('[data-dismiss]').forEach(b => b.onclick = () => t.classList.remove('show'));
  }
  function checkoutToast(i, fileOnly) {
    const c = commits[i];
    const cmd = fileOnly ? `git checkout ${short(c.sha)} -- ${file}` : `git checkout ${short(c.sha)}`;
    toast(`<b>${fileOnly ? 'Restored this file at' : 'Checked out'} ${short(c.sha)}</b> <span class="muted">${c.message}</span><code>${cmd}</code>
      <div class="t-actions"><button class="btn" data-dismiss>Undo</button><button class="btn" data-dismiss>Open in editor</button></div>`);
  }
  function checkoutMenu(anchor, i) {
    const c = commits[i];
    toast(`<b>Checkout ${short(c.sha)}</b> <span class="muted">${c.message}</span>
      <div class="t-actions"><button class="btn primary" data-co="full">Checkout commit</button><button class="btn" data-co="file">Restore just ${file}</button><button class="btn" data-dismiss>Cancel</button></div>`, 8000);
    document.querySelectorAll('.toast [data-co]').forEach(b => b.onclick = () => checkoutToast(i, b.dataset.co === 'file'));
  }

  window.FH = { file, commits, versions, lines, diff, stats, hunks, blame, heat, highlight, short, initials, fmtDate, rel, rowEl, morph, renderUnified, scrollToChange, topbar, toast, checkoutToast, checkoutMenu, esc };
})();
