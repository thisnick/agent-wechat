#!/usr/bin/env node
// Release checks and exact-source tag reservation. Asset publication is separate.
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { nativePackageUnpublished } from './native-prepare-package.mjs';
import { releaseRepository } from '../packages/cli/scripts/native-runtime.mjs';

export async function validateNativeRelease({ root = resolve('packages/cli'), version = process.env.RELEASE_VERSION,
  sourceCommit = process.env.GITHUB_SHA, token = process.env.GH_TOKEN, tagMode = 'check', fetchImpl = fetch } = {}) {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  if (pkg.version !== version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version ?? '')) throw new Error('Version differs from committed CLI package');
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full native release source commit is required');
  if (!await nativePackageUnpublished({ root, fetchImpl })) throw new Error('Native assets require an unpublished CLI version; existing npm versions cannot be repurposed');
  const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
  if (token) headers.Authorization = `Bearer ${token}`;
  const request = (path, options) => fetchImpl(`https://api.github.com/repos/${releaseRepository}/${path}`, {
    headers, redirect: 'error', signal: AbortSignal.timeout(10000), ...options,
  });
  const tag = `v${version}`;
  let response = await request(`git/ref/tags/${tag}`);
  if (response.status === 404) {
    if (tagMode === 'require') throw new Error(`Missing release tag ${tag} for ${sourceCommit}`);
    if (tagMode !== 'reserve') return { version, sourceCommit, tagExists: false };
    response = await request('git/refs', {
      method: 'POST',
      headers: { ...headers, 'Content-Type': 'application/json' },
      body: JSON.stringify({ ref: `refs/tags/${tag}`, sha: sourceCommit }),
    });
    if (response.status !== 201) {
      throw new Error(`Cannot reserve ${tag} at ${sourceCommit} (HTTP ${response.status}). See release tag recovery in RELEASING.md.`);
    }
  } else if (response.status !== 200) {
    throw new Error(`Cannot verify native release tag (HTTP ${response.status})`);
  }
  let object = (await response.json()).object;
  for (let depth = 0; depth < 5; depth++) {
    if (!/^[a-f0-9]{40}$/.test(object?.sha ?? '')) throw new Error('Invalid native release tag object');
    if (object.type === 'commit') {
      if (object.sha !== sourceCommit) throw new Error('Existing version tag targets a different source commit');
      return { version, sourceCommit, tagExists: true };
    }
    if (object.type !== 'tag') throw new Error('Native version tag must resolve to a commit');
    const annotated = await request(`git/tags/${object.sha}`);
    if (annotated.status !== 200) throw new Error(`Cannot verify annotated native release tag (HTTP ${annotated.status})`);
    object = (await annotated.json()).object;
  }
  throw new Error('Native release tag nesting exceeds its bound');
}
if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const flag = process.argv[2];
  const tagMode = flag === '--reserve-tag' ? 'reserve' : flag === '--require-tag' ? 'require' : 'check';
  const result = await validateNativeRelease({ tagMode });
  console.error(`Validated unpublished native CLI ${result.version} at ${result.sourceCommit}; version tag ${result.tagExists ? 'matches' : 'does not exist'}.`);
}
