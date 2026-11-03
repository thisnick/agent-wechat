#!/usr/bin/env node
// Check native publication before npm uses its original push/OIDC context.
import { appendFile, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
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

async function publicationJobsSucceeded({ runId, requiredJobs, token = process.env.GH_TOKEN, fetchImpl = fetch }) {
  if (!/^[1-9][0-9]*$/.test(String(runId ?? ''))) throw new Error('A native workflow run ID is required');
  const response = await githubRequest(`actions/runs/${runId}/jobs?per_page=100`, { token, fetchImpl });
  if (response.status !== 200) throw new Error(`Cannot check native publication (HTTP ${response.status})`);
  const result = await response.json();
  if (!Array.isArray(result.jobs) || result.total_count > 100) throw new Error('Cannot establish native publication');
  return requiredJobs.every(name => result.jobs.some(job => job.name === name && job.status === 'completed' && job.conclusion === 'success'));
}

export async function runtimeImagePublished({ sourceCommit, token = process.env.GH_TOKEN, fetchImpl = fetch }) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full image source commit is required');
  for (const [workflow, job] of [['native-release.yml', 'image / manifest'], ['native-image.yml', 'manifest']]) {
    const query = new URLSearchParams({ head_sha: sourceCommit, branch: 'main', per_page: '100' });
    const response = await githubRequest(`actions/workflows/${workflow}/runs?${query}`, { token, fetchImpl });
    if (response.status !== 200) throw new Error(`Cannot check runtime image publication (HTTP ${response.status})`);
    const result = await response.json();
    if (!Array.isArray(result.workflow_runs) || result.total_count > 100) throw new Error('Cannot establish runtime image publication');
    const runs = result.workflow_runs.filter(run => run.head_sha === sourceCommit && run.head_branch === 'main'
      && run.path === `.github/workflows/${workflow}` && ['push', 'workflow_dispatch'].includes(run.event)
      && run.repository?.full_name === releaseRepository && run.head_repository?.full_name === releaseRepository
      && run.status === 'completed' && run.conclusion === 'success');
    for (const run of runs) {
      if (await publicationJobsSucceeded({ runId: run.id, requiredJobs: [job], token, fetchImpl })) return true;
    }
  }
  return false;
}

export async function npmReleaseReadiness(options = {}) {
  const native = await nativeReleaseReadiness(options);
  if (!native.ready || native.state === 'published') return native;
  const ready = await runtimeImagePublished({ ...options, sourceCommit: options.sourceCommit ?? process.env.GITHUB_SHA });
  return { version: native.version, ready, state: ready ? 'available' : 'image-pending' };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  if (process.argv[2] === 'ready') {
    const result = await npmReleaseReadiness();
    if (!process.env.GITHUB_OUTPUT) throw new Error('GITHUB_OUTPUT is required');
    await appendFile(process.env.GITHUB_OUTPUT, `ready=${result.ready}\n`);
    if (!result.ready) {
      const sourceCommit = process.env.GITHUB_SHA;
      if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full npm release source commit is required');
      const summary = `## ${result.state === 'image-pending' ? 'Waiting for native runtime image' : 'Waiting for native release assets'}\n\n`
        + `npm ${result.version} is waiting for native assets and the runtime image at commit \`${sourceCommit}\`.\n\n`
        + `Merging the Changesets Version Packages PR starts **Prepare native release assets** automatically. For recovery, run it on main with version \`${result.version}\` and \`confirm_publish=true\` while main still points to this commit. `
        + `After successful native asset and runtime image publication, rerun this original push run to verify all source and checksum bindings before publishing npm.\n`;
      if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
      console.error(summary);
    }
    console.log(JSON.stringify(result));
  } else {
    throw new Error('Usage: native-release-orchestration.mjs ready');
  }
}
