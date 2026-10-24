#!/usr/bin/env node
import { appendFile, readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { releaseRepository } from '../packages/cli/scripts/native-runtime.mjs';
import { nativeReleaseReadiness } from './native-release-orchestration.mjs';

export async function isVersionPackageMerge({ sourceCommit, token = process.env.GH_TOKEN, fetchImpl = fetch }) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full source commit is required');
  const response = await fetchImpl(`https://api.github.com/repos/${releaseRepository}/commits/${sourceCommit}/pulls?per_page=100`, {
    headers: { Accept: 'application/vnd.github+json', Authorization: `Bearer ${token}`, 'X-GitHub-Api-Version': '2022-11-28' },
    redirect: 'error', signal: AbortSignal.timeout(10000),
  });
  if (response.status !== 200) throw new Error(`Cannot identify the version PR (HTTP ${response.status})`);
  const pulls = await response.json();
  if (!Array.isArray(pulls) || pulls.length >= 100) throw new Error('Cannot establish the version PR');
  return pulls.some(pr => pr.merged_at && pr.merge_commit_sha === sourceCommit
    && pr.base?.ref === 'main' && pr.base?.repo?.full_name === releaseRepository
    && pr.head?.ref === 'changeset-release/main' && pr.head?.repo?.full_name === releaseRepository);
}

export async function nativeReleaseTrigger({ eventName, event, sourceCommit, root, token, fetchImpl = fetch }) {
  if (eventName === 'workflow_dispatch') {
    if (event.ref !== 'refs/heads/main' || ![true, 'true'].includes(event.inputs?.confirm_publish)) return { publish: false };
    return { publish: true, version: event.inputs.version };
  }
  if (eventName !== 'push' || event.ref !== 'refs/heads/main') return { publish: false };
  if (!await isVersionPackageMerge({ sourceCommit, token, fetchImpl })) return { publish: false };
  const state = await nativeReleaseReadiness({ root, token, fetchImpl });
  return { publish: state.state === 'missing', version: state.version };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const event = JSON.parse(await readFile(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const result = await nativeReleaseTrigger({ eventName: process.env.GITHUB_EVENT_NAME, event: { ...event, ref: process.env.GITHUB_REF }, sourceCommit: process.env.GITHUB_SHA });
  if (result.publish && !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(result.version ?? '')) throw new Error('Invalid release version');
  await appendFile(process.env.GITHUB_OUTPUT, `publish=${result.publish}\nversion=${result.version ?? ''}\n`);
  console.log(JSON.stringify(result));
}
