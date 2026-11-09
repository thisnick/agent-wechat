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

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'wechat-versions-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const put = async (path, value) => {
    await mkdir(join(root, path, '..'), { recursive: true });
    await writeFile(join(root, path), typeof value === 'string' ? value : JSON.stringify(value));
  };
  await put('package.json', { name: 'fixture', private: true, packageManager: 'pnpm@9.15.4' });
  await put('pnpm-workspace.yaml', 'packages:\n  - packages/*\n');
  await put('.changeset/config.json', { changelog: [require.resolve('@changesets/cli/changelog'), null], fixed: [applicationPackages], linked: [],
    access: 'public', baseBranch: 'main', updateInternalDependencies: 'patch',
    privatePackages: { version: true, tag: false }, ignore: [] });
  for (const [directory, name] of [['cli', applicationPackages[0]], ['openclaw-extension', applicationPackages[1]],
    ['desktop', applicationPackages[2]], ['rust-crates', cratePackage.name]]) {
    await put(`packages/${directory}/package.json`, { name, version: '1.0.0', private: ['desktop', 'rust-crates'].includes(directory) });
  }
  await put('packages/native-client-rust/Cargo.toml', '[workspace.package]\nversion = "1.0.0"\n');
  for (const name of crateNames) await put(`packages/native-client-rust/crates/${name}/Cargo.toml`,
    `[package]\nname = "${name}"\nversion = "1.0.0"\n[dependencies]\nwechat-platform = { path = "../wechat-platform", version = "=1.0.0" }\n`);
  await put('packages/desktop/src-tauri/tauri.conf.json', { version: '1.0.0' });
  await put('packages/desktop/src-tauri/Cargo.toml', '[package]\nversion = "1.0.0"\n');
  execFileSync('git', ['init', '-b', 'main'], { cwd: root, stdio: 'pipe' });
  execFileSync('git', ['add', '.'], { cwd: root, stdio: 'pipe' });
  execFileSync('git', ['-c', 'user.name=Version test', '-c', 'user.email=version@example.invalid', 'commit', '-m', 'Fixture'], { cwd: root, stdio: 'pipe' });
  return { root, put };
}

for (const scenario of [
  { name: 'application only', changes: '"@agent-wechat/cli": patch', app: '1.0.1', crates: '1.0.0' },
  { name: 'libraries only', changes: '"@agent-wechat/rust-crates": minor', app: '1.0.0', crates: '1.1.0' },
  { name: 'both with different bumps', changes: '"@agent-wechat/cli": patch\n"@agent-wechat/rust-crates": major', app: '1.0.1', crates: '2.0.0' },
]) test(`real Changesets versioning keeps ${scenario.name} releases independent`, async t => {
  const { root, put } = await fixture(t);
  await put('.changeset/change.md', `---\n${scenario.changes}\n---\n\nDescribe a public change.\n`);
  const cli = require.resolve('@changesets/cli/bin.js');
  execFileSync(process.execPath, [cli, 'version'], { cwd: root, stdio: 'pipe' });
  // Cargo's lockfile updates are checked separately with the real workspace.
  const result = await syncVersions(root, { runCargo() {} });
  assert.deepEqual(result, { applicationVersion: scenario.app, crateVersion: scenario.crates });
  for (const directory of ['cli', 'openclaw-extension', 'desktop']) {
    assert.equal(JSON.parse(await readFile(join(root, `packages/${directory}/package.json`), 'utf8')).version, scenario.app);
  }
  for (const name of crateNames) {
    const manifest = await readFile(join(root, `packages/native-client-rust/crates/${name}/Cargo.toml`), 'utf8');
    assert.ok(manifest.includes(`version = "${scenario.crates}"`));
    assert.ok(manifest.includes(`version = "=${scenario.crates}"`));
  }
  assert.ok((await readFile(join(root, 'packages/native-client-rust/Cargo.toml'), 'utf8')).includes(`version = "${scenario.app}"`));
  const pkg = JSON.parse(await readFile(join(root, cratePackage.directory, 'package.json'), 'utf8'));
  assert.equal(pkg.private, true);
  if (scenario.crates !== '1.0.0') assert.match(await readFile(join(root, cratePackage.directory, 'CHANGELOG.md'), 'utf8'), /Describe a public change/);
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
