// Injects a single "Open in Refactor Diff" button on GitHub pull request and
// compare pages. Depends on github-url.js (globalThis.RefactorDiffGitHub).
(function () {
  'use strict';

  const api = globalThis.RefactorDiffGitHub;
  if (!api) return;

  const ATTR = 'data-refactor-diff';
  const CROSS_FORK_TITLE =
    "Cross-fork compares aren't supported yet; open the pull request instead";

  // Ordered placement candidates. `where` is how the button joins the target.
  const PR_TARGETS = [
    { selector: '[data-component="PH_Actions"]', where: 'prepend' },
    { selector: '[data-testid="pr-header-actions"]', where: 'prepend' },
    { selector: '.gh-header-actions', where: 'prepend' },
  ];
  const COMPARE_TARGETS = [
    { selector: '.range-editor', where: 'append' },
    { selector: '[data-testid="compare-range-editor"]', where: 'append' },
    { selector: '.compare-show-header, .Subhead', where: 'append' },
  ];

  function findTarget(parsed) {
    const list = typeof parsed.pr === 'number' ? PR_TARGETS : COMPARE_TARGETS;
    for (const t of list) {
      const el = document.querySelector(t.selector);
      if (el) return { el, where: t.where };
    }
    return null;
  }

  function keyFor(parsed) {
    if (parsed.unsupported) return parsed.repo + '|' + parsed.unsupported;
    return api.buildOpenUrl(parsed) || '';
  }

  const ICON_SVG =
    '<svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16" class="octicon">' +
    '<rect x="1" y="3" width="9" height="3" rx="1" fill="#f85149"></rect>' +
    '<rect x="6" y="10" width="9" height="3" rx="1" fill="#3fb950"></rect></svg>';

  function createButton(parsed, key) {
    const url = api.buildOpenUrl(parsed);
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'btn btn-sm refactor-diff-btn';
    btn.setAttribute(ATTR, key);
    btn.innerHTML = ICON_SVG + '<span>Open in Refactor Diff</span>';
    if (!url) {
      btn.disabled = true;
      btn.setAttribute('aria-disabled', 'true');
      btn.title = CROSS_FORK_TITLE;
    } else {
      btn.title = 'Open this diff in the Refactor Diff app';
      btn.addEventListener('click', (e) => {
        e.preventDefault();
        window.location.href = url;
      });
    }
    return btn;
  }

  function place(btn, target) {
    btn.classList.remove('refactor-diff-floating');
    if (!target) {
      btn.classList.add('refactor-diff-floating');
      document.body.appendChild(btn);
    } else if (target.where === 'prepend') {
      target.el.prepend(btn);
    } else {
      target.el.append(btn);
    }
  }

  function update() {
    const existing = Array.from(document.querySelectorAll('[' + ATTR + ']'));
    const parsed = api.parseGitHubLocation(window.location.pathname);

    if (!parsed) {
      existing.forEach((el) => el.remove());
      return;
    }

    const key = keyFor(parsed);
    let btn = null;
    for (const el of existing) {
      if (!btn && el.getAttribute(ATTR) === key) btn = el;
      else el.remove(); // stale (different PR/compare) or a duplicate
    }

    const target = findTarget(parsed);
    if (!btn) {
      place(createButton(parsed, key), target);
      return;
    }
    // Move the button if a better spot appeared (or its spot went away).
    const parent = target ? target.el : document.body;
    const floating = btn.classList.contains('refactor-diff-floating');
    if (btn.parentElement !== parent || floating !== !target) place(btn, target);
  }

  let timer = null;
  function schedule() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      update();
    }, 250);
  }

  ['turbo:load', 'turbo:render', 'pjax:end'].forEach((ev) =>
    document.addEventListener(ev, update)
  );
  window.addEventListener('popstate', update);

  const observer = new MutationObserver((mutations) => {
    // Ignore mutations caused only by our own button.
    const ours = mutations.every((m) =>
      [...m.addedNodes, ...m.removedNodes].every(
        (n) => n.nodeType === 1 && n.hasAttribute && n.hasAttribute(ATTR)
      ) && (m.addedNodes.length || m.removedNodes.length)
    );
    if (!ours) schedule();
  });
  if (document.body) observer.observe(document.body, { childList: true, subtree: true });

  update();
})();
