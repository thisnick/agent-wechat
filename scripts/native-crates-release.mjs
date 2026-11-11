#!/usr/bin/env node
// Library releases share the application version and keep the existing trusted-publishing workflow identity.
import { execFileSync } from 'node:child_process';
import { appendFile, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cratePackage, crateReleaseTag, readCratePolicy, readCrateVersion } from './native-crates-version.mjs';
import { isVersionPackageMerge, previousPackageVersion } from './native-release-trigger.mjs';
import { isReleaseContext, repository, verifyReleaseSource } from './native-crates.mjs';
import { changelogEntry } from './native-release-notes.mjs';

export async function crateReleaseTrigger({ eventName, event, sourceCommit, previousVersion, root, token, fetchImpl = fetch }) {
  if (eventName === 'workflow_dispatch') {
    if (event.ref !== 'refs/heads/main' || ![true, 'true'].includes(event.inputs?.confirm_publish) || !event.inputs?.crate_version) return { publish: false };
    return { publish: true, version: await readCrateVersion({ root, version: event.inputs.crate_version }) };
  }
  if (eventName !== 'push' || event.ref !== 'refs/heads/main') return { publish: false };
  if (!await isVersionPackageMerge({ sourceCommit, token, fetchImpl })) return { publish: false };
  previousVersion = typeof previousVersion === 'function' ? await previousVersion() : previousVersion;
  if (previousVersion === undefined) throw new Error('Previous Rust library version is required');
  const version = await readCrateVersion({ root });
  return { publish: version !== previousVersion, version };
}

async function github(path, { token, fetchImpl = fetch, method = 'GET', body } = {}) {
  const response = await fetchImpl(`https://api.github.com/repos/${repository}/${path}`, {
    method, headers: { Accept: 'application/vnd.github+json', Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json', 'X-GitHub-Api-Version': '2022-11-28' },
    ...(body ? { body: JSON.stringify(body) } : {}), redirect: 'error', signal: AbortSignal.timeout(30000),
  });
  if (![200, 201, ...(method === 'GET' ? [404] : [])].includes(response.status)) {
    throw new Error(`Cannot ${method} Rust release metadata (HTTP ${response.status})`);
  }
  return response;
}

export async function crateReleaseNotes({ root = process.cwd(), version, sourceCommit }) {
  const entry = changelogEntry(await readFile(join(root, cratePackage.directory, 'CHANGELOG.md'), 'utf8'), version);
  // A release with only application changes leaves the fixed-group library entry empty.
  const changes = entry || 'No library changes. The version follows the application release.';
  return `# Rust libraries ${version}\n\n${changes}\n\nSource: ${sourceCommit}\n\nAll five Rust libraries use this version, which matches the application release.\n`;
}

export async function prepareCrateRelease({ root, version, sourceCommit, token, fetchImpl = fetch }) {
  const policy = await readCratePolicy({ root, version });
  const notes = await crateReleaseNotes({ root, version, sourceCommit });
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full release source commit is required');
  const tag = crateReleaseTag(version);
  const response = await github(`git/ref/tags/${tag}`, { token, fetchImpl });
  if (response.status === 404) await github('git/refs', { token, fetchImpl, method: 'POST',
    body: { ref: `refs/tags/${tag}`, sha: sourceCommit } });
  await verifyReleaseSource({ ...policy, sourceCommit, token, fetchImpl });
  const releaseResponse = await github(`releases/tags/${tag}`, { token, fetchImpl });
  if (releaseResponse.status !== 404) {
    const existing = await releaseResponse.json();
    if (existing.tag_name !== tag || existing.draft !== false || existing.prerelease !== policy.prerelease) {
      throw new Error('Existing Rust release channel mismatch');
    }
  }
  return { ...policy, tag, notes, exists: releaseResponse.status !== 404 };
}

export async function finishCrateRelease(options) {
  const release = await prepareCrateRelease(options);
  if (release.exists) return;
  await github('releases', { ...options, method: 'POST', body: { tag_name: release.tag,
    target_commitish: options.sourceCommit, name: `Rust libraries ${release.version}`,
    body: release.notes, draft: false, prerelease: release.prerelease, make_latest: 'false' } });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const command = process.argv[2];
  const sourceCommit = process.env.GITHUB_SHA;
  if (command === 'trigger') {
    const event = JSON.parse(await readFile(process.env.GITHUB_EVENT_PATH, 'utf8'));
    const result = await crateReleaseTrigger({ eventName: process.env.GITHUB_EVENT_NAME,
      event: { ...event, ref: process.env.GITHUB_REF }, sourceCommit, previousVersion: () => previousPackageVersion(event, cratePackage.directory) });
    await appendFile(process.env.GITHUB_OUTPUT, `crate_publish=${result.publish}\ncrate_version=${result.version ?? ''}\n`);
    console.log(JSON.stringify(result));
  } else if (['prepare', 'finish'].includes(command)) {
    if (!isReleaseContext(process.env)) throw new Error('Rust releases require the main-branch native release workflow');
    if (execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim() !== sourceCommit
      || execFileSync('git', ['status', '--porcelain'], { encoding: 'utf8' }).trim()) throw new Error('Crate source must be clean and match the release commit');
    const options = { version: process.env.RELEASE_VERSION, sourceCommit, token: process.env.GH_TOKEN };
    if (!options.version) throw new Error('An explicit RELEASE_VERSION is required');
    await (command === 'prepare' ? prepareCrateRelease : finishCrateRelease)(options);
  } else throw new Error('Usage: native-crates-release.mjs trigger|prepare|finish');
}
