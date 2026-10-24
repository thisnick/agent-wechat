import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { assetName, manifestName, releaseRepository, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';
import { nativeReleaseReadiness, resumeNpmRelease, nativePublicationSucceeded, runtimeImagePublished, npmReleaseReadiness } from './native-release-orchestration.mjs';

const version = '1.0.0-beta.10';
const sourceCommit = 'a'.repeat(40);
const token = 'fixture-github-token';
const missing = () => new Response(null, { status: 404 });
const registryUrl = `https://registry.npmjs.org/@agent-wechat%2fcli/${version}`;
const releaseUrl = `https://api.github.com/repos/${releaseRepository}/releases/tags/v${version}`;
const assets = [manifestName(version), `wx-v${version}-THIRD-PARTY-NOTICES.txt`, ...supportedTargets.map(target => assetName(version, target))]
  .map(name => ({ name, state: 'uploaded' }));
const release = () => ({ tag_name: `v${version}`, draft: false, assets: structuredClone(assets) });

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'native-release-orchestration-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, 'package.json'), JSON.stringify({ name: '@agent-wechat/cli', version }));
  return { root, token };
}

test('an unpublished CLI waits for its missing native release without forwarding GitHub credentials to npm', async t => {
  const options = await fixture(t);
  const requests = [];
  const result = await nativeReleaseReadiness({ ...options, fetchImpl: async (url, init) => {
    requests.push(url);
    assert.equal(init.redirect, 'error');
    if (url === registryUrl) assert.equal(init.headers, undefined);
    else { assert.equal(url, releaseUrl); assert.equal(init.headers.Authorization, `Bearer ${token}`); }
    return missing();
  } });
  assert.deepEqual(requests, [registryUrl, releaseUrl]);
  assert.deepEqual(result, { version, ready: false, state: 'missing' });
});

test('the waiting CLI exits successfully and writes an explicit non-publication output and operator summary', async t => {
  const { root } = await fixture(t);
  await mkdir(join(root, 'packages/cli'), { recursive: true });
  await writeFile(join(root, 'packages/cli/package.json'), JSON.stringify({ name: '@agent-wechat/cli', version }));
  const setup = join(root, 'fetch-fixture.mjs');
  await writeFile(setup, 'globalThis.fetch = async () => new Response(null, { status: 404 });');
  const output = join(root, 'output');
  const summary = join(root, 'summary');
  const { stdout } = await promisify(execFile)(process.execPath,
    ['--import', setup, fileURLToPath(new URL('./native-release-orchestration.mjs', import.meta.url)), 'ready'],
    { cwd: root, env: { ...process.env, GITHUB_SHA: sourceCommit, GITHUB_OUTPUT: output, GITHUB_STEP_SUMMARY: summary } });
  assert.deepEqual(JSON.parse(stdout), { version, ready: false, state: 'missing' });
  assert.equal(await readFile(output, 'utf8'), 'ready=false\n');
  const text = await readFile(summary, 'utf8');
  assert.match(text, /Waiting for native release assets/);
  assert.ok(text.includes(version) && text.includes(sourceCommit));
  assert.match(text, /confirm_publish=true/);
  assert.match(text, /automatically retries this original push run/);
});

test('every target, manifest and notice must finish uploading before npm can proceed', async t => {
  const options = await fixture(t);
  for (const asset of assets) {
    for (const state of ['missing', 'starter']) {
      const value = release();
      value.assets = state === 'missing' ? value.assets.filter(item => item.name !== asset.name)
        : value.assets.map(item => item.name === asset.name ? { ...item, state } : item);
      const result = await nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(value) });
      assert.deepEqual(result, { version, ready: false, state: 'uploading' });
    }
  }
  assert.deepEqual(await nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(release()) }),
    { version, ready: true, state: 'available' });
});

test('an already published CLI still permits extension-only recovery without querying GitHub assets', async t => {
  const options = await fixture(t);
  const result = await nativeReleaseReadiness({ ...options, fetchImpl: async url => {
    assert.equal(url, registryUrl);
    return Response.json({ name: '@agent-wechat/cli', version });
  } });
  assert.deepEqual(result, { version, ready: true, state: 'published' });
});

test('registry failures, GitHub failures and invalid metadata fail instead of reporting a waiting release', async t => {
  const options = await fixture(t);
  await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async () => new Response(null, { status: 503 }) }), /HTTP 503/);
  for (const status of [401, 403, 429, 500]) {
    await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : new Response(null, { status }) }), new RegExp(`HTTP ${status}`));
  }
  for (const value of [{ ...release(), tag_name: 'v0.16.0' }, { ...release(), draft: true }, { ...release(), assets: null }]) {
    await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(value) }), /Invalid native release availability/);
  }
});

function originalRun(overrides = {}) {
  return { id: 123, head_sha: sourceCommit, event: 'push', head_branch: 'main', path: '.github/workflows/release.yml',
    repository: { full_name: releaseRepository }, head_repository: { full_name: releaseRepository },
    status: 'completed', conclusion: 'success', ...overrides };
}

test('native completion reruns the original push for the exact commit, including a successful waiting run', async () => {
  for (const conclusion of ['success', 'failure']) {
    const requests = [];
    const result = await resumeNpmRelease({ sourceCommit, token, fetchImpl: async (url, init) => {
      requests.push({ url, method: init.method });
      assert.equal(init.headers.Authorization, `Bearer ${token}`);
      assert.equal(init.redirect, 'error');
      if (init.method === 'GET') {
        const parsed = new URL(url);
        assert.equal(parsed.pathname, `/repos/${releaseRepository}/actions/workflows/release.yml/runs`);
        assert.equal(parsed.searchParams.get('head_sha'), sourceCommit);
        assert.equal(parsed.searchParams.get('event'), 'push');
        assert.equal(parsed.searchParams.get('branch'), 'main');
        return Response.json({ total_count: 1, workflow_runs: [originalRun({ conclusion })] });
      }
      assert.equal(url, `https://api.github.com/repos/${releaseRepository}/actions/runs/123/rerun`);
      return new Response(null, { status: 201 });
    } });
    assert.deepEqual(result, { runId: 123, sourceCommit });
    assert.equal(requests.length, 2);
    assert.equal(requests[1].method, 'POST');
  }
});

test('resume refuses another commit, branch, workflow, event or repository even if the API returns it', async () => {
  const invalid = [{ head_sha: 'b'.repeat(40) }, { head_branch: 'feature' }, { event: 'pull_request' },
    { path: '.github/workflows/native-release.yml' }, { repository: { full_name: 'other/repo' } },
    { head_repository: { full_name: 'other/repo' } }, { id: -1 }];
  for (const overrides of invalid) {
    await assert.rejects(resumeNpmRelease({ sourceCommit, fetchImpl: async (_url, init) => {
      assert.equal(init.method, 'GET');
      return Response.json({ total_count: 1, workflow_runs: [originalRun(overrides)] });
    } }), /exactly one original/);
  }
});

test('resume refuses missing, ambiguous, unfinished or unbounded run results', async () => {
  for (const value of [{ total_count: 0, workflow_runs: [] }, { total_count: 2, workflow_runs: [originalRun(), originalRun({ id: 124 })] },
    { total_count: 101, workflow_runs: [originalRun()] }, { total_count: 1 },
    { total_count: 1, workflow_runs: [originalRun({ status: 'in_progress' })] }]) {
    await assert.rejects(resumeNpmRelease({ sourceCommit, fetchImpl: async (_url, init) => {
      assert.equal(init.method, 'GET');
      return Response.json(value);
    } }), /original|unique/i);
  }
});

test('resume fails visibly on invalid source, API access errors and rejected reruns', async () => {
  await assert.rejects(resumeNpmRelease({ sourceCommit: 'main', fetchImpl: () => assert.fail('must not fetch') }), /full native release source commit/);
  await assert.rejects(resumeNpmRelease({ sourceCommit, fetchImpl: async () => new Response(null, { status: 403 }) }), /HTTP 403/);
  await assert.rejects(resumeNpmRelease({ sourceCommit, fetchImpl: async (_url, init) => init.method === 'GET'
    ? Response.json({ total_count: 1, workflow_runs: [originalRun()] }) : new Response(null, { status: 403 }) }), /HTTP 403/);
});


test('completion retries npm only after the native publication job actually succeeds', async () => {
  for (const conclusion of ['success', 'skipped', 'failure', 'cancelled']) {
    const result = await nativePublicationSucceeded({ runId: 123, fetchImpl: async url => {
      assert.ok(url.endsWith('/actions/runs/123/jobs?per_page=100'));
      return Response.json({ total_count: 2, jobs: [{ name: 'prepare', status: 'completed', conclusion: 'success' }, { name: 'image / manifest', status: 'completed', conclusion }] });
    } });
    assert.equal(result, conclusion === 'success');
  }
  assert.equal(await nativePublicationSucceeded({ runId: 123, fetchImpl: async () => Response.json({ total_count: 0, jobs: [] }) }), false);
  await assert.rejects(nativePublicationSucceeded({ runId: 123, fetchImpl: async () => new Response(null, { status: 403 }) }), /HTTP 403/);
});

function imageRun(workflow = 'native-release.yml', overrides = {}) {
  return { ...originalRun(), path: `.github/workflows/${workflow}`, ...overrides };
}

test('an image must finish its manifest publication for the exact source before npm is ready', async t => {
  const options = await fixture(t);
  for (const conclusion of ['success', 'skipped', 'failure']) {
    const result = await npmReleaseReadiness({ ...options, sourceCommit, fetchImpl: async (url, init) => {
      if (url === registryUrl) return missing();
      if (url === releaseUrl) return Response.json(release());
      if (url.includes('/native-release.yml/runs?')) return Response.json({ total_count: 1, workflow_runs: [imageRun()] });
      if (url.includes('/native-image.yml/runs?')) return Response.json({ total_count: 0, workflow_runs: [] });
      assert.ok(url.endsWith('/actions/runs/123/jobs?per_page=100'));
      assert.equal(init.headers.Authorization, `Bearer ${token}`);
      return Response.json({ total_count: 1, jobs: [{ name: 'image / manifest', status: 'completed', conclusion }] });
    } });
    assert.deepEqual(result, { version, ready: conclusion === 'success', state: conclusion === 'success' ? 'available' : 'image-pending' });
  }
});

test('unrelated or unfinished image workflows never unlock npm', async () => {
  for (const overrides of [{ head_sha: 'b'.repeat(40) }, { head_branch: 'feature' }, { event: 'pull_request' },
    { repository: { full_name: 'other/repo' } }, { head_repository: { full_name: 'other/repo' } },
    { status: 'in_progress' }, { conclusion: 'failure' }, { path: '.github/workflows/unrelated.yml' }]) {
    assert.equal(await runtimeImagePublished({ sourceCommit, fetchImpl: async url => {
      assert.ok(url.includes('/runs?'), 'must not read jobs for an unrelated run');
      return Response.json({ total_count: 1, workflow_runs: [imageRun('native-release.yml', overrides)] });
    } }), false);
  }
});

test('manual image recovery also satisfies the exact-source npm gate', async () => {
  assert.equal(await runtimeImagePublished({ sourceCommit, fetchImpl: async url => {
    if (url.includes('/native-release.yml/')) return Response.json({ total_count: 0, workflow_runs: [] });
    if (url.includes('/native-image.yml/')) return Response.json({ total_count: 1, workflow_runs: [imageRun('native-image.yml', { event: 'workflow_dispatch' })] });
    return Response.json({ total_count: 1, jobs: [{ name: 'manifest', status: 'completed', conclusion: 'success' }] });
  } }), true);
});

test('image status errors fail visibly instead of unlocking publication', async () => {
  await assert.rejects(runtimeImagePublished({ sourceCommit: 'main' }), /full image source commit/);
  await assert.rejects(runtimeImagePublished({ sourceCommit, fetchImpl: async () => new Response(null, { status: 403 }) }), /HTTP 403/);
  await assert.rejects(runtimeImagePublished({ sourceCommit, fetchImpl: async () => Response.json({ total_count: 101, workflow_runs: [] }) }), /establish/);
});
