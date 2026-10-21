#!/usr/bin/env node
// prepack checks bundled metadata; prepublishOnly additionally verifies the
// complete, already published executable matrix before npm can be published.
import { fetchBytes, manifestName, readManifest, releaseUrl, validateManifest, verifyBytes } from './native-runtime.mjs';
const { manifest, asset } = await readManifest();
if (process.env.GITHUB_SHA && manifest.sourceCommit !== process.env.GITHUB_SHA) throw new Error('Native manifest source commit differs from publication commit');
if (process.argv.includes('--publication')) {
  if (manifest.sourceTreeDirty === true) throw new Error('Dirty local validation artifacts cannot be published');
  // Manual npm publication must enforce the same asset-before-package ordering
  // as the normal release workflow; local pack/offline smoke does not fetch.
  const remote = JSON.parse(await fetchBytes(releaseUrl(manifest.version, manifestName(manifest.version)), 65536));
  validateManifest(manifest, manifest.version, { requireAllTargets: true });
  validateManifest(remote, manifest.version, { requireAllTargets: true });
  if (remote.sourceCommit !== manifest.sourceCommit || remote.sourceTreeDirty === true) throw new Error('Prepared release differs from this npm package');
  for (const item of manifest.assets) {
    const published = remote.assets.find(candidate => candidate.arch === item.arch);
    if (published.sha256 !== item.sha256 || published.bytes !== item.bytes) throw new Error('Prepared release differs from this npm package');
    verifyBytes(await fetchBytes(releaseUrl(manifest.version, item.name), item.bytes), item);
  }
}
console.error(`Verified bundled native release manifest for ${manifest.version}.`);
