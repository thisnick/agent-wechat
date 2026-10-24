import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { releaseRepository } from '../packages/cli/scripts/native-runtime.mjs';
import { isVersionPackageMerge, nativeReleaseTrigger } from './native-release-trigger.mjs';
const sourceCommit = 'a'.repeat(40);
const repo = { full_name: releaseRepository };
const pr = { merged_at: '2026-10-23T00:00:00Z', merge_commit_sha: sourceCommit,
  base: { ref: 'main', repo }, head: { ref: 'changeset-release/main', repo } };
const push = { eventName: 'push', event: { ref: 'refs/heads/main' }, sourceCommit };

test('only a merged same-repository Changesets PR at the exact push commit qualifies', async () => {
  assert.equal(await isVersionPackageMerge({ sourceCommit, fetchImpl: async () => Response.json([pr]) }), true);
  for (const invalid of [{ ...pr, merged_at: null }, { ...pr, merge_commit_sha: 'b'.repeat(40) },
    { ...pr, head: { ref: 'changeset-release/main', repo: { full_name: 'fork/repo' } } },
    { ...pr, head: { ref: 'feature', repo } }, { ...pr, base: { ref: 'other', repo } }]) {
    assert.equal(await isVersionPackageMerge({ sourceCommit, fetchImpl: async () => Response.json([invalid]) }), false);
  }
});

test('ordinary pushes and other events never query release availability or publish', async () => {
  assert.deepEqual(await nativeReleaseTrigger({ ...push, fetchImpl: async () => Response.json([]) }), { publish: false });
  for (const eventName of ['pull_request', 'workflow_run']) {
    assert.deepEqual(await nativeReleaseTrigger({ ...push, eventName, fetchImpl: () => assert.fail('must not fetch') }), { publish: false });
  }
});

test('a version merge starts assets only when the CLI and native release are unpublished', async t => {
  const root = await mkdtemp(join(tmpdir(), 'native-trigger-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const version = '1.0.0-beta.13';
  await writeFile(join(root, 'package.json'), JSON.stringify({ name: '@agent-wechat/cli', version }));
  for (const published of [false, true]) {
    const result = await nativeReleaseTrigger({ ...push, root, token: 'fixture-token', fetchImpl: async (url, init) => {
      if (url.includes('/pulls?')) return Response.json([pr]);
      if (url.startsWith('https://registry.npmjs.org/')) {
        assert.equal(init.headers, undefined);
        return published ? Response.json({ name: '@agent-wechat/cli', version }) : new Response(null, { status: 404 });
      }
      assert.ok(url.endsWith(`/releases/tags/v${version}`));
      return new Response(null, { status: 404 });
    } });
    assert.deepEqual(result, { publish: !published, version });
  }
});

test('manual recovery requires confirmation and main', async () => {
  for (const ref of ['refs/heads/main', 'refs/heads/feature']) {
    for (const confirm_publish of [false, 'false', true, 'true']) {
      const result = await nativeReleaseTrigger({ eventName: 'workflow_dispatch', event: { ref, inputs: { confirm_publish, version: '1.2.3' } } });
      assert.deepEqual(result, ref === 'refs/heads/main' && [true, 'true'].includes(confirm_publish)
        ? { publish: true, version: '1.2.3' } : { publish: false });
    }
  }
});

test('API errors, invalid metadata and ambiguous results fail before building', async () => {
  await assert.rejects(isVersionPackageMerge({ sourceCommit: 'main' }), /full source commit/);
  await assert.rejects(isVersionPackageMerge({ sourceCommit, fetchImpl: async () => new Response(null, { status: 403 }) }), /HTTP 403/);
  for (const value of [{}, Array(100).fill(pr)]) {
    await assert.rejects(isVersionPackageMerge({ sourceCommit, fetchImpl: async () => Response.json(value) }), /establish/);
  }
});
