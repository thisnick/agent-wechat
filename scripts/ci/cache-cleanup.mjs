#!/usr/bin/env node
// Deletes GitHub Actions cache entries that no workflow will read again, so
// they stop pushing the caches main fills out of the repository's 10 GB:
// - a pull request's entries, once it is closed (only that pull request
//   could read them);
// - entries saved by a branch that no longer exists;
// - all but the newest copy of each Ubuntu package cache. The apt-install
//   action restores only the newest entry for its prefix.
// Rust, Docker and pnpm caches on main are left to GitHub's own expiry.
import { appendFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// The key .github/actions/apt-install saves: prefix, then run ID, attempt
// and a UUID. Keep in step with that action.
const APT_KEY = /^(apt-v1-[a-z0-9]+-[a-z0-9]+-[0-9a-f]{16}-)\d+-\d+-[0-9a-f-]{36}$/;

// Returns { cache, reason } for each entry to delete. `pullOpen(number)` and
// `branchExists(name)` answer from GitHub; each is asked once per ref.
export async function unusedCaches(caches, { pullOpen, branchExists, defaultBranch = 'main' }) {
  caches = [...new Map(caches.map(cache => [cache.id, cache])).values()];
  const unused = [];
  const answers = new Map();
  const ask = (key, question) => {
    if (!answers.has(key)) answers.set(key, question());
    return answers.get(key);
  };
  const kept = [];
  for (const cache of caches) {
    const pull = cache.ref.match(/^refs\/pull\/(\d+)\/(?:merge|head)$/);
    const branch = cache.ref.match(/^refs\/heads\/(.+)$/)?.[1];
    if (pull && !(await ask(cache.ref, () => pullOpen(Number(pull[1]))))) {
      unused.push({ cache, reason: `pull request #${pull[1]} is closed` });
    } else if (branch && branch !== defaultBranch && !(await ask(cache.ref, () => branchExists(branch)))) {
      unused.push({ cache, reason: `branch ${branch} no longer exists` });
    } else kept.push(cache);
  }
  const newest = new Map();
  for (const cache of kept) {
    const prefix = cache.key.match(APT_KEY)?.[1];
    if (!prefix) continue;
    const group = `${cache.ref}\n${prefix}`;
    const current = newest.get(group);
    // Ties go to the higher ID, so exactly one copy is the newest.
    const newer = !current || Date.parse(cache.created_at) > Date.parse(current.created_at) ||
      (Date.parse(cache.created_at) === Date.parse(current.created_at) && cache.id > current.id);
    if (newer) {
      if (current) unused.push({ cache: current, reason: 'a newer copy of this package cache exists' });
      newest.set(group, cache);
    } else unused.push({ cache, reason: 'a newer copy of this package cache exists' });
  }
  return unused;
}

// GitHub also answers 404 for what a token can't see, so a 404 means
// "missing" only where the caller says so, and branch lookups are trusted
// only after the default branch is found.
export function githubCaches({ api, repository, token, defaultBranch = 'main', fetchImpl = fetch }) {
  const call = async (method, path, { missing } = {}) => {
    const response = await fetchImpl(`${api}/repos/${repository}${path}`, {
      method,
      headers: { accept: 'application/vnd.github+json', authorization: `Bearer ${token}`, 'x-github-api-version': '2022-11-28' },
    });
    if (response.status === 404 && missing !== undefined) return missing;
    if (!response.ok) throw new Error(`${method} ${path}: GitHub API returned HTTP ${response.status}`);
    return response.status === 204 ? true : response.json();
  };
  const branchPath = name => `/git/ref/heads/${name.split('/').map(encodeURIComponent).join('/')}`;
  let branchesVisible;
  return {
    // Newest first, so entries created while paging push the rest down (and
    // are dropped as repeats) rather than being skipped.
    list: async ref => {
      const caches = new Map();
      for (let page = 1; ; page++) {
        const body = await call('GET', `/actions/caches?per_page=100&page=${page}&sort=created_at&direction=desc${ref ? `&ref=${encodeURIComponent(ref)}` : ''}`);
        for (const cache of body.actions_caches) caches.set(cache.id, cache);
        if (body.actions_caches.length < 100 || page * 100 >= body.total_count) return [...caches.values()];
      }
    },
    // Pull requests aren't deleted, so a 404 is a token that can't read them.
    pullOpen: async number => (await call('GET', `/pulls/${number}`)).state === 'open',
    branchExists: async name => {
      branchesVisible ??= call('GET', branchPath(defaultBranch), { missing: false }).then(found => {
        if (!found) throw new Error(`Cannot see branch ${defaultBranch}; the token needs contents: read`);
        return true;
      });
      await branchesVisible;
      return (await call('GET', branchPath(name), { missing: false })) !== false;
    },
    // An entry already gone counts as deleted.
    remove: async id => { await call('DELETE', `/actions/caches/${id}`, { missing: true }); },
  };
}

const megabytes = bytes => `${(bytes / 1e6).toFixed(0)} MB`;

// Deletes `unused` (or only lists it) and returns a Markdown report.
export async function removeCaches(unused, { remove, dryRun, log = console.log }) {
  let freed = 0;
  const failures = [];
  for (const { cache, reason } of unused) {
    if (!dryRun) {
      try {
        await remove(cache.id);
      } catch (error) {
        failures.push(`${cache.key}: ${error.message}`);
        continue;
      }
    }
    freed += cache.size_in_bytes;
    log(`${dryRun ? 'Would delete' : 'Deleted'} ${cache.key} (${cache.ref}, ${megabytes(cache.size_in_bytes)}): ${reason}`);
  }
  const report = `${dryRun ? 'Would free' : 'Freed'} ${megabytes(freed)} from ${unused.length - failures.length} unused cache entries.` +
    (failures.length ? `\n\nCould not delete:\n${failures.map(failure => `- ${failure}`).join('\n')}` : '');
  return { report, failures };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const args = process.argv.slice(2);
  const pull = args.includes('--pull') ? Number(args[args.indexOf('--pull') + 1]) : undefined;
  const dryRun = args.includes('--dry-run');
  const { GITHUB_API_URL = 'https://api.github.com', GITHUB_REPOSITORY, GH_TOKEN, GITHUB_STEP_SUMMARY } = process.env;
  if (!GITHUB_REPOSITORY || !GH_TOKEN || (pull !== undefined && !(Number.isInteger(pull) && pull > 0))) {
    throw new Error('Usage: GH_TOKEN=... GITHUB_REPOSITORY=owner/repo scripts/ci/cache-cleanup.mjs [--pull NUMBER] [--dry-run]');
  }
  const github = githubCaches({ api: GITHUB_API_URL, repository: GITHUB_REPOSITORY, token: GH_TOKEN });
  // A closed pull request's own entries go at once; a sweep checks every ref.
  const unused = pull === undefined
    ? await unusedCaches(await github.list(), github)
    : (await Promise.all([`refs/pull/${pull}/merge`, `refs/pull/${pull}/head`].map(ref => github.list(ref))))
      .flat().map(cache => ({ cache, reason: `pull request #${pull} closed` }));
  const { report, failures } = await removeCaches(unused, { remove: github.remove, dryRun });
  console.log(report);
  if (GITHUB_STEP_SUMMARY) await appendFile(GITHUB_STEP_SUMMARY, `${report}\n`);
  if (failures.length) process.exitCode = 1;
}
