import test from 'node:test';
import assert from 'node:assert/strict';
import { githubCaches, removeCaches, unusedCaches } from './cache-cleanup.mjs';

const uuid = '0f8c2b52-4b7e-4d47-9a3e-1c2d3e4f5a6b';
const apt = (prefix, run, created) => ({ key: `apt-v1-noble-amd64-${prefix}-${run}-1-${uuid}`, ref: 'refs/heads/main', created_at: created });
let id = 0;
const entry = fields => ({ id: ++id, size_in_bytes: 1e6, created_at: '2026-11-19T00:00:00Z', ...fields });

const answers = ({ open = [], branches = [] } = {}) => {
  const asked = [];
  return {
    asked,
    pullOpen: async number => { asked.push(`pull ${number}`); return open.includes(number); },
    branchExists: async name => { asked.push(`branch ${name}`); return branches.includes(name); },
  };
};

test("deletes a closed pull request's entries and keeps an open one's", async () => {
  const caches = [
    entry({ key: 'interfaces-a', ref: 'refs/pull/12/merge' }),
    entry({ key: 'desktop-cli-a', ref: 'refs/pull/12/merge' }),
    entry({ key: 'interfaces-b', ref: 'refs/pull/30/merge' }),
  ];
  const github = answers({ open: [30] });
  const unused = await unusedCaches(caches, github);
  assert.deepEqual(unused.map(u => u.cache.key), ['interfaces-a', 'desktop-cli-a']);
  assert.match(unused[0].reason, /#12 is closed/);
  assert.deepEqual(github.asked, ['pull 12', 'pull 30']);
});

test('deletes entries of a deleted branch, never asks about main', async () => {
  const caches = [
    entry({ key: 'portable-a', ref: 'refs/heads/main' }),
    entry({ key: 'portable-b', ref: 'refs/heads/feature/gone' }),
    entry({ key: 'portable-c', ref: 'refs/heads/feature/kept' }),
  ];
  const github = answers({ branches: ['feature/kept'] });
  const unused = await unusedCaches(caches, github);
  assert.deepEqual(unused.map(u => u.cache.key), ['portable-b']);
  assert.deepEqual(github.asked, ['branch feature/gone', 'branch feature/kept']);
});

test('keeps only the newest copy of each Ubuntu package cache', async () => {
  const a = '0123456789abcdef';
  const b = 'fedcba9876543210';
  const caches = [
    entry(apt(a, 100, '2026-11-17T00:00:00Z')),
    entry(apt(a, 300, '2026-11-19T00:00:00Z')),
    entry(apt(a, 200, '2026-11-18T00:00:00Z')),
    entry(apt(b, 150, '2026-11-10T00:00:00Z')),
    entry({ key: 'apt-v1-noble-arm64-0123456789abcdef-50-1-' + uuid, ref: 'refs/heads/main', created_at: '2026-11-01T00:00:00Z' }),
    entry({ key: 'v0-rust-portable-v3-old', ref: 'refs/heads/main', created_at: '2026-11-01T00:00:00Z' }),
  ];
  const unused = await unusedCaches(caches, answers());
  assert.deepEqual(unused.map(u => u.cache.key).sort(), [caches[0].key, caches[2].key].sort());
});

test('keeps the newest package copy when an entry is listed twice or two share a time', async () => {
  const a = '0123456789abcdef';
  const newest = entry(apt(a, 300, '2026-11-19T00:00:00Z'));
  const older = entry(apt(a, 100, '2026-11-17T00:00:00Z'));
  let unused = await unusedCaches([newest, older, newest], answers());
  assert.deepEqual(unused.map(u => u.cache.id), [older.id]);

  const first = entry(apt(a, 400, '2026-11-20T00:00:00Z'));
  const second = entry(apt(a, 401, '2026-11-20T00:00:00Z'));
  unused = await unusedCaches([second, first], answers());
  assert.deepEqual(unused.map(u => u.cache.id), [first.id]);
});

// Answers each request from `routes` by method and path, and records it.
function fakeGitHub(routes) {
  const requests = [];
  const github = githubCaches({
    api: 'https://api.example.test', repository: 'owner/repo', token: 'token-value',
    fetchImpl: async (url, init) => {
      const path = url.replace('https://api.example.test/repos/owner/repo', '');
      requests.push(`${init.method} ${path}`);
      assert.equal(init.headers.authorization, 'Bearer token-value');
      const route = routes[`${init.method} ${path}`];
      const { status, body } = typeof route === 'function' ? route() : route ?? { status: 500 };
      return { ok: status < 300, status, json: async () => body };
    },
  });
  return { github, requests };
}

test('lists every page newest first, drops repeated entries and filters by ref', async () => {
  const page = n => `GET /actions/caches?per_page=100&page=${n}&sort=created_at&direction=desc`;
  const { github } = fakeGitHub({
    [page(1)]: { status: 200, body: { total_count: 101, actions_caches: Array.from({ length: 100 }, (_, i) => ({ id: i })) } },
    // An entry created meanwhile pushed id 99 onto the next page.
    [page(2)]: { status: 200, body: { total_count: 102, actions_caches: [{ id: 99 }, { id: 100 }] } },
    [`${page(1)}&ref=refs%2Fpull%2F7%2Fmerge`]: { status: 200, body: { total_count: 0, actions_caches: [] } },
  });
  assert.deepEqual((await github.list()).map(c => c.id), Array.from({ length: 101 }, (_, i) => i));
  assert.deepEqual(await github.list('refs/pull/7/merge'), []);
});

test("a branch counts as deleted only once the token can see the default branch", async () => {
  const { github, requests } = fakeGitHub({
    'GET /git/ref/heads/main': { status: 200, body: {} },
    'GET /git/ref/heads/feature/a%20b': { status: 404 },
    'GET /git/ref/heads/kept': { status: 200, body: {} },
  });
  assert.equal(await github.branchExists('feature/a b'), false);
  assert.equal(await github.branchExists('kept'), true);
  assert.deepEqual(requests, ['GET /git/ref/heads/main', 'GET /git/ref/heads/feature/a%20b', 'GET /git/ref/heads/kept']);

  const blind = fakeGitHub({ 'GET /git/ref/heads/main': { status: 404 }, 'GET /git/ref/heads/kept': { status: 404 } });
  await assert.rejects(blind.github.branchExists('kept'), /contents: read/);
});

test('a pull request the token cannot see is an error, not a closed one', async () => {
  const { github } = fakeGitHub({
    'GET /pulls/7': { status: 200, body: { state: 'closed' } },
    'GET /pulls/8': { status: 200, body: { state: 'open' } },
    'GET /pulls/9': { status: 404 },
  });
  assert.equal(await github.pullOpen(7), false);
  assert.equal(await github.pullOpen(8), true);
  await assert.rejects(github.pullOpen(9), /HTTP 404/);
});

test('an entry already gone counts as deleted; other refusals and a missing list are errors', async () => {
  const { github } = fakeGitHub({
    'DELETE /actions/caches/5': { status: 204 },
    'DELETE /actions/caches/6': { status: 404 },
    'DELETE /actions/caches/7': { status: 403 },
    'GET /actions/caches?per_page=100&page=1&sort=created_at&direction=desc': { status: 404 },
  });
  await github.remove(5);
  await github.remove(6);
  await assert.rejects(github.remove(7), /HTTP 403/);
  await assert.rejects(github.list(), /HTTP 404/);
});

test('reports what it freed, keeps going past a failed delete, and a dry run deletes nothing', async () => {
  const unused = [
    { cache: entry({ key: 'a', ref: 'refs/pull/1/merge', size_in_bytes: 300e6 }), reason: 'closed' },
    { cache: entry({ key: 'b', ref: 'refs/pull/1/merge', size_in_bytes: 200e6 }), reason: 'closed' },
  ];
  const removed = [];
  const result = await removeCaches(unused, {
    remove: async cacheId => { if (cacheId === unused[0].cache.id) throw new Error('HTTP 500'); removed.push(cacheId); },
    log: () => {},
  });
  assert.deepEqual(removed, [unused[1].cache.id]);
  assert.match(result.report, /^Freed 200 MB from 1 unused cache entries\./);
  assert.match(result.report, /a: HTTP 500/);
  assert.equal(result.failures.length, 1);

  const dry = await removeCaches(unused, { remove: async () => assert.fail('dry run deleted'), dryRun: true, log: () => {} });
  assert.equal(dry.report, 'Would free 500 MB from 2 unused cache entries.');
});
