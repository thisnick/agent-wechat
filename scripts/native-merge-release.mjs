#!/usr/bin/env node
// Merge independently tested architecture artifacts without executing foreign code.
import { copyFile, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { manifestName, targetKey, supportedTargets, validateManifest, verifyBytes } from '../packages/cli/scripts/native-runtime.mjs';

export async function mergeNativeRelease(output, directories) {
  if (directories.length !== supportedTargets.length) throw new Error('Require one artifact directory per supported target');
  let merged;
  const files = [];
  for (const directory of directories) {
    const names = (await readdir(directory)).filter(name => /^wx-v.+-checksums\.json$/.test(name));
    if (names.length !== 1) throw new Error('Require exactly one manifest per architecture');
    const manifest = JSON.parse(await readFile(join(directory, names[0]), 'utf8'));
    if (manifest.sourceTreeDirty === true) throw new Error('Dirty source artifacts cannot be merged for publication');
    if (manifest.assets?.length !== 1) throw new Error('Require one independently tested architecture per fragment');
    const target = supportedTargets.find(item => targetKey(item) === targetKey(manifest.assets[0]));
    if (!target) throw new Error('Unsupported architecture');
    const asset = validateManifest(manifest, manifest.version, { target });
    if (!merged) merged = { ...manifest, assets: [] };
    if (manifest.sourceCommit !== merged.sourceCommit || manifest.version !== merged.version || manifest.sourceTreeDirty !== merged.sourceTreeDirty) {
      throw new Error('Architecture fragments must have the same version and source');
    }
    const bytes = await readFile(join(directory, asset.name));
    verifyBytes(bytes, asset);
    const notices = await readFile(join(directory, `wx-v${manifest.version}-THIRD-PARTY-NOTICES.txt`));
    if (files.length && !notices.equals(files[0].notices)) throw new Error('Architecture notices differ');
    files.push({ source: join(directory, asset.name), name: asset.name, notices });
    merged.assets.push(asset);
  }
  merged.assets.sort((a, b) => targetKey(a).localeCompare(targetKey(b)));
  validateManifest(merged, merged.version, { requireAllTargets: true });
  await mkdir(output, { recursive: true });
  if ((await readdir(output)).length) throw new Error('Merged artifact directory must be empty');
  for (const file of files) await copyFile(file.source, join(output, file.name));
  await writeFile(join(output, manifestName(merged.version)), `${JSON.stringify(merged, null, 2)}\n`, { flag: 'wx' });
  await writeFile(join(output, `wx-v${merged.version}-THIRD-PARTY-NOTICES.txt`), files[0].notices, { flag: 'wx' });
  return merged;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [output, ...directories] = process.argv.slice(2).map(value => resolve(value));
  if (!output) throw new Error('Usage: native-merge-release.mjs OUTPUT TARGET_DIRECTORY...');
  await mergeNativeRelease(output, directories);
}
