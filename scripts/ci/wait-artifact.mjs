#!/usr/bin/env node
// Lets a desktop build compile while its platform's CLI is still building in
// the same run: waits for the job that builds that CLI and returns once it has
// succeeded and its artifact is listed. Ends at once if that job fails, is
// cancelled or is skipped. Never downloads or publishes anything.
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Matrix values GitHub appends to a job name: "portable / build (macos-latest,
// aarch64-apple-darwin)" gives ["macos-latest", "aarch64-apple-darwin"].
const matrixValues = name => {
  const open = name.lastIndexOf(' (');
  return open === -1 || !name.endsWith(')') ? [] : name.slice(open + 2, -1).split(',').map(value => value.trim());
};

// Jobs named "<prefix> (...)" whose matrix values include the target.
export const producers = (jobs, prefixes, target) => jobs.filter(job =>
  prefixes.some(prefix => job.name.startsWith(`${prefix} (`)) && matrixValues(job.name).includes(target));

// `listJobs` returns the current attempt's jobs; `hasArtifact` says whether
// the run lists `name`. A producer that isn't in this attempt ran in an
// earlier one that this re-run reuses, so its artifact is accepted.
export async function waitForArtifact({ name, target, prefixes, listJobs, hasArtifact, timeoutMs,
  intervalMs = 30_000, sleep = ms => new Promise(done => setTimeout(done, ms)), now = Date.now, log = console.log }) {
  const deadline = now() + timeoutMs;
  let missingAfterSuccess = 0;
  for (let polls = 0; ; polls++) {
    let jobs;
    try {
      jobs = producers(await listJobs(), prefixes, target);
      if (jobs.length === 0 || (!jobs.some(job => job.status !== 'completed') && jobs.some(job => job.conclusion === 'success'))) {
        if (await hasArtifact()) {
          log(`${name} is available`);
          return;
        }
      }
    } catch (error) {
      log(`Cannot read this run's jobs and artifacts yet: ${error.message}`);
      jobs = undefined;
    }
    if (jobs?.length && !jobs.some(job => job.status !== 'completed')) {
      if (!jobs.some(job => job.conclusion === 'success')) {
        throw new Error(`${jobs.map(job => `"${job.name}" ended with ${job.conclusion}`).join('; ')}: no ${name} to wait for`);
      }
      // The upload finishes before the job does; allow for a listing delay.
      if (++missingAfterSuccess >= 3) throw new Error(`"${jobs[0].name}" succeeded without uploading ${name}`);
    }
    if (now() >= deadline) throw new Error(`${name} was not uploaded within ${Math.round(timeoutMs / 60_000)} minutes`);
    if (polls % 10 === 0) {
      log(jobs?.length ? `Waiting for ${jobs.map(job => `"${job.name}"`).join(', ')}` : `Waiting for a job named ${prefixes.map(prefix => `"${prefix} (...${target}...)"`).join(' or ')}`);
    }
    await sleep(Math.min(intervalMs, Math.max(0, deadline - now())));
  }
}

const request = (fetchImpl, token) => async url => {
  const response = await fetchImpl(url, {
    headers: { accept: 'application/vnd.github+json', authorization: `Bearer ${token}`, 'x-github-api-version': '2022-11-28' },
  });
  if (!response.ok) throw new Error(`GitHub API returned HTTP ${response.status}`);
  return response.json();
};

// One request per poll while the CLI builds keeps several waiting jobs well
// inside the repository's shared GITHUB_TOKEN rate limit.
export function githubRun({ api, repository, runId, attempt, token, fetchImpl = fetch }) {
  const get = request(fetchImpl, token);
  const run = `${api}/repos/${repository}/actions/runs/${runId}`;
  return {
    listJobs: async () => {
      const jobs = [];
      for (let page = 1; ; page++) {
        const body = await get(`${run}/attempts/${attempt}/jobs?per_page=100&page=${page}`);
        jobs.push(...body.jobs);
        if (body.jobs.length < 100 || jobs.length >= body.total_count) return jobs;
      }
    },
    hasArtifact: async name => {
      const body = await get(`${run}/artifacts?name=${encodeURIComponent(name)}&per_page=100`);
      return body.artifacts.some(artifact => artifact.name === name && !artifact.expired);
    },
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const [name, ...args] = process.argv.slice(2);
  const values = flag => args.flatMap((arg, index) => (arg === flag ? [args[index + 1]] : []));
  const [target] = values('--target');
  const prefixes = values('--job').flatMap(value => (value ?? '').split(',')).map(value => value.trim()).filter(Boolean);
  const timeout = values('--timeout-minutes').length ? Number(values('--timeout-minutes')[0]) : 100;
  const { GITHUB_API_URL = 'https://api.github.com', GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_RUN_ATTEMPT, GH_TOKEN } = process.env;
  if (!name || !target || !prefixes.length || !(timeout > 0 && Number.isFinite(timeout)) ||
      !GITHUB_REPOSITORY || !GITHUB_RUN_ID || !GITHUB_RUN_ATTEMPT || !GH_TOKEN) {
    throw new Error('Usage: GH_TOKEN=... scripts/ci/wait-artifact.mjs NAME --target RUST_TARGET --job "JOB NAME"[,...] [--timeout-minutes N] (inside a GitHub Actions job)');
  }
  const run = githubRun({ api: GITHUB_API_URL, repository: GITHUB_REPOSITORY, runId: GITHUB_RUN_ID, attempt: GITHUB_RUN_ATTEMPT, token: GH_TOKEN });
  await waitForArtifact({
    name, target, prefixes, timeoutMs: timeout * 60_000,
    listJobs: run.listJobs,
    hasArtifact: () => run.hasArtifact(name),
  });
}
