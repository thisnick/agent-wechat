import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { crateReleaseTrigger, prepareCrateRelease, finishCrateRelease } from './native-crates-release.mjs';
import { cratePackage } from './native-crates-version.mjs';

const sourceCommit = 'a'.repeat(40), version = '2.0.1';
const repo = { full_name: 'thisnick/agent-wechat' };
const pr = { merged_at: '2026-11-07T00:00:00Z', merge_commit_sha: sourceCommit,
  base: { ref: 'main', repo }, head: { ref: 'changeset-release/main', repo } };
const push = { eventName: 'push', event: { ref: 'refs/heads/main' }, sourceCommit };
const missing = () => new Response(null, { status: 404 });

async function fixture(t, v = version) {
  const root = await mkdtemp(join(tmpdir(), 'wechat-crate-release-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, cratePackage.directory), { recursive: true });
  await writeFile(join(root, cratePackage.directory, 'package.json'), JSON.stringify({ name: cratePackage.name, version: v, private: true }));
  await writeFile(join(root, cratePackage.directory, 'CHANGELOG.md'), `# Rust libraries\n\n## ${v}\n\n### Patch Changes\n\nLibrary changes.\n`);
  return { root, version: v, sourceCommit, token: 'synthetic-token' };
}

test('only a version-PR merge that changes the library version starts publication', async t => {
  const options = await fixture(t);
  for (const previousVersion of [version, '2.0.0']) {
    assert.deepEqual(await crateReleaseTrigger({ ...push, ...options, previousVersion,
      fetchImpl: async () => Response.json([pr]) }), { publish: previousVersion !== version, version });
  }
  const never = () => assert.fail('ordinary pushes must not read a previous library version');
  assert.deepEqual(await crateReleaseTrigger({ ...push, ...options, previousVersion: never,
    fetchImpl: async () => Response.json([]) }), { publish: false });
  await assert.rejects(crateReleaseTrigger({ ...push, ...options,
    fetchImpl: async () => Response.json([pr]) }), /Previous Rust library version/);
});

test('manual library recovery needs main, confirmation and the exact committed version', async t => {
  const options = await fixture(t);
  for (const ref of ['refs/heads/main', 'refs/heads/feature']) {
    for (const confirm_publish of [false, true]) {
      const result = await crateReleaseTrigger({ ...options, eventName: 'workflow_dispatch',
        event: { ref, inputs: { confirm_publish, crate_version: version } } });
      assert.deepEqual(result, ref === 'refs/heads/main' && confirm_publish ? { publish: true, version } : { publish: false });
    }
  }
  await assert.rejects(crateReleaseTrigger({ ...options, eventName: 'workflow_dispatch',
    event: { ref: 'refs/heads/main', inputs: { confirm_publish: true, crate_version: '9.0.0' } } }), /version mismatch/);
  assert.deepEqual(await crateReleaseTrigger({ ...options, eventName: 'workflow_dispatch',
    event: { ref: 'refs/heads/main', inputs: { confirm_publish: true, version } } }), { publish: false });
});

test('preparation creates only a missing immutable library tag and validates the source on recovery', async t => {
  const options = await fixture(t);
  let tagged = false;
  const writes = [];
  const fetchImpl = async (url, init) => {
    if (init.method === 'POST') {
      writes.push(JSON.parse(init.body)); tagged = true;
      assert.ok(url.endsWith('/git/refs'));
      return Response.json({}, { status: 201 });
    }
    if (url.includes('/releases/tags/')) return missing();
    assert.ok(url.endsWith(`/git/ref/tags/crates-v${version}`));
    return tagged ? Response.json({ object: { type: 'commit', sha: sourceCommit } }) : missing();
  };
  await prepareCrateRelease({ ...options, fetchImpl });
  await prepareCrateRelease({ ...options, fetchImpl });
  assert.deepEqual(writes, [{ ref: `refs/tags/crates-v${version}`, sha: sourceCommit }]);
  await assert.rejects(prepareCrateRelease({ ...options, sourceCommit: 'b'.repeat(40), fetchImpl }), /differs from crate source/);
  await assert.rejects(prepareCrateRelease({ ...options, fetchImpl: async () => new Response(null, { status: 503 }) }), /HTTP 503/);
});

test('stable and beta library release notes never take over the latest application release', async t => {
  for (const v of [version, '2.1.0-beta.0']) {
    const options = await fixture(t, v);
    const writes = [];
    let existing;
    const fetchImpl = async (url, init) => {
      if (init.method === 'POST') {
        assert.ok(url.endsWith('/releases'));
        existing = JSON.parse(init.body); writes.push(existing);
        return Response.json({}, { status: 201 });
      }
      if (url.includes('/git/ref/')) return Response.json({ object: { type: 'commit', sha: sourceCommit } });
      assert.ok(url.endsWith(`/releases/tags/crates-v${v}`));
      return existing ? Response.json(existing) : missing();
    };
    await finishCrateRelease({ ...options, fetchImpl });
    await finishCrateRelease({ ...options, fetchImpl });
    assert.equal(writes.length, 1);
    assert.equal(writes[0].make_latest, 'false');
    assert.equal(writes[0].prerelease, v.includes('-'));
    assert.equal(writes[0].target_commitish, sourceCommit);
    assert.match(writes[0].body, /Library changes/);
    existing.prerelease = !existing.prerelease;
    await assert.rejects(finishCrateRelease({ ...options, fetchImpl }), /channel mismatch/);
  }
});

test('a missing or empty versioned changelog fails before any release write', async t => {
  const options = await fixture(t);
  await writeFile(join(options.root, cratePackage.directory, 'CHANGELOG.md'), '# Rust libraries\n\n## 0.0.1\n\nOld entry.\n');
  await assert.rejects(prepareCrateRelease({ ...options, fetchImpl: () => assert.fail('must not write') }), /exact changelog/);
});
