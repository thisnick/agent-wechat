#!/usr/bin/env node
// Package native libraries without publishing; registry writes require the release command.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFile, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readReleasePolicy } from './native-release-policy.mjs';

export const crateNames = ['wechat-platform', 'wechat-media', 'wechat-protocol', 'wechat-profile'];
export const repository = 'thisnick/agent-wechat';
const workspace = 'packages/native-client-rust/Cargo.toml';
const selections = names => names.flatMap(name => ['-p', name]);

export function isReleaseContext(env) {
  return env.GITHUB_REPOSITORY === repository && env.GITHUB_REF === 'refs/heads/main'
    && ['push', 'workflow_dispatch'].includes(env.GITHUB_EVENT_NAME)
    && env.GITHUB_WORKFLOW_REF === `${repository}/.github/workflows/native-release.yml@refs/heads/main`;
}

export async function syncCrateVersions(root, version) {
  for (const name of crateNames) {
    const path = join(root, 'packages/native-client-rust/crates', name, 'Cargo.toml');
    const old = await readFile(path, 'utf8');
    const updated = old.replace(/^(wechat-[\w-]+) = \{ path = "\.\.\/[^"\n]+", version = "[^"]+" \}$/gm,
      (line, dependency) => {
        if (!crateNames.includes(dependency)) throw new Error(`Unpublished library dependency: ${dependency}`);
        return line.replace(/version = "[^"]+"/, `version = "=${version}"`);
      });
    if (old !== updated) await writeFile(path, updated);
  }
}

export function validateCrates(metadata, version) {
  const members = metadata.packages.filter(pkg => metadata.workspace_members.includes(pkg.id));
  const published = members.filter(pkg => pkg.publish === null || pkg.publish.length > 0);
  if (published.length !== crateNames.length || published.some(pkg => !crateNames.includes(pkg.name))) {
    throw new Error('Only the four selected native libraries may be published');
  }
  for (const name of crateNames) {
    const pkg = members.find(pkg => pkg.name === name);
    if (!pkg || pkg.version !== version || JSON.stringify(pkg.publish) !== '["crates-io"]') {
      throw new Error(`Crate version or registry mismatch: ${name}`);
    }
    if (pkg.rust_version !== '1.93.1' || !pkg.description || !pkg.readme || (!pkg.license && !pkg.license_file)
      || pkg.repository !== `https://github.com/${repository}`) throw new Error(`Missing crate metadata: ${name}`);
    for (const dep of pkg.dependencies) {
      if (dep.path && (!crateNames.includes(dep.name) || dep.req !== `=${version}`)) {
        throw new Error(`Library dependency must be published at the same exact version: ${name} -> ${dep.name}`);
      }
    }
  }
  return crateNames;
}

async function request(url, fetchImpl, token) {
  const response = await fetchImpl(url, {
    headers: { 'User-Agent': 'agent-wechat release (github.com/thisnick/agent-wechat)',
      ...(token ? { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json' } : {}) },
    redirect: 'error', signal: AbortSignal.timeout(30000),
  });
  if (![200, 404].includes(response.status)) throw new Error(`Cannot verify publication state (HTTP ${response.status})`);
  return response;
}

export async function verifyReleaseSource({ version, sourceCommit, prerelease, fetchImpl = fetch, token }) {
  if (!/^[a-f0-9]{40}$/.test(sourceCommit ?? '')) throw new Error('A full release source commit is required');
  const base = `https://api.github.com/repos/${repository}`;
  let response = await request(`${base}/git/ref/tags/v${version}`, fetchImpl, token);
  if (response.status !== 200) throw new Error('Crates wait for the matching published native release');
  let object = (await response.json()).object;
  for (let depth = 0; object?.type === 'tag' && depth < 5; depth++) {
    if (!/^[a-f0-9]{40}$/.test(object.sha ?? '')) throw new Error('Invalid release tag');
    response = await request(`${base}/git/tags/${object.sha}`, fetchImpl, token);
    if (response.status !== 200) throw new Error('Cannot resolve release tag');
    object = (await response.json()).object;
  }
  if (object?.type !== 'commit' || object.sha !== sourceCommit) throw new Error('Release tag differs from crate source');
  response = await request(`${base}/releases/tags/v${version}`, fetchImpl, token);
  if (response.status !== 200) throw new Error('Native release is missing');
  const release = await response.json();
  if (release.tag_name !== `v${version}` || release.draft !== false || release.prerelease !== prerelease) {
    throw new Error('Native release version/channel mismatch');
  }
}

async function archiveSource(bytes, name, version) {
  const directory = await mkdtemp(join(tmpdir(), 'wechat-crate-source-'));
  try {
    const archive = join(directory, 'source.crate');
    await writeFile(archive, bytes);
    return JSON.parse(execFileSync('tar', ['-xzOf', archive, `${name}-${version}/.cargo_vcs_info.json`],
      { encoding: 'utf8', maxBuffer: 1024 * 1024 }));
  } finally { await rm(directory, { recursive: true, force: true }); }
}

export async function publicationPlan({ version, sourceCommit, checksums, fetchImpl = fetch, readSource = archiveSource }) {
  const pending = [];
  const newCrates = [];
  for (const name of crateNames) {
    const base = `https://crates.io/api/v1/crates/${name}`;
    const response = await request(`${base}/${version}`, fetchImpl);
    if (response.status === 404) {
      const crate = await request(base, fetchImpl);
      if (crate.status === 404) newCrates.push(name);
      else if ((await crate.json()).crate?.repository?.replace(/\.git$/, '') !== `https://github.com/${repository}`) {
        throw new Error(`Crate name belongs to a different project: ${name}`);
      }
      pending.push(name);
      continue;
    }
    const published = (await response.json()).version;
    if (published?.crate !== name || published.num !== version || published.yanked !== false
      || !/^[a-f0-9]{64}$/.test(published.checksum ?? '')) throw new Error(`Invalid published crate: ${name}`);
    const download = await request(`https://static.crates.io/crates/${name}/${name}-${version}.crate`, fetchImpl);
    if (download.status !== 200) throw new Error(`Cannot verify existing crate source: ${name}`);
    const bytes = Buffer.from(await download.arrayBuffer());
    if (bytes.length > 20 * 1024 * 1024 || createHash('sha256').update(bytes).digest('hex') !== published.checksum
      || published.checksum !== checksums?.[name]) {
      throw new Error(`Published crate checksum mismatch: ${name}`);
    }
    const source = await readSource(bytes, name, version);
    if (source.git?.sha1 !== sourceCommit || source.git?.dirty === true
      || source.path_in_vcs !== `packages/native-client-rust/crates/${name}`) {
      throw new Error(`Published crate uses another source commit: ${name}`);
    }
  }
  return { version, sourceCommit, pending, newCrates, bootstrap: newCrates.length > 0 };
}

export async function main(command, { root = process.cwd(), allowDirty = false } = {}) {
  const policy = await readReleasePolicy({ root, version: process.env.RELEASE_VERSION });
  const cargo = args => execFileSync('cargo', [...args, '--manifest-path', join(root, workspace)],
    { cwd: root, stdio: 'inherit' });
  const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--no-deps', '--locked', '--format-version', '1',
    '--manifest-path', join(root, workspace)], { cwd: root, encoding: 'utf8' }));
  validateCrates(metadata, policy.version);
  if (command === 'check') return console.log(`Validated native library boundaries at ${policy.version}`);
  if (command === 'package') {
    // Cargo's multi-package mode verifies interdependent archives before their first publication.
    cargo(['package', '--locked', ...selections(crateNames), ...(allowDirty ? ['--allow-dirty'] : [])]);
    return;
  }
  if (!['plan', 'publish'].includes(command)) throw new Error('Usage: native-crates.mjs check|package|plan|publish [--allow-dirty]');
  if (allowDirty || !isReleaseContext(process.env)) {
    throw new Error('Registry publication is restricted to the main-branch native release workflow');
  }
  if (execFileSync('git', ['status', '--porcelain'], { cwd: root, encoding: 'utf8' }).trim()) throw new Error('Crate source must be clean');
  const sourceCommit = process.env.GITHUB_SHA;
  if (execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim() !== sourceCommit) throw new Error('Checkout differs from release source');
  await verifyReleaseSource({ ...policy, sourceCommit, token: process.env.GH_TOKEN });
  const checksums = {};
  const archiveDirectory = process.env.CRATE_ARCHIVE_DIR;
  if (!archiveDirectory) throw new Error('CRATE_ARCHIVE_DIR must contain this run\'s verified package artifacts');
  for (const name of crateNames) {
    const bytes = await readFile(join(archiveDirectory, `${name}-${policy.version}.crate`));
    const source = await archiveSource(bytes, name, policy.version);
    if (source.git?.sha1 !== sourceCommit || source.git?.dirty === true
      || source.path_in_vcs !== `packages/native-client-rust/crates/${name}`) throw new Error(`Package artifact source mismatch: ${name}`);
    checksums[name] = createHash('sha256').update(bytes).digest('hex');
  }
  const plan = await publicationPlan({ version: policy.version, sourceCommit, checksums });
  if (command === 'plan') {
    if (plan.bootstrap && process.env.CRATES_IO_BOOTSTRAP_AVAILABLE !== 'true') {
      throw new Error('First publication requires CRATES_IO_BOOTSTRAP_TOKEN in the protected native-release environment; configure trusted publishing and revoke it afterward');
    }
    if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT,
      `pending=${plan.pending.length > 0}\nbootstrap=${plan.bootstrap}\n`);
    return console.log(JSON.stringify(plan));
  }
  if (plan.pending.length) {
    if (!process.env.CARGO_REGISTRY_TOKEN) throw new Error('A registry publishing token is required');
    cargo(['publish', '--registry', 'crates-io', '--locked', ...selections(plan.pending)]);
  }
  // Registry transport errors and uncertain uploads stop the job. A rerun verifies already-published source before continuing.
  const verified = await publicationPlan({ version: policy.version, sourceCommit, checksums });
  if (verified.pending.length) throw new Error('Crate publication has not finished; inspect registry state before retrying');
  console.log(`Verified all four native crates at ${policy.version} (${sourceCommit})`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args.some(arg => arg.startsWith('--') && arg !== '--allow-dirty')) throw new Error('Unknown option');
  await main(args[0] ?? 'check', { allowDirty: args.includes('--allow-dirty') });
}
