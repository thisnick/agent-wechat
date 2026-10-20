#!/usr/bin/env node
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assetName, inspectBinary, releaseRepository, supportedTarget, validateManifest } from '../packages/cli/scripts/native-runtime.mjs';

export async function generateManifest(binary, version, sourceCommit, sourceTreeDirty = false) {
  const bytes = await readFile(binary);
  const manifest = { schemaVersion: 1, version, repository: releaseRepository, sourceCommit, sourceTreeDirty,
    assets: [{ name: assetName(version), ...supportedTarget, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }] };
  validateManifest(manifest, version);
  await inspectBinary(resolve(binary), version);
  return manifest;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [binary, version, sourceCommit, output, dirty] = process.argv.slice(2);
  if (!binary || !version || !sourceCommit || !output) throw new Error('Usage: native-release-manifest.mjs BINARY VERSION SOURCE_COMMIT OUTPUT');
  await writeFile(output, `${JSON.stringify(await generateManifest(binary, version, sourceCommit, dirty === 'true'), null, 2)}\n`, { flag: 'wx' });
}
