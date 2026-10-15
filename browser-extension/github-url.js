// Pure helpers that turn a GitHub pathname into a refactor-diff:// link.
// No DOM use: loaded as a content script (sets globalThis.RefactorDiffGitHub)
// and required from Node by the tests (module.exports).
(function (root, factory) {
  const api = factory();
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  } else {
    root.RefactorDiffGitHub = api;
  }
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  const OWNER_RE = /^[A-Za-z0-9-]+$/;
  const REPO_RE = /^[A-Za-z0-9._-]+$/;

  // First path segments that are GitHub pages, not repository owners.
  const RESERVED = new Set([
    'about', 'account', 'apps', 'codespaces', 'collections', 'contact',
    'customer-stories', 'dashboard', 'enterprise', 'enterprises', 'events',
    'explore', 'features', 'gist', 'github-copilot', 'issues', 'login',
    'logout', 'marketplace', 'new', 'notifications', 'organizations', 'orgs',
    'pricing', 'pulls', 'readme', 'search', 'security', 'sessions', 'settings',
    'signup', 'site', 'sponsors', 'stars', 'team', 'topics', 'trending',
    'users', 'watching',
  ]);

  function decodeRef(raw) {
    if (!raw) return null;
    try {
      return decodeURIComponent(raw);
    } catch (_) {
      return null;
    }
  }

  /**
   * Parse a GitHub pathname.
   * @returns {{repo:string, pr:number} | {repo:string, base:(string|null), head:string}
   *   | {repo:string, unsupported:'cross-fork'} | null}
   */
  function parseGitHubLocation(pathname) {
    if (typeof pathname !== 'string') return null;
    const path = pathname.replace(/[?#].*$/, '');
    const segments = path.split('/').filter((s) => s !== '');
    if (segments.length < 3) return null;

    const [owner, name, kind] = segments;
    if (RESERVED.has(owner.toLowerCase())) return null;
    if (!OWNER_RE.test(owner) || !REPO_RE.test(name)) return null;
    const repo = owner + '/' + name;

    if (kind === 'pull') {
      // /o/n/pull/123 and any sub-tab: /files, /commits/<sha>, /checks, ...
      const num = segments[3];
      if (!num || !/^\d+$/.test(num)) return null;
      const pr = Number(num);
      return pr > 0 ? { repo, pr } : null;
    }

    if (kind === 'compare') {
      // Branch names may contain "/", so take everything after /compare/.
      const range = segments.slice(3).join('/');
      if (!range) return null;
      let baseRaw = null;
      let headRaw;
      const three = range.indexOf('...');
      if (three !== -1) {
        baseRaw = range.slice(0, three);
        headRaw = range.slice(three + 3);
      } else {
        const two = range.indexOf('..');
        if (two !== -1) {
          baseRaw = range.slice(0, two);
          headRaw = range.slice(two + 2);
        } else {
          headRaw = range;
        }
      }
      const head = decodeRef(headRaw);
      const base = baseRaw === null ? null : decodeRef(baseRaw);
      if (head === null || (baseRaw !== null && base === null)) return null;
      // "user:branch" or "owner:repo:branch" names another fork.
      if (head.includes(':') || (base !== null && base.includes(':'))) {
        return { repo, unsupported: 'cross-fork' };
      }
      return { repo, base, head };
    }

    return null;
  }

  /** Build the refactor-diff:// link for a parsed location, or null. */
  function buildOpenUrl(parsed) {
    if (!parsed || parsed.unsupported || !parsed.repo) return null;
    const params = new URLSearchParams();
    params.set('repo', parsed.repo);
    if (typeof parsed.pr === 'number') {
      params.set('pr', String(parsed.pr));
    } else if (parsed.head) {
      if (parsed.base) params.set('base', parsed.base);
      params.set('head', parsed.head);
    } else {
      return null;
    }
    return 'refactor-diff://open?' + params.toString();
  }

  return { parseGitHubLocation, buildOpenUrl };
});
