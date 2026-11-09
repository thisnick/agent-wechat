#!/usr/bin/env node
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { fetchBytes, manifestName, packageRoot, releaseUrl, targetFor, validateManifest, verifyBytes } from '../packages/cli/scripts/native-runtime.mjs';

/** Stage the verified bytes for one Linux architecture in a fresh build context. */
export async function stageReleaseImageBinary({ version, sourceCommit, directory, target = targetFor(), fetchOptions }) {
  if (target.platform !== 'linux') throw new Error('Native release images require a Linux target');
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full source commit is required before image publication');

  const manifestBytes = await fetchBytes(releaseUrl(version, manifestName(version)), 65_536, fetchOptions);
  const manifest = JSON.parse(manifestBytes);
  const asset = validateManifest(manifest, version, { target, requireAllTargets: true });
  if (manifest.sourceCommit !== sourceCommit || manifest.sourceTreeDirty) {
    throw new Error('Native image input must come from the exact clean release commit');
  }
  const binary = await fetchBytes(releaseUrl(version, asset.name), asset.bytes, { ...fetchOptions, timeoutMs: 300_000 });
  verifyBytes(binary, asset);

  await mkdir(directory, { mode: 0o700 });
  try {
    await writeFile(join(directory, 'manifest.json'), manifestBytes, { flag: 'wx' });
    await writeFile(join(directory, 'wx'), binary, { flag: 'wx', mode: 0o755 });
  } catch (error) {
    await rm(directory, { recursive: true, force: true });
    throw error;
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [version, directory] = process.argv.slice(2);
  const pkg = JSON.parse(await readFile(join(packageRoot, 'package.json'), 'utf8'));
  if (!directory || version !== pkg.version) throw new Error('Require the exact CLI release version and a new output directory');
  await stageReleaseImageBinary({ version, directory, sourceCommit: process.env.GITHUB_SHA });
}
