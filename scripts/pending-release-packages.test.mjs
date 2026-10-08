import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pendingReleasePackages } from './pending-release-packages.mjs';

const packages = [
  { name: '@agent-wechat/cli', version: '0.15.4' },
  { name: '@agent-wechat/agent-wechat', version: '0.15.4' },
  { name: '@agent-wechat/wechaty-puppet', version: '0.15.4' },
];
const env = { GITHUB_REPOSITORY: 'thisnick/agent-wechat', GH_TOKEN: 'fixture-token' };

async function fixture(t, manifests = packages) {
  const cwd = await mkdtemp(join(tmpdir(), 'agent-wechat-release-recovery-'));
  t.after(() => rm(cwd, { recursive: true, force: true }));
  for (const [index, pkg] of [...manifests,
    { name: '@agent-wechat/shared', version: '0.3.1', private: true }].entries()) {
    const dir = join(cwd, 'packages', `package-${index}`);
    await mkdir(dir, { recursive: true });
    await writeFile(join(dir, 'package.json'), JSON.stringify(pkg));
  }
  return cwd;
}

test('already published npm packages still start artifact jobs when their release is missing', async t => {
  const cwd = await fixture(t);
  const result = await pendingReleasePackages({ cwd, env, fetchImpl: async (url, options) => {
    assert.equal(url, 'https://api.github.com/repos/thisnick/agent-wechat/releases/tags/v0.15.4');
    assert.equal(options.headers.Authorization, 'Bearer fixture-token');
    return { status: 404 };
  } });
  assert.deepEqual(result, packages);
});

test('ordinary pushes do not rebuild an existing release', async t => {
  const cwd = await fixture(t);
  assert.deepEqual(await pendingReleasePackages({ cwd, env,
    fetchImpl: async () => ({ status: 200 }) }), []);
});

test('GitHub authentication and network errors do not trigger a release', async t => {
  const cwd = await fixture(t);
  for (const status of [401, 403, 429, 500]) {
    await assert.rejects(pendingReleasePackages({ cwd, env,
      fetchImpl: async () => ({ status }) }), new RegExp(`HTTP ${status}`));
  }
  await assert.rejects(pendingReleasePackages({ cwd, env,
    fetchImpl: async () => { throw new Error('network error'); } }), /network error/);
});

test('invalid package versions fail before looking up the release', async t => {
  for (const manifests of [[], [{ name: 'other-package', version: '0.15.4' }],
    [{ ...packages[0], version: 'latest' }], [packages[0], { ...packages[1], version: '0.15.3' }]]) {
    const cwd = await fixture(t, manifests);
    await assert.rejects(pendingReleasePackages({ cwd, env,
      fetchImpl: () => { throw new Error('must not fetch'); } }), /shared version/);
  }
});
