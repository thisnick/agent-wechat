#!/usr/bin/env node
import { fetchBytes, manifestName, readManifest, releaseUrl, validateManifest, verifyBytes } from './native-runtime.mjs';
const { manifest, asset } = await readManifest();
if (process.env.GITHUB_SHA && manifest.sourceCommit !== process.env.GITHUB_SHA) throw new Error('Native manifest source commit differs from publication commit');
if (process.argv.includes('--publication')) {
  if (manifest.sourceTreeDirty === true) throw new Error('Dirty local validation artifacts cannot be published');
  // Manual npm publication must enforce the same asset-before-package ordering
  // as the normal release workflow; local pack/offline smoke does not fetch.
  const remote = JSON.parse(await fetchBytes(releaseUrl(manifest.version, manifestName(manifest.version)), 65536));
  const remoteAsset = validateManifest(remote, manifest.version);
  if (remote.sourceCommit !== manifest.sourceCommit || remoteAsset.sha256 !== asset.sha256 || remoteAsset.bytes !== asset.bytes || remote.sourceTreeDirty === true) throw new Error('Prepared release differs from this npm package');
  verifyBytes(await fetchBytes(releaseUrl(manifest.version, asset.name), asset.bytes), asset);
}
console.error(`Verified bundled native release manifest for ${manifest.version}.`);
