import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { assetName, manifestName, releaseRepository, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';
import { mergeNativeRelease } from './native-merge-release.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'native-matrix-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const directories = [];
  for (const target of supportedTargets) {
    const dir = join(root, target.arch); await mkdir(dir); directories.push(dir);
    const bytes = Buffer.from(`fixture-${target.arch}`);
    const manifest = { schemaVersion: 1, version: '1.0.0', repository: releaseRepository,
      sourceCommit: 'a'.repeat(40), sourceTreeDirty: false,
      assets: [{ ...target, name: assetName('1.0.0', target), sha256: createHash('sha256').update(bytes).digest('hex'), bytes: bytes.length }] };
    await writeFile(join(dir, manifest.assets[0].name), bytes);
    await writeFile(join(dir, manifestName('1.0.0')), JSON.stringify(manifest));
    await writeFile(join(dir, 'wx-v1.0.0-THIRD-PARTY-NOTICES.txt'), 'fixture notices');
  }
  return { root, directories, output: join(root, 'merged') };
}
test('merge verifies distinct target bytes, notices and source before publication', async t => {
  const { directories, output } = await fixture(t);
  const merged = await mergeNativeRelease(output, directories);
  assert.deepEqual(merged.assets.map(asset => asset.arch).sort(), ['arm64', 'x64']);
  for (const asset of merged.assets) assert.equal((await readFile(join(output, asset.name))).toString(), `fixture-${asset.arch}`);
  await assert.rejects(mergeNativeRelease(output, directories), /empty/);
});
test('merge rejects missing/duplicate targets, source mismatch and corrupted bytes', async t => {
  const { directories, output } = await fixture(t);
  await assert.rejects(mergeNativeRelease(output, [directories[0]]), /one artifact directory/);
  await assert.rejects(mergeNativeRelease(output, [directories[0], directories[0]]), /duplicate/);
  const path = join(directories[1], manifestName('1.0.0'));
  const manifest = JSON.parse(await readFile(path)); manifest.sourceCommit = 'b'.repeat(40);
  await writeFile(path, JSON.stringify(manifest));
  await assert.rejects(mergeNativeRelease(output, directories), /same version and source/);
  manifest.sourceCommit = 'a'.repeat(40); await writeFile(path, JSON.stringify(manifest));
  await writeFile(join(directories[1], manifest.assets[0].name), 'corrupt');
  await assert.rejects(mergeNativeRelease(output, directories), /SHA-256/);
});
