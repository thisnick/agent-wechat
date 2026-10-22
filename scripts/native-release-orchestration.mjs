#!/usr/bin/env node
// Keep manual native publication separate from npm's original push/OIDC context.
import { appendFile, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { assetName, manifestName, releaseRepository, supportedTargets } from '../packages/cli/scripts/native-runtime.mjs';
import { nativePackageUnpublished } from './native-prepare-package.mjs';

function githubRequest(path, { token, fetchImpl = fetch, method = 'GET' } = {}) {
  const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
  if (token) headers.Authorization = `Bearer ${token}`;
  return fetchImpl(`https://api.github.com/repos/${releaseRepository}/${path}`, {
    headers, method, redirect: 'error', signal: AbortSignal.timeout(10000),
  });
}

export async function nativeReleaseReadiness({ root = resolve('packages/cli'), token = process.env.GH_TOKEN, fetchImpl = fetch } = {}) {
  const { version } = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  // Allow extension-only recovery after the immutable CLI version is published.
  if (!await nativePackageUnpublished({ root, fetchImpl })) return { version, ready: true, state: 'published' };
  const response = await githubRequest(`releases/tags/v${version}`, { token, fetchImpl });
  if (response.status === 404) return { version, ready: false, state: 'missing' };
  if (response.status !== 200) throw new Error(`Cannot check native release availability (HTTP ${response.status})`);
  const release = await response.json();
  if (release.tag_name !== `v${version}` || release.draft !== false || !Array.isArray(release.assets)) {
    throw new Error('Invalid native release availability response');
  }
  const uploaded = new Set(release.assets.filter(asset => asset.state === 'uploaded').map(asset => asset.name));
  const required = [manifestName(version), `wx-v${version}-THIRD-PARTY-NOTICES.txt`, ...supportedTargets.map(target => assetName(version, target))];
  const ready = required.every(name => uploaded.has(name));
  // Availability never substitutes for exact source, binary and checksum checks.
  return { version, ready, state: ready ? 'available' : 'uploading' };
}

export async function resumeNpmRelease({ sourceCommit, token = process.env.GH_TOKEN, fetchImpl = fetch } = {}) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full native release source commit is required');
  const query = new URLSearchParams({ head_sha: sourceCommit, event: 'push', branch: 'main', per_page: '100' });
  const response = await githubRequest(`actions/workflows/release.yml/runs?${query}`, { token, fetchImpl });
  if (response.status !== 200) throw new Error(`Cannot find the original npm release run (HTTP ${response.status})`);
  const result = await response.json();
  if (!Array.isArray(result.workflow_runs) || result.total_count > 100) throw new Error('Cannot establish a unique original npm release run');
  const runs = result.workflow_runs.filter(run => run.head_sha === sourceCommit && run.event === 'push'
    && run.head_branch === 'main' && run.path === '.github/workflows/release.yml'
    && run.repository?.full_name === releaseRepository && run.head_repository?.full_name === releaseRepository);
  if (runs.length !== 1 || !Number.isSafeInteger(runs[0].id) || runs[0].id < 1) {
    throw new Error('Require exactly one original main-branch npm release run for the native source commit');
  }
  const run = runs[0];
  if (run.status !== 'completed') throw new Error('Original npm release run has not completed; retry after it finishes');
  // A waiting run is successful, so rerun the whole job, not just failed jobs.
  // Keep its original GITHUB_SHA and OIDC provenance instead of publishing from
  // workflow_run, whose GITHUB_SHA may refer to a newer default-branch commit.
  const retry = await githubRequest(`actions/runs/${run.id}/rerun`, { token, fetchImpl, method: 'POST' });
  if (retry.status !== 201) throw new Error(`Cannot resume the original npm release run (HTTP ${retry.status})`);
  return { runId: run.id, sourceCommit };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  if (process.argv[2] === 'ready') {
    const result = await nativeReleaseReadiness();
    if (!process.env.GITHUB_OUTPUT) throw new Error('GITHUB_OUTPUT is required');
    await appendFile(process.env.GITHUB_OUTPUT, `ready=${result.ready}\n`);
    if (!result.ready) {
      const sourceCommit = process.env.GITHUB_SHA;
      if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full npm release source commit is required');
      const summary = `## Waiting for native release assets\n\n`
        + `npm ${result.version} is waiting for the approved native release at commit \`${sourceCommit}\`.\n\n`
        + `Run **Prepare native release assets** on main with version \`${result.version}\` and \`confirm_publish=true\` while main still points to this commit. `
        + `After successful native publication, Release automatically retries this original push run and verifies all source and checksum bindings before publishing npm.\n`;
      if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
      console.error(summary);
    }
    console.log(JSON.stringify(result));
  } else if (process.argv[2] === 'resume') {
    const result = await resumeNpmRelease({ sourceCommit: process.env.NATIVE_SOURCE_COMMIT });
    const summary = `Resumed [the original npm release run](https://github.com/${releaseRepository}/actions/runs/${result.runId}) at commit \`${result.sourceCommit}\`.\n`;
    if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
    console.log(summary);
  } else {
    throw new Error('Usage: native-release-orchestration.mjs ready|resume');
  }
}
