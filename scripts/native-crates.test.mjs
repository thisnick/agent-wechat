import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { crateNames, isReleaseContext, publicationPlan, syncCrateVersions, validateCrates, verifyReleaseSource } from './native-crates.mjs';

const version = '1.4.0-beta.2';
const sourceCommit = 'a'.repeat(40);
const bytes = Buffer.from('synthetic crate archive');
const checksum = createHash('sha256').update(bytes).digest('hex');
const checksums = Object.fromEntries(crateNames.map(name => [name, checksum]));
const missing = () => new Response(null, { status: 404 });
const readSource = async (_bytes, name) => ({ git: { sha1: sourceCommit }, path_in_vcs: `packages/native-client-rust/crates/${name}` });

test('only the main-branch native release workflow can publish', () => {
  const env = { GITHUB_REPOSITORY: 'thisnick/agent-wechat', GITHUB_REF: 'refs/heads/main',
    GITHUB_EVENT_NAME: 'push', GITHUB_WORKFLOW_REF: 'thisnick/agent-wechat/.github/workflows/native-release.yml@refs/heads/main' };
  assert.equal(isReleaseContext(env), true);
  assert.equal(isReleaseContext({ ...env, GITHUB_EVENT_NAME: 'workflow_dispatch' }), true);
  for (const changed of [{ GITHUB_EVENT_NAME: 'pull_request' }, { GITHUB_EVENT_NAME: 'workflow_run' },
    { GITHUB_REPOSITORY: 'example/fork' }, { GITHUB_REF: 'refs/heads/feature' },
    { GITHUB_WORKFLOW_REF: 'thisnick/agent-wechat/.github/workflows/unrelated.yml@refs/heads/main' }]) {
    assert.equal(isReleaseContext({ ...env, ...changed }), false);
  }
});
function metadata() {
  const packages = crateNames.map(name => ({ id: name, name, version, publish: ['crates-io'],
    rust_version: '1.93.1', description: name, readme: 'README.md', license: 'MIT',
    repository: 'https://github.com/thisnick/agent-wechat', dependencies: name === 'wechat-platform' ? []
      : [{ name: 'wechat-platform', path: '/fixture/wechat-platform', req: `=${version}` }] }));
  packages.push({ id: 'wechat-core', name: 'wechat-core', publish: [] });
  return { packages, workspace_members: packages.map(pkg => pkg.id) };
}
function published(name) {
  return Response.json({ version: { crate: name, num: version, yanked: false, checksum } });
}
async function registry(url) {
  if (url.startsWith('https://static.crates.io/')) return new Response(bytes);
  const name = url.split('/')[6];
  return published(name);
}

test('only selected libraries publish, with aligned versions and registry dependencies', () => {
  assert.deepEqual(validateCrates(metadata(), version), crateNames);
  const badVersion = metadata(); badVersion.packages[0].version = '1.2.0';
  assert.throws(() => validateCrates(badVersion, version), /version or registry/);
  const leaked = metadata(); leaked.packages.at(-1).publish = null;
  assert.throws(() => validateCrates(leaked, version), /Only the four/);
  const leakedDependency = metadata(); leakedDependency.packages[1].dependencies[0].name = 'wechat-core';
  assert.throws(() => validateCrates(leakedDependency, version), /dependency must be published/);
  const looseDependency = metadata(); looseDependency.packages[1].dependencies[0].req = version;
  assert.throws(() => validateCrates(looseDependency, version), /same exact version/);
});

test('version cutting updates internal public dependencies for beta and stable releases', async t => {
  const root = await mkdtemp(join(tmpdir(), 'wechat-crate-versions-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  for (const name of crateNames) {
    const directory = join(root, 'packages/native-client-rust/crates', name);
    await mkdir(directory, { recursive: true });
    await writeFile(join(directory, 'Cargo.toml'), '[dependencies]\nwechat-platform = { path = "../wechat-platform", version = "=1.0.0" }\nopenssl = "0.10"\n');
  }
  for (const next of [version, '2.0.0']) {
    await syncCrateVersions(root, next);
    const text = await readFile(join(root, 'packages/native-client-rust/crates/wechat-profile/Cargo.toml'), 'utf8');
    assert.ok(text.includes(`version = "=${next}"`));
    assert.ok(text.includes('openssl = "0.10"'));
  }
});

test('new names require bootstrap, while registry outages never look like missing packages', async () => {
  const plan = await publicationPlan({ version, sourceCommit, checksums, fetchImpl: async () => missing() });
  assert.deepEqual(plan.pending, crateNames);
  assert.deepEqual(plan.newCrates, crateNames);
  assert.equal(plan.bootstrap, true);
  await assert.rejects(publicationPlan({ version, sourceCommit, fetchImpl: async () => new Response(null, { status: 503 }) }), /HTTP 503/);
});

test('existing crate names from another project are rejected', async () => {
  await assert.rejects(publicationPlan({ version, sourceCommit, fetchImpl: async url => url.endsWith(version) ? missing()
    : Response.json({ crate: { repository: 'https://github.com/example/other' } }) }), /different project/);
});

test('a partial publication resumes only missing versions and does not require bootstrap for existing names', async () => {
  const plan = await publicationPlan({ version, sourceCommit, checksums, readSource, fetchImpl: async url => {
    if (url.includes('wechat-profile')) return url.endsWith(version) ? missing()
      : Response.json({ crate: { repository: 'https://github.com/thisnick/agent-wechat' } });
    return registry(url);
  } });
  assert.deepEqual(plan.pending, ['wechat-profile']);
  assert.equal(plan.bootstrap, false);
});

test('reruns verify checksum, exact packaged bytes and source before skipping published versions', async () => {
  const options = { version, sourceCommit, checksums, readSource, fetchImpl: registry };
  assert.deepEqual((await publicationPlan(options)).pending, []);
  await assert.rejects(publicationPlan({ ...options, checksums: { ...checksums, 'wechat-platform': 'b'.repeat(64) } }), /checksum mismatch/);
  await assert.rejects(publicationPlan({ ...options, readSource: async () => ({ git: { sha1: 'b'.repeat(40) } }) }), /another source/);
  await assert.rejects(publicationPlan({ ...options, fetchImpl: async url => url.startsWith('https://static.') ? new Response('changed') : registry(url) }), /checksum mismatch/);
});

test('release guards require the exact tag source and matching stable/beta channel', async () => {
  const options = { version, sourceCommit, prerelease: true, token: 'synthetic-token', fetchImpl: async (url, init) => {
    assert.equal(init.headers.Authorization, 'Bearer synthetic-token');
    assert.ok(url.startsWith('https://api.github.com/'));
    return url.includes('/git/ref/') ? Response.json({ object: { type: 'commit', sha: sourceCommit } })
      : Response.json({ tag_name: `v${version}`, draft: false, prerelease: true });
  } };
  await verifyReleaseSource(options);
  await assert.rejects(verifyReleaseSource({ ...options, sourceCommit: 'c'.repeat(40) }), /differs from crate source/);
  await assert.rejects(verifyReleaseSource({ ...options, prerelease: false }), /channel mismatch/);
  await assert.rejects(verifyReleaseSource({ ...options, fetchImpl: async () => missing() }), /matching published/);
});
