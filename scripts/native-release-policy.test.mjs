import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { publicPackages, readReleasePolicy, releaseChannel, requireStablePublicPackages } from './native-release-policy.mjs';

test('stable and beta versions select distinct npm and GitHub release channels', () => {
  assert.deepEqual(releaseChannel('1.0.0'), { version: '1.0.0', distTag: 'latest', prerelease: false });
  assert.deepEqual(releaseChannel('1.0.0-beta.0', { mode: 'pre', tag: 'beta' }), { version: '1.0.0-beta.0', distTag: 'beta', prerelease: true });
  assert.equal(releaseChannel('1.0.0-rc.1', { mode: 'pre', tag: 'rc' }).distTag, 'rc');
});
test('a missing, mismatched or exiting prerelease state cannot publish a beta as latest', () => {
  for (const state of [undefined, { mode: 'exit', tag: 'beta' }, { mode: 'pre', tag: 'alpha' }, { mode: 'pre', tag: 'latest' }]) {
    assert.throws(() => releaseChannel('1.0.0-beta.0', state), /matching Changesets/);
  }
  assert.throws(() => releaseChannel('0.16.0', { mode: 'pre', tag: 'beta' }), /version PR first/);
  for (const version of ['1.0.0.beta-1', '01.0.0', '1.0.0-beta.01', '1.0.0+metadata', '1.0.0-beta.0\nunsafe', '1.0.0-beta.0\n']) {
    assert.throws(() => releaseChannel(version, { mode: 'pre', tag: 'beta' }), /Invalid release version/);
  }
});
test('read-only beta registry guard requires both existing public stable lines and fails closed', async () => {
  const policy = releaseChannel('1.0.0-beta.0', { mode: 'pre', tag: 'beta' });
  const requested = [];
  await requireStablePublicPackages(policy, async (url, options) => {
    requested.push(url); assert.equal(options.redirect, 'error'); assert.equal(options.headers, undefined);
    const name = publicPackages.find(([, value]) => url.includes(value.replace('/', '%2f')))?.[1];
    return Response.json({ name, version: '0.16.0' });
  });
  assert.equal(requested.length, 2);
  for (const response of [new Response(null, { status: 503 }), Response.json({ name: '@agent-wechat/cli', version: '1.0.0-beta.0' })]) {
    await assert.rejects(requireStablePublicPackages(policy, async () => response.clone()), /stable npm latest/);
  }
  await requireStablePublicPackages(releaseChannel('1.0.0'), async () => { throw new Error('stable release must not query beta guard'); });
});
test('versioned package files and pre-state must agree before CI can publish', async t => {
  const root = await mkdtemp(join(tmpdir(), 'native-release-policy-')); t.after(() => rm(root, { recursive: true, force: true }));
  for (const [directory, name] of publicPackages) {
    await mkdir(join(root, directory), { recursive: true });
    await writeFile(join(root, directory, 'package.json'), JSON.stringify({ name, version: '1.0.0-beta.0' }));
  }
  await assert.rejects(readReleasePolicy({ root }), /matching Changesets/);
  await mkdir(join(root, '.changeset'));
  await writeFile(join(root, '.changeset/pre.json'), JSON.stringify({ mode: 'pre', tag: 'beta' }));
  assert.equal((await readReleasePolicy({ root })).distTag, 'beta');
  await assert.rejects(readReleasePolicy({ root, version: '1.0.0' }), /package\/version mismatch/);
});
