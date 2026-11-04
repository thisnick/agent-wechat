// Check for an existing native release before starting a new build.
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assetName, manifestName, releaseRepository, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';
import { nativePackageUnpublished } from './native-prepare-package.mjs';

function githubRequest(path, { token, fetchImpl = fetch } = {}) {
  const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
  if (token) headers.Authorization = `Bearer ${token}`;
  return fetchImpl(`https://api.github.com/repos/${releaseRepository}/${path}`, {
    headers, redirect: 'error', signal: AbortSignal.timeout(10000),
  });
}

export async function nativeReleaseReadiness({ root = resolve('packages/cli'), token = process.env.GH_TOKEN, fetchImpl = fetch } = {}) {
  const { version } = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  if (!await nativePackageUnpublished({ root, fetchImpl })) return { version, state: 'published' };
  const response = await githubRequest(`releases/tags/v${version}`, { token, fetchImpl });
  if (response.status === 404) return { version, state: 'missing' };
  if (response.status !== 200) throw new Error(`Cannot check native release availability (HTTP ${response.status})`);
  const release = await response.json();
  if (release.tag_name !== `v${version}` || release.draft !== false || !Array.isArray(release.assets)) {
    throw new Error('Invalid native release availability response');
  }
  const uploaded = new Set(release.assets.filter(asset => asset.state === 'uploaded').map(asset => asset.name));
  const required = [manifestName(version), `wx-v${version}-THIRD-PARTY-NOTICES.txt`, ...supportedTargets.map(target => assetName(version, target))];
  const complete = required.every(name => uploaded.has(name));
  // Availability never substitutes for exact source, binary and checksum checks.
  return { version, state: complete ? 'available' : 'uploading' };
}
