#!/usr/bin/env node
// Lets a desktop build compile while its platform's CLI is still building in
// the same run: waits until that job uploads the CLI artifact. A failed or
// cancelled CLI job uploads NAME-failed-ATTEMPT instead, so the wait ends at
// once rather than at the timeout. Never downloads or publishes anything.
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const failureName = (name, attempt) => `${name}-failed-${attempt}`;

// Returns once `name` is uploaded; throws if this attempt's failure marker
// appears first or the deadline passes. `list` returns the run's artifacts.
export async function waitForArtifact({ name, attempt, list, timeoutMs, intervalMs = 30_000,
  sleep = ms => new Promise(done => setTimeout(done, ms)), now = Date.now, log = console.log }) {
  const failed = failureName(name, attempt);
  const deadline = now() + timeoutMs;
  for (let polls = 0; ; polls++) {
    let names;
    try {
      names = new Set((await list()).filter(artifact => !artifact.expired).map(artifact => artifact.name));
    } catch (error) {
      log(`Cannot list this run's artifacts yet: ${error.message}`);
    }
    if (names?.has(failed)) throw new Error(`The job that builds ${name} failed; see that job's log`);
    if (names?.has(name)) {
      log(`${name} is available`);
      return;
    }
    if (now() >= deadline) throw new Error(`${name} was not uploaded within ${Math.round(timeoutMs / 60_000)} minutes`);
    if (polls % 10 === 0) log(`Waiting for ${name}`);
    await sleep(Math.min(intervalMs, Math.max(0, deadline - now())));
  }
}

// One request per poll lists every artifact of the run, so several waiting
// jobs stay well inside the GITHUB_TOKEN rate limit.
export function runArtifacts({ api, repository, runId, token, fetchImpl = fetch }) {
  return async () => {
    const artifacts = [];
    for (let page = 1; ; page++) {
      const response = await fetchImpl(`${api}/repos/${repository}/actions/runs/${runId}/artifacts?per_page=100&page=${page}`, {
        headers: { accept: 'application/vnd.github+json', authorization: `Bearer ${token}`, 'x-github-api-version': '2022-11-28' },
      });
      if (!response.ok) throw new Error(`GitHub API returned HTTP ${response.status}`);
      const body = await response.json();
      artifacts.push(...body.artifacts);
      if (body.artifacts.length < 100 || artifacts.length >= body.total_count) return artifacts;
    }
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [name, ...args] = process.argv.slice(2);
  const option = (flag, fallback) => (args.includes(flag) ? Number(args[args.indexOf(flag) + 1]) : fallback);
  const { GITHUB_API_URL = 'https://api.github.com', GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT, GH_TOKEN } = process.env;
  if (!name || !GITHUB_REPOSITORY || !GITHUB_RUN_ID || !GITHUB_RUN_ATTEMPT || !GH_TOKEN) {
    throw new Error('Usage: GH_TOKEN=... scripts/ci/wait-artifact.mjs NAME [--timeout-minutes N] (inside a GitHub Actions job)');
  }
  await waitForArtifact({
    name,
    attempt: GITHUB_RUN_ATTEMPT,
    timeoutMs: option('--timeout-minutes', 100) * 60_000,
    list: runArtifacts({ api: GITHUB_API_URL, repository: GITHUB_REPOSITORY, runId: GITHUB_RUN_ID, token: GH_TOKEN }),
  });
}
