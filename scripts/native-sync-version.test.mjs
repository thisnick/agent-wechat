import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { syncVersions } from './native-sync-version.mjs';
import { crateNames } from './native-crates.mjs';
import { cratePackage } from './native-crates-version.mjs';

const require = createRequire(import.meta.url);
const applicationPackages = ['@agent-wechat/cli', '@agent-wechat/agent-wechat', '@agent-wechat/desktop'];
// Use the repository's own fixed group so this test fails if the libraries leave it.
const { fixed } = JSON.parse(await readFile(new URL('../.changeset/config.json', import.meta.url), 'utf8'));

async function fixture(t, crateVersion = '1.0.0') {
  const root = await mkdtemp(join(tmpdir(), 'wechat-versions-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const put = async (path, value) => {
    await mkdir(join(root, path, '..'), { recursive: true });
    await writeFile(join(root, path), typeof value === 'string' ? value : JSON.stringify(value));
  };
  await put('package.json', { name: 'fixture', private: true, packageManager: 'pnpm@9.15.4' });
  await put('pnpm-workspace.yaml', 'packages:\n  - packages/*\n');
  await put('.changeset/config.json', { changelog: [require.resolve('@changesets/cli/changelog'), null], fixed, linked: [],
    access: 'public', baseBranch: 'main', updateInternalDependencies: 'patch',
    privatePackages: { version: true, tag: false }, ignore: [] });
  for (const [directory, name] of [['cli', applicationPackages[0]], ['openclaw-extension', applicationPackages[1]],
    ['desktop', applicationPackages[2]], ['rust-crates', cratePackage.name]]) {
    await put(`packages/${directory}/package.json`, { name, version: directory === 'rust-crates' ? crateVersion : '1.0.0',
      private: ['desktop', 'rust-crates'].includes(directory) });
  }
  await put('packages/native-client-rust/Cargo.toml', '[workspace.package]\nversion = "1.0.0"\n');
  for (const name of crateNames) await put(`packages/native-client-rust/crates/${name}/Cargo.toml`,
    `[package]\nname = "${name}"\nversion = "${crateVersion}"\n[dependencies]\nwechat-platform = { path = "../wechat-platform", version = "=${crateVersion}" }\n`);
  await put('packages/desktop/src-tauri/tauri.conf.json', { version: '1.0.0' });
  await put('packages/desktop/src-tauri/Cargo.toml', '[package]\nversion = "1.0.0"\n');
  execFileSync('git', ['init', '-b', 'main'], { cwd: root, stdio: 'pipe' });
  execFileSync('git', ['add', '.'], { cwd: root, stdio: 'pipe' });
  execFileSync('git', ['-c', 'user.name=Version test', '-c', 'user.email=version@example.invalid', 'commit', '-m', 'Fixture'], { cwd: root, stdio: 'pipe' });
  return { root, put };
}

test('the fixed group holds the applications and the Rust library package', () => {
  assert.deepEqual(fixed, [[...applicationPackages, cratePackage.name]]);
});

for (const scenario of [
  { name: 'an application change', changes: '"@agent-wechat/cli": patch', version: '1.0.1' },
  { name: 'a library change', changes: '"@agent-wechat/rust-crates": minor', version: '1.1.0', libraryNotes: true },
  { name: 'different bumps', changes: '"@agent-wechat/cli": patch\n"@agent-wechat/rust-crates": major', version: '2.0.0', libraryNotes: true },
  { name: 'libraries that start behind the applications', changes: '"@agent-wechat/cli": patch', version: '1.0.1', crateStart: '0.9.0' },
]) test(`real Changesets versioning gives applications and libraries one version for ${scenario.name}`, async t => {
  const { root, put } = await fixture(t, scenario.crateStart);
  await put('.changeset/change.md', `---\n${scenario.changes}\n---\n\nDescribe a public change.\n`);
  const cli = require.resolve('@changesets/cli/bin.js');
  execFileSync(process.execPath, [cli, 'version'], { cwd: root, stdio: 'pipe' });
  // Cargo's lockfile updates are checked separately with the real workspace.
  const result = await syncVersions(root, { runCargo() {} });
  assert.deepEqual(result, { applicationVersion: scenario.version, crateVersion: scenario.version });
  for (const directory of ['cli', 'openclaw-extension', 'desktop']) {
    assert.equal(JSON.parse(await readFile(join(root, `packages/${directory}/package.json`), 'utf8')).version, scenario.version);
  }
  for (const name of crateNames) {
    const manifest = await readFile(join(root, `packages/native-client-rust/crates/${name}/Cargo.toml`), 'utf8');
    assert.ok(manifest.includes(`version = "${scenario.version}"`));
    assert.ok(manifest.includes(`version = "=${scenario.version}"`));
  }
  assert.ok((await readFile(join(root, 'packages/native-client-rust/Cargo.toml'), 'utf8')).includes(`version = "${scenario.version}"`));
  const pkg = JSON.parse(await readFile(join(root, cratePackage.directory, 'package.json'), 'utf8'));
  assert.equal(pkg.private, true);
  // Every release writes one exact library changelog heading, with or without library notes.
  const changelog = await readFile(join(root, cratePackage.directory, 'CHANGELOG.md'), 'utf8');
  assert.equal(changelog.split(`## ${scenario.version}\n`).length, 2);
  assert.equal(changelog.includes('Describe a public change'), Boolean(scenario.libraryNotes));
});

test('the new private library marker participates in the existing beta-exit version PR', async t => {
  const { root, put } = await fixture(t);
  for (const [directory, name] of [['cli', applicationPackages[0]], ['openclaw-extension', applicationPackages[1]],
    ['desktop', applicationPackages[2]], ['rust-crates', cratePackage.name]]) {
    await put(`packages/${directory}/package.json`, { name, version: '2.0.0-beta.1', private: ['desktop', 'rust-crates'].includes(directory) });
  }
  await put('.changeset/pre.json', { mode: 'exit', tag: 'beta',
    initialVersions: Object.fromEntries(applicationPackages.map(name => [name, '1.5.1'])), changesets: [] });
  await put('.changeset/change.md', '---\n"@agent-wechat/rust-crates": patch\n---\n\nSeparate library versions.\n');
  execFileSync(process.execPath, [require.resolve('@changesets/cli/bin.js'), 'version'], { cwd: root, stdio: 'pipe' });
  assert.deepEqual(await syncVersions(root, { runCargo() {} }), { applicationVersion: '2.0.0', crateVersion: '2.0.0' });
  assert.match(await readFile(join(root, cratePackage.directory, 'CHANGELOG.md'), 'utf8'), /Separate library versions/);
});
