#!/usr/bin/env node
// Validate stable/prerelease channels before publication; never publish here.
import { appendFile, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

export const publicPackages = [['packages/cli', '@agent-wechat/cli'], ['packages/openclaw-extension', '@agent-wechat/agent-wechat']];
const semver = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

export function releaseChannel(version, preState) {
  const match = typeof version === 'string' && version.match(semver);
  if (!match || match[0] !== version || match[4]?.split('.').some(id => /^\d+$/.test(id) && id.length > 1 && id.startsWith('0'))) throw new Error('Invalid release version');
  if (!match[4]) {
    if (preState?.mode === 'pre') throw new Error('Stable package version in prerelease mode; merge the generated version PR first');
    return { version, distTag: 'latest', prerelease: false };
  }
  if (preState?.mode !== 'pre' || !/^[a-z][a-z0-9-]*$/.test(preState.tag ?? '')
    || preState.tag === 'latest' || !match[4].startsWith(`${preState.tag}.`)) {
    throw new Error('Prerelease version requires matching Changesets prerelease mode; refusing a latest-tag publication');
  }
  return { version, distTag: preState.tag, prerelease: true };
}

// Alignment checks are also used by PR packaging while a channel switch is staged.
export async function readReleaseVersion({ root = process.cwd(), version } = {}) {
  const cli = JSON.parse(await readFile(join(root, publicPackages[0][0], 'package.json'), 'utf8'));
  version ??= cli.version;
  for (const [directory, name] of publicPackages) {
    const pkg = JSON.parse(await readFile(join(root, directory, 'package.json'), 'utf8'));
    if (pkg.name !== name || pkg.private === true || pkg.version !== version) throw new Error('Release channel package/version mismatch');
  }
  return version;
}

export async function readReleasePolicy({ root = process.cwd(), version } = {}) {
  version = await readReleaseVersion({ root, version });
  let preState;
  try { preState = JSON.parse(await readFile(join(root, '.changeset/pre.json'), 'utf8')); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  return releaseChannel(version, preState);
}

export async function requireStablePublicPackages(policy, fetchImpl = fetch) {
  if (!policy.prerelease) return;
  // Changesets v2 gives never-stable packages special latest-tag behavior. These
  // existing public packages must retain a stable line before beta publication.
  await Promise.all(publicPackages.map(async ([, name]) => {
    const response = await fetchImpl(`https://registry.npmjs.org/${name.replace('/', '%2f')}/latest`,
      { redirect: 'error', signal: AbortSignal.timeout(10000) });
    if (response.status !== 200) throw new Error(`Cannot establish stable npm latest for ${name} (HTTP ${response.status})`);
    const stable = await response.json();
    if (stable.name !== name || typeof stable.version !== 'string' || !semver.test(stable.version) || stable.version.includes('-')) {
      throw new Error(`Prerelease publication requires an existing stable npm latest for ${name}`);
    }
  }));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const { values } = parseArgs({ options: { 'check-registry': { type: 'boolean' }, 'github-output': { type: 'boolean' } } });
  const policy = await readReleasePolicy({ version: process.env.RELEASE_VERSION });
  if (values['check-registry']) await requireStablePublicPackages(policy);
  if (values['github-output']) {
    if (!process.env.GITHUB_OUTPUT) throw new Error('GITHUB_OUTPUT is required');
    await appendFile(process.env.GITHUB_OUTPUT, `version=${policy.version}\ndist_tag=${policy.distTag}\nprerelease=${policy.prerelease}\n`);
  }
  console.log(JSON.stringify(policy));
}
