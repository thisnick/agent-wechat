import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { assetName, manifestName, releaseRepository, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';
import { nativeReleaseReadiness } from './native-release-orchestration.mjs';

const version = '1.0.0-beta.10';
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

test('release availability keeps GitHub credentials out of npm requests', async t => {
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
  assert.deepEqual(result, { version, state: 'missing' });
});

test('complete release availability requires every target, manifest and notice', async t => {
  const options = await fixture(t);
  for (const asset of assets) {
    for (const state of ['missing', 'starter']) {
      const value = release();
      value.assets = state === 'missing' ? value.assets.filter(item => item.name !== asset.name)
        : value.assets.map(item => item.name === asset.name ? { ...item, state } : item);
      const result = await nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(value) });
      assert.deepEqual(result, { version, state: 'uploading' });
    }
  }
  assert.deepEqual(await nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(release()) }),
    { version, state: 'available' });
});

test('an already published CLI blocks a new native build without querying GitHub assets', async t => {
  const options = await fixture(t);
  const result = await nativeReleaseReadiness({ ...options, fetchImpl: async url => {
    assert.equal(url, registryUrl);
    return Response.json({ name: '@agent-wechat/cli', version });
  } });
  assert.deepEqual(result, { version, state: 'published' });
});

test('registry failures, GitHub failures and invalid metadata fail the release check', async t => {
  const options = await fixture(t);
  await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async () => new Response(null, { status: 503 }) }), /HTTP 503/);
  for (const status of [401, 403, 429, 500]) {
    await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : new Response(null, { status }) }), new RegExp(`HTTP ${status}`));
  }
  for (const value of [{ ...release(), tag_name: 'v0.16.0' }, { ...release(), draft: true }, { ...release(), assets: null }]) {
    await assert.rejects(nativeReleaseReadiness({ ...options, fetchImpl: async url => url === registryUrl ? missing() : Response.json(value) }), /Invalid native release availability/);
  }
});
