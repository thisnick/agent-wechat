import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { recordArtifact, restoreArtifact } from './native-ci-artifact.mjs';
import { assetName, manifestName, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';

async function fixture(t, target = supportedTargets[0]) {
  const root = await mkdtemp(join(tmpdir(), 'native-ci-artifact-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const directory = join(root, 'artifact');
  await mkdir(directory);
  await mkdir(join(root, 'packages/cli'), { recursive: true });
  const version = '1.2.3';
  await writeFile(join(root, 'packages/cli/package.json'), JSON.stringify({ version }));
  await writeFile(join(root, 'rust-toolchain.toml'), '[toolchain]\nchannel = "1.93.1"\n');
  const bytes = Buffer.from('synthetic executable');
  const asset = { ...target, name: assetName(version, target), bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
  const options = { root, target, compiler: 'rustc 1.93.1 (fixture)', sourceCommit: 'a'.repeat(40), rustflags: target.platform === 'win32' ? '-C target-feature=+crt-static' : '' };
  const manifest = { schemaVersion: 1, version, repository: 'thisnick/agent-wechat', sourceCommit: options.sourceCommit, sourceTreeDirty: false, assets: [asset] };
  await writeFile(join(directory, asset.name), bytes);
  const manifestPath = join(directory, manifestName(version));
  await writeFile(manifestPath, JSON.stringify(manifest));
  const destination = join(root, 'restored', target.platform === 'win32' ? 'wx.exe' : 'wx');
  await recordArtifact(directory, options);
  return { directory, options, asset, manifest, manifestPath, destination, bytes };
}

test('every supported target restores the exact verified CLI bytes and checks runtime identity', async t => {
  for (const target of supportedTargets) {
    const f = await fixture(t, target);
    let inspected = false;
    await restoreArtifact(f.directory, f.destination, { ...f.options, inspect: async (path, version) => {
      assert.equal(path, f.destination);
      assert.equal(version, '1.2.3');
      assert.deepEqual(await readFile(path), f.bytes);
      inspected = true;
    } });
    assert.ok(inspected);
  }
});

test('stale source, dirty builds, compiler/flag/target changes and corrupted bytes fail before installation', async t => {
  for (const mutate of [
    f => { f.options.sourceCommit = 'b'.repeat(40); },
    async f => { f.manifest.sourceTreeDirty = true; await writeFile(f.manifestPath, JSON.stringify(f.manifest)); },
    f => { f.options.compiler = 'rustc 1.93.1 (different compiler)'; },
    f => { f.options.compiler = 'rustc 1.94.0 (different version)'; },
    f => { f.options.rustflags = '-C target-cpu=native'; },
    f => { f.options.target = supportedTargets[1]; },
    async f => { await writeFile(join(f.directory, f.asset.name), 'changed'); },
    async f => { await rm(join(f.directory, 'native-ci-build.json')); },
  ]) {
    const f = await fixture(t);
    await mutate(f);
    await assert.rejects(restoreArtifact(f.directory, f.destination, { ...f.options, inspect: () => assert.fail('must reject before execution') }));
    await assert.rejects(stat(f.destination), { code: 'ENOENT' });
  }
});

test('a runtime identity failure removes the rejected CLI without overwriting an existing destination', async t => {
  const f = await fixture(t);
  await assert.rejects(restoreArtifact(f.directory, f.destination, { ...f.options, inspect: async () => { throw Error('wrong runtime'); } }), /wrong runtime/);
  await assert.rejects(stat(f.destination), { code: 'ENOENT' });
  await mkdir(join(f.destination, '..'), { recursive: true });
  await writeFile(f.destination, 'existing');
  await assert.rejects(restoreArtifact(f.directory, f.destination, f.options), { code: 'EEXIST' });
  assert.equal(await readFile(f.destination, 'utf8'), 'existing');
});
