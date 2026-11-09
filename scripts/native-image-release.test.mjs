import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { assetName, manifestName, releaseRepository, supportedTargets, targetFor } from '../packages/cli/scripts/native-runtime.mjs';
import { stageReleaseImageBinary } from './native-image-release.mjs';

async function fixture(t, arch = 'x64', { dirty = false, corrupt = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'native-image-release-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const version = '2.1.0';
  const sourceCommit = 'a'.repeat(40);
  const target = targetFor({ platform: 'linux', arch, glibc: '2.36' });
  const binary = Buffer.from(assetName(version, target));
  const assets = supportedTargets.map(item => {
    const name = assetName(version, item);
    return { ...item, name, bytes: Buffer.byteLength(name), sha256: createHash('sha256').update(name).digest('hex') };
  });
  const manifest = Buffer.from(JSON.stringify({ schemaVersion: 1, version, repository: releaseRepository,
    sourceCommit, sourceTreeDirty: dirty, assets }, null, 2) + '\n');
  const requests = [];
  const fetchImpl = async url => {
    const name = url.pathname.split('/').pop();
    requests.push(name);
    if (name === manifestName(version)) return new Response(manifest);
    assert.equal(name, assetName(version, target));
    return new Response(corrupt ? Buffer.alloc(binary.length) : binary);
  };
  return { options: { version, sourceCommit, target, directory: join(root, 'image'), fetchOptions: { fetchImpl } },
    binary, manifest, requests };
}

test('image staging fetches only the matching Linux executable and preserves the attested bytes', async t => {
  for (const arch of ['x64', 'arm64']) {
    const { options, binary, manifest, requests } = await fixture(t, arch);
    await stageReleaseImageBinary(options);
    assert.deepEqual(requests, [manifestName(options.version), assetName(options.version, options.target)]);
    assert.deepEqual(await readFile(join(options.directory, 'manifest.json')), manifest);
    assert.deepEqual(await readFile(join(options.directory, 'wx')), binary);
  }
});

test('wrong source, dirty artifacts and a bad checksum never leave an image context', async t => {
  const wrongSource = await fixture(t);
  await assert.rejects(stageReleaseImageBinary({ ...wrongSource.options, sourceCommit: 'b'.repeat(40) }), /exact clean release commit/);
  assert.equal(wrongSource.requests.length, 1);
  await assert.rejects(stat(wrongSource.options.directory), { code: 'ENOENT' });

  const dirty = await fixture(t, 'x64', { dirty: true });
  await assert.rejects(stageReleaseImageBinary(dirty.options), /exact clean release commit/);
  assert.equal(dirty.requests.length, 1);
  await assert.rejects(stat(dirty.options.directory), { code: 'ENOENT' });

  const corrupt = await fixture(t, 'arm64', { corrupt: true });
  await assert.rejects(stageReleaseImageBinary(corrupt.options), /SHA-256/);
  await assert.rejects(stat(corrupt.options.directory), { code: 'ENOENT' });
});
