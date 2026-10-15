'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { parseGitHubLocation: parse, buildOpenUrl } = require('./github-url.js');

test('pull request', () => {
  assert.deepStrictEqual(parse('/octo/repo/pull/123'), { repo: 'octo/repo', pr: 123 });
});

test('pull request sub-tabs and trailing slash', () => {
  for (const p of [
    '/octo/repo/pull/123/',
    '/octo/repo/pull/123/files',
    '/octo/repo/pull/123/files/',
    '/octo/repo/pull/123/commits',
    '/octo/repo/pull/123/commits/abc123',
    '/octo/repo/pull/123/checks',
    '/octo/repo/pull/123/changes',
  ]) {
    assert.deepStrictEqual(parse(p), { repo: 'octo/repo', pr: 123 }, p);
  }
});

test('query and hash are ignored', () => {
  assert.deepStrictEqual(parse('/octo/repo/pull/7/files?w=1#diff-abc'), { repo: 'octo/repo', pr: 7 });
  assert.deepStrictEqual(parse('/o/n/compare/main...dev?expand=1'), { repo: 'o/n', base: 'main', head: 'dev' });
});

test('compare with three dots', () => {
  assert.deepStrictEqual(parse('/o/n/compare/main...feature'), { repo: 'o/n', base: 'main', head: 'feature' });
  assert.deepStrictEqual(parse('/o/n/compare/main...feature/'), { repo: 'o/n', base: 'main', head: 'feature' });
});

test('compare with two dots', () => {
  assert.deepStrictEqual(parse('/o/n/compare/v1.0..v2.0'), { repo: 'o/n', base: 'v1.0', head: 'v2.0' });
});

test('compare with a single ref', () => {
  assert.deepStrictEqual(parse('/o/n/compare/feature'), { repo: 'o/n', base: null, head: 'feature' });
});

test('branch names with slashes and percent-encoding', () => {
  const want = { repo: 'o/n', base: 'release/1.x', head: 'feature/x' };
  assert.deepStrictEqual(parse('/o/n/compare/release/1.x...feature/x'), want);
  assert.deepStrictEqual(parse('/o/n/compare/release%2F1.x...feature%2Fx'), want);
  assert.deepStrictEqual(parse('/o/n/compare/feature%2Fx'), { repo: 'o/n', base: null, head: 'feature/x' });
  assert.deepStrictEqual(parse('/o/n/compare/main...caf%C3%A9'), { repo: 'o/n', base: 'main', head: 'café' });
});

test('malformed percent-encoding is rejected', () => {
  assert.strictEqual(parse('/o/n/compare/main...bad%ZZ'), null);
});

test('cross-fork compares', () => {
  const want = { repo: 'o/n', unsupported: 'cross-fork' };
  assert.deepStrictEqual(parse('/o/n/compare/main...someone:branch'), want);
  assert.deepStrictEqual(parse('/o/n/compare/main...owner:repo:branch'), want);
  assert.deepStrictEqual(parse('/o/n/compare/someone:main...branch'), want);
  assert.deepStrictEqual(parse('/o/n/compare/someone:branch'), want);
  assert.deepStrictEqual(parse('/o/n/compare/main...someone%3Abranch'), want);
});

test('reserved top-level paths', () => {
  for (const p of [
    '/settings/profile/pull/1',
    '/orgs/acme/pull/1',
    '/marketplace',
    '/marketplace/actions/pull/1',
    '/notifications',
    '/notifications/beta/pull/1',
    '/explore/x/compare/a...b',
  ]) {
    assert.strictEqual(parse(p), null, p);
  }
});

test('non-matching pages', () => {
  for (const p of [
    '/',
    '',
    '/o',
    '/o/n',
    '/o/n/',
    '/o/n/issues/1',
    '/o/n/pulls',
    '/o/n/pull',
    '/o/n/pull/abc',
    '/o/n/pull/0',
    '/o/n/compare',
    '/o/n/compare/',
    '/o/n/tree/main',
    '/o/n/commit/abc',
  ]) {
    assert.strictEqual(parse(p), null, JSON.stringify(p));
  }
  assert.strictEqual(parse(undefined), null);
});

test('invalid owner or repo names', () => {
  assert.strictEqual(parse('/o_x/n/pull/1'), null);
  assert.strictEqual(parse('/o/n!/pull/1'), null);
  assert.strictEqual(parse('/o%20x/n/pull/1'), null);
  assert.deepStrictEqual(parse('/my-org/my.repo_name-2/pull/1'), { repo: 'my-org/my.repo_name-2', pr: 1 });
});

test('buildOpenUrl for pull requests', () => {
  assert.strictEqual(buildOpenUrl({ repo: 'o/n', pr: 123 }), 'refactor-diff://open?repo=o%2Fn&pr=123');
});

test('buildOpenUrl for compares encodes values', () => {
  assert.strictEqual(
    buildOpenUrl({ repo: 'o/n', base: 'main', head: 'feature/x' }),
    'refactor-diff://open?repo=o%2Fn&base=main&head=feature%2Fx'
  );
  assert.strictEqual(
    buildOpenUrl({ repo: 'o/n', base: null, head: 'feature/x' }),
    'refactor-diff://open?repo=o%2Fn&head=feature%2Fx'
  );
  const url = buildOpenUrl({ repo: 'o/n', base: 'a&b', head: 'c=d#e' });
  const params = new URL(url).searchParams;
  assert.strictEqual(params.get('base'), 'a&b');
  assert.strictEqual(params.get('head'), 'c=d#e');
});

test('buildOpenUrl round-trips parsed locations', () => {
  assert.strictEqual(
    buildOpenUrl(parse('/o/n/compare/main...feature%2Fx')),
    'refactor-diff://open?repo=o%2Fn&base=main&head=feature%2Fx'
  );
  assert.strictEqual(buildOpenUrl(parse('/o/n/pull/9/files')), 'refactor-diff://open?repo=o%2Fn&pr=9');
});

test('buildOpenUrl returns null for unsupported or empty input', () => {
  assert.strictEqual(buildOpenUrl(null), null);
  assert.strictEqual(buildOpenUrl(undefined), null);
  assert.strictEqual(buildOpenUrl({ repo: 'o/n', unsupported: 'cross-fork' }), null);
  assert.strictEqual(buildOpenUrl({ repo: 'o/n' }), null);
});
