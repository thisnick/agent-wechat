#!/usr/bin/env node
// Called before publication: immutable npm metadata anchors the release binary hash.
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { mkdtemp } from 'node:fs/promises';
import { fetchBytes, inspectBinary, manifestName, releaseUrl, validateManifest, verifyBytes } from '../packages/cli/scripts/native-runtime.mjs';
export async function nativePackageUnpublished({ root = resolve('packages/cli'), fetchImpl = fetch } = {}) {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  if (pkg.name !== '@agent-wechat/cli' || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(pkg.version)) throw new Error('Invalid CLI publication identity');
  const response = await fetchImpl(`https://registry.npmjs.org/@agent-wechat%2fcli/${pkg.version}`, { redirect: 'error', signal: AbortSignal.timeout(10000) });
  if (response.status === 404) return true;
  if (response.status !== 200) throw new Error(`Cannot check native CLI publication (HTTP ${response.status})`);
  const published = await response.json();
  if (published.name !== pkg.name || published.version !== pkg.version) throw new Error('Registry returned mismatched CLI publication identity');
  return false;
}
export async function prepareNativePackage({ root = resolve('packages/cli'), sourceCommit = process.env.GITHUB_SHA,
  fetchOptions, inspect = inspectBinary } = {}) {
  const { version } = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full source commit is required before CLI publication');
  const data = await fetchBytes(releaseUrl(version, manifestName(version)), 65536, fetchOptions);
  const manifest = JSON.parse(data);
  const asset = validateManifest(manifest, version, { requireAllTargets: true });
  if (manifest.sourceTreeDirty === true) throw new Error('Dirty local validation artifacts cannot be published');
  if (manifest.sourceCommit !== sourceCommit) throw new Error('Prepared native release does not match the publication source commit');
  const temporary = await mkdtemp(join(tmpdir(), 'agent-wechat-native-release-'));
  try {
    for (const item of manifest.assets) {
      const binary = await fetchBytes(releaseUrl(version, item.name), item.bytes, fetchOptions);
      verifyBytes(binary, item);
      if (item.name === asset.name) {
        const path = join(temporary, 'wx');
        await writeFile(path, binary, { mode: 0o755, flag: 'wx' });
        await inspect(path, version);
      }
    }
  } finally { await rm(temporary, { recursive: true, force: true }); }
  await mkdir(root, { recursive: true });
  await writeFile(join(root, 'native-release.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  return manifest;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  if (process.argv.includes('--if-unpublished') && !await nativePackageUnpublished()) {
    console.error('CLI version is already published; ordinary/recovery pushes do not require a new native artifact set.');
  } else {
    await prepareNativePackage();
    console.error('Verified native release assets; npm package now contains their exact checksums.');
  }
}
