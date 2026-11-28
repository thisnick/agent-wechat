import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, readlink, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { ensureWxOnPath, findWxOnPath, isDirOnPath, resolveGlobalBinDir, shellProfileHint } from '../scripts/native-path.mjs';
import { assetName, manifestName, supportedTargets, targetFor, targetKey } from '../scripts/native-runtime.mjs';

const exec = promisify(execFile);
const version = '3.2.1';
const supportedTarget = targetFor();

function fakeWxScript(ver, product = 'agent-wechat') {
  const payload = JSON.stringify({
    product,
    backend: 'native',
    version: ver,
    apiVersion: 1,
    target: supportedTarget.rustTarget,
    capabilities: ['serve'],
  });
  return `#!/bin/sh\nif [ "$1" = "--json" ] && [ "$2" = "version" ]; then\n  printf '%s\\n' '${payload}'\n  exit 0\nfi\necho "fake-wx ${ver}"\n`;
}

async function createGlobalNpmFixture(t, { pkgVersion = version, createBinLink = true } = {}) {
  const base = await mkdtemp(join(tmpdir(), 'wx-native-path-'));
  t.after(() => rm(base, { recursive: true, force: true }));

  const home = join(base, 'home');
  const localBin = join(home, '.local', 'bin');
  const prefix = join(base, 'npm-global');
  const binDir = join(prefix, 'bin');
  const pkgRoot = join(prefix, 'lib', 'node_modules', '@agent-wechat', 'cli');
  const scriptsDir = join(pkgRoot, 'scripts');
  const artifacts = join(base, 'artifacts');

  await mkdir(localBin, { recursive: true });
  await mkdir(binDir, { recursive: true });
  await mkdir(scriptsDir, { recursive: true });
  await mkdir(artifacts, { recursive: true });

  for (const name of ['native-install.mjs', 'native-launcher.mjs', 'native-runtime.mjs', 'native-path.mjs']) {
    await copyFile(new URL(`../scripts/${name}`, import.meta.url), join(scriptsDir, name));
  }
  await chmod(join(scriptsDir, 'native-launcher.mjs'), 0o755);
  await chmod(join(scriptsDir, 'native-install.mjs'), 0o755);

  const bytes = Buffer.from(fakeWxScript(pkgVersion));
  const asset = {
    name: assetName(pkgVersion),
    ...supportedTarget,
    bytes: bytes.length,
    sha256: createHash('sha256').update(bytes).digest('hex'),
  };
  const manifest = {
    schemaVersion: 1,
    version: pkgVersion,
    repository: 'thisnick/agent-wechat',
    sourceCommit: 'a'.repeat(40),
    sourceTreeDirty: false,
    assets: [
      asset,
      ...supportedTargets
        .filter(target => targetKey(target) !== targetKey(supportedTarget))
        .map(target => ({ bytes: asset.bytes, sha256: asset.sha256, ...target, name: assetName(pkgVersion, target) })),
    ],
  };

  await writeFile(join(pkgRoot, 'package.json'), JSON.stringify({
    name: '@agent-wechat/cli',
    version: pkgVersion,
    type: 'module',
    bin: { wx: './scripts/native-launcher.mjs', 'wx-native-install': './scripts/native-install.mjs' },
  }));
  await writeFile(join(pkgRoot, 'native-release.json'), JSON.stringify(manifest));
  await writeFile(join(artifacts, manifestName(pkgVersion)), JSON.stringify(manifest));
  await writeFile(join(artifacts, asset.name), bytes, { mode: 0o755 });

  const binWx = join(binDir, 'wx');
  const binInstall = join(binDir, 'wx-native-install');
  if (createBinLink) {
    await symlink('../lib/node_modules/@agent-wechat/cli/scripts/native-launcher.mjs', binWx);
    await symlink('../lib/node_modules/@agent-wechat/cli/scripts/native-install.mjs', binInstall);
  }

  return { base, home, localBin, prefix, binDir, pkgRoot, artifacts, binWx, binInstall };
}

test('does nothing during postinstall when npm global bin folder is already on PATH', async t => {
  const { home, localBin, prefix, binDir, pkgRoot } = await createGlobalNpmFixture(t);
  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${binDir}:${localBin}:/usr/bin`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'on-path');
  assert.deepEqual(logs, []);
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);
});

test('recognizes symlinked PATH entry pointing at npm global bin folder', async t => {
  const { base, home, localBin, prefix, binDir, pkgRoot } = await createGlobalNpmFixture(t);
  const aliasBin = join(base, 'current-node-bin');
  await symlink(binDir, aliasBin);
  assert.equal(await isDirOnPath(binDir, { env: { PATH: `${aliasBin}:/usr/bin` }, platform: 'linux' }), true);
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${aliasBin}:${localBin}`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: () => {},
  });
  assert.equal(result.action, 'on-path');
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);
});

test('links ~/.local/bin/wx to npm wx launcher when ~/.local/bin is on PATH and has no wx', async t => {
  const { home, localBin, prefix, binDir, pkgRoot, binWx } = await createGlobalNpmFixture(t);
  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix, SHELL: '/bin/bash' },
    platform: 'linux',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'linked');
  assert.equal(result.linkPath, join(localBin, 'wx'));
  assert.equal(result.target, binWx);
  const statInfo = await lstat(join(localBin, 'wx'));
  assert.equal(statInfo.isSymbolicLink(), true);
  assert.equal(await readlink(join(localBin, 'wx')), binWx);
  const output = logs.join('\n');
  assert.match(output, /Linked wx into PATH:/);
  assert.match(output, new RegExp(`rm ${join(localBin, 'wx').replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`));
  assert.equal(await findWxOnPath({ env: { PATH: `${localBin}:/usr/bin` }, platform: 'linux' }), join(localBin, 'wx'));
});

test('creates ~/.local/bin directory if it is listed on PATH but does not yet exist', async t => {
  const { home, localBin, prefix, pkgRoot, binWx } = await createGlobalNpmFixture(t);
  await rm(localBin, { recursive: true, force: true });
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: () => {},
  });
  assert.equal(result.action, 'linked');
  assert.equal(await readlink(join(localBin, 'wx')), binWx);
});

test('re-running postinstall or --link when ~/.local/bin/wx already links to this install is idempotent', async t => {
  const { home, localBin, prefix, pkgRoot, binWx } = await createGlobalNpmFixture(t);
  await symlink(binWx, join(localBin, 'wx'));

  const postLogs = [];
  const postResult = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: line => postLogs.push(line),
  });
  assert.equal(postResult.action, 'already-linked');
  assert.deepEqual(postLogs, []);

  const linkLogs = [];
  const linkResult = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: false,
    log: line => linkLogs.push(line),
  });
  assert.equal(linkResult.action, 'already-linked');
  assert.match(linkLogs.join('\n'), /already linked/);
  assert.match(linkLogs.join('\n'), /rm /);
});

test('leaves another wx on PATH alone and warns when its version differs', async t => {
  const { base, home, localBin, prefix, binDir, pkgRoot } = await createGlobalNpmFixture(t);
  const desktopBin = join(base, 'desktop-app');
  await mkdir(desktopBin, { recursive: true });
  const desktopWx = join(desktopBin, 'wx');
  await writeFile(desktopWx, fakeWxScript('3.0.0'), { mode: 0o755 });
  await symlink(desktopWx, join(localBin, 'wx'));

  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'other-on-path');
  assert.equal(result.existingPath, join(localBin, 'wx'));
  assert.equal(result.existingVersion, '3.0.0');
  assert.equal(result.packageVersion, version);
  assert.equal(result.versionMismatch, true);
  // Desktop symlink must stay intact.
  assert.equal(await readlink(join(localBin, 'wx')), desktopWx);
  const output = logs.join('\n');
  assert.match(output, /Warning: another wx \(3\.0\.0\) is already on PATH/);
  assert.match(output, new RegExp(version.replace(/\./g, '\\.')));
  assert.match(output, new RegExp(binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
});

test('leaves another wx on PATH alone without mismatch warning when versions match', async t => {
  const { base, home, localBin, prefix, pkgRoot } = await createGlobalNpmFixture(t);
  const otherBin = join(base, 'other-bin');
  await mkdir(otherBin, { recursive: true });
  const otherWx = join(otherBin, 'wx');
  await writeFile(otherWx, fakeWxScript(version), { mode: 0o755 });

  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${otherBin}:${localBin}`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'other-on-path');
  assert.equal(result.existingPath, otherWx);
  assert.equal(result.versionMismatch, false);
  assert.doesNotMatch(logs.join('\n'), /Warning:/);
  assert.match(logs.join('\n'), /leaving it unchanged/);
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);
});

test('leaves an unrelated wx on PATH alone and warns when version cannot be confirmed', async t => {
  const { base, home, localBin, prefix, pkgRoot } = await createGlobalNpmFixture(t);
  const otherBin = join(base, 'other-bin');
  await mkdir(otherBin, { recursive: true });
  const otherWx = join(otherBin, 'wx');
  await writeFile(otherWx, '#!/bin/sh\necho "unrelated tool"\n', { mode: 0o755 });

  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${otherBin}:${localBin}`, npm_config_prefix: prefix },
    platform: 'linux',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'other-on-path');
  assert.equal(result.existingVersion, null);
  assert.equal(result.versionMismatch, true);
  assert.match(logs.join('\n'), /Warning: another wx is already on PATH/);
  assert.match(logs.join('\n'), /could not confirm/);
});

test('never replaces an existing non-executable file or foreign dangling symlink in ~/.local/bin/wx', async t => {
  const { home, localBin, prefix, binDir, pkgRoot } = await createGlobalNpmFixture(t);

  // Case 1: regular non-executable file in ~/.local/bin/wx
  const dest = join(localBin, 'wx');
  await writeFile(dest, 'user file', { mode: 0o644 });
  const fileLogs = [];
  const fileResult = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix, SHELL: '/bin/bash' },
    platform: 'linux',
    postinstall: true,
    log: line => fileLogs.push(line),
  });
  assert.equal(fileResult.action, 'conflict');
  assert.equal(await readFile(dest, 'utf8'), 'user file');
  assert.match(fileLogs.join('\n'), /was not replaced/);
  assert.match(fileLogs.join('\n'), new RegExp(`export PATH="${binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}:\\$PATH"`));

  // Case 2: foreign dangling symlink in ~/.local/bin/wx
  await rm(dest);
  await symlink('/nonexistent/other/wx', dest);
  const linkLogs = [];
  const linkResult = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: `${localBin}:/usr/bin`, npm_config_prefix: prefix, SHELL: '/bin/zsh' },
    platform: 'linux',
    postinstall: true,
    log: line => linkLogs.push(line),
  });
  assert.equal(linkResult.action, 'conflict');
  assert.equal(await readlink(dest), '/nonexistent/other/wx');
  assert.match(linkLogs.join('\n'), /was not replaced/);
  assert.match(linkLogs.join('\n'), /~\/\.zshrc/);
});

test('prints shell profile line when ~/.local/bin is not on PATH (e.g. macOS default)', async t => {
  const { home, localBin, prefix, binDir, pkgRoot } = await createGlobalNpmFixture(t);
  const logs = [];
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, PATH: '/usr/local/bin:/usr/bin:/bin', npm_config_prefix: prefix, SHELL: '/bin/zsh' },
    platform: 'darwin',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'hint');
  assert.equal(result.hint.file, '~/.zshrc');
  assert.equal(result.hint.line, `export PATH="${binDir}:$PATH"`);
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);
  const output = logs.join('\n');
  assert.match(output, /~\/\.zshrc/);
  assert.match(output, new RegExp(`export PATH="${binDir.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}:\\$PATH"`));
});

test('formats shell profile hints for bash, zsh, fish, sh, and Windows PowerShell', () => {
  const bashLinux = shellProfileHint('/opt/npm/bin', { env: { SHELL: '/bin/bash' }, platform: 'linux' });
  assert.equal(bashLinux.file, '~/.bashrc');
  assert.equal(bashLinux.line, 'export PATH="/opt/npm/bin:$PATH"');

  const bashMac = shellProfileHint('/opt/npm/bin', { env: { SHELL: '/bin/bash' }, platform: 'darwin' });
  assert.equal(bashMac.file, '~/.bash_profile');

  const fish = shellProfileHint('/opt/my npm/bin', { env: { SHELL: '/usr/bin/fish' }, platform: 'linux' });
  assert.equal(fish.file, '~/.config/fish/config.fish');
  assert.equal(fish.line, "fish_add_path '/opt/my npm/bin'");

  const sh = shellProfileHint('/opt/npm/bin', { env: { SHELL: '' }, platform: 'linux' });
  assert.equal(sh.file, '~/.profile');

  const win = shellProfileHint('C:\\Users\\alice\\AppData\\Roaming\\npm', { env: {}, platform: 'win32' });
  assert.equal(win.shell, 'powershell');
  assert.match(win.line, /SetEnvironmentVariable\('Path'/);
  assert.match(win.line, /C:\\Users\\alice\\AppData\\Roaming\\npm/);
});

test('Windows prints hint instead of creating ~/.local/bin symlink when prefix is not on PATH', async t => {
  const { home, localBin, pkgRoot } = await createGlobalNpmFixture(t);
  const logs = [];
  const winPrefix = 'C:\\Users\\alice\\AppData\\Roaming\\npm';
  const result = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, Path: 'C:\\Windows\\System32', npm_config_prefix: winPrefix },
    platform: 'win32',
    postinstall: true,
    log: line => logs.push(line),
  });
  assert.equal(result.action, 'hint');
  assert.equal(result.binDir, winPrefix);
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);
  assert.match(logs.join('\n'), /PowerShell/);

  // And when prefix IS on Windows Path (case-insensitive or double-quoted), does nothing.
  const onPath = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, Path: 'C:\\Windows\\System32;c:\\users\\alice\\appdata\\roaming\\npm\\', npm_config_prefix: winPrefix },
    platform: 'win32',
    postinstall: true,
    log: () => {},
  });
  assert.equal(onPath.action, 'on-path');

  const quotedOnPath = await ensureWxOnPath({
    root: pkgRoot,
    env: { HOME: home, Path: `"${winPrefix}";C:\\Windows\\System32`, npm_config_prefix: winPrefix },
    platform: 'win32',
    postinstall: true,
    log: () => {},
  });
  assert.equal(quotedOnPath.action, 'on-path');
});

test('resolves global bin folder for npm_config_global_prefix, pnpm (POSIX and Windows), and rejects _npx and local node_modules/.bin', async t => {
  const base = await mkdtemp(join(tmpdir(), 'wx-pnpm-path-'));
  t.after(() => rm(base, { recursive: true, force: true }));

  const pnpmHome = join(base, 'pnpm');
  const pnpmPkg = join(pnpmHome, 'global', '5', '.pnpm', '@agent-wechat+cli@3.2.1', 'node_modules', '@agent-wechat', 'cli');
  await mkdir(pnpmPkg, { recursive: true });
  await writeFile(join(pnpmHome, 'wx'), '#!/bin/sh\n', { mode: 0o755 });
  await writeFile(join(pnpmHome, 'wx.cmd'), '@echo off\r\n');

  // POSIX: without PNPM_HOME set, discovers pnpmHome from global/ ancestor.
  assert.equal(await resolveGlobalBinDir({ root: pnpmPkg, env: {}, platform: 'linux', argv1: join(pnpmPkg, 'scripts', 'native-install.mjs') }), pnpmHome);
  // POSIX: with PNPM_HOME set, discovers pnpmHome directly.
  assert.equal(await resolveGlobalBinDir({ root: join(base, 'other'), env: { PNPM_HOME: pnpmHome }, platform: 'linux', argv1: '' }), pnpmHome);
  // Windows: pnpm global install inside .pnpm is NOT misidentified by npm node_modules derivation.
  const winPnpmHome = 'C:\\Users\\alice\\AppData\\Local\\pnpm';
  const winPnpmPkg = `${winPnpmHome}\\global\\5\\.pnpm\\@agent-wechat+cli@3.2.1\\node_modules\\@agent-wechat\\cli`;
  const winFiles = new Set([`${winPnpmHome}\\wx.cmd`]);
  assert.equal(await resolveGlobalBinDir({
    root: winPnpmPkg,
    env: {},
    platform: 'win32',
    argv1: '',
    exists: async p => winFiles.has(p),
  }), winPnpmHome);
  // Windows: local project node_modules without wx.cmd at project root is rejected.
  assert.equal(await resolveGlobalBinDir({
    root: 'C:\\Users\\alice\\my-project\\node_modules\\@agent-wechat\\cli',
    env: {},
    platform: 'win32',
    argv1: '',
    exists: async () => false,
  }), null);
  // Local ./node_modules/.bin/wx-native-install is rejected.
  const localProjectPkg = join(base, 'my-project', 'node_modules', '@agent-wechat', 'cli');
  assert.equal(await resolveGlobalBinDir({
    root: localProjectPkg,
    env: {},
    platform: 'linux',
    argv1: join(base, 'my-project', 'node_modules', '.bin', 'wx-native-install'),
  }), null);
  // Temporary _npx cache directory is not treated as a global prefix, even if npm_config_global_prefix points to an empty prefix.
  const npxRoot = join(base, '.npm', '_npx', 'deadbeef', 'node_modules', '@agent-wechat', 'cli');
  const emptyPrefix = join(base, 'empty-prefix');
  await mkdir(join(emptyPrefix, 'bin'), { recursive: true });
  assert.equal(await resolveGlobalBinDir({
    root: npxRoot,
    env: { npm_config_global_prefix: emptyPrefix },
    platform: 'linux',
    argv1: join(npxRoot, 'scripts', 'native-install.mjs'),
  }), null);
  // When npm_config_global_prefix DOES contain bin/wx (e.g. default npm prefix), --link resolves it cleanly.
  await writeFile(join(emptyPrefix, 'bin', 'wx'), '#!/bin/sh\n', { mode: 0o755 });
  assert.equal(await resolveGlobalBinDir({
    root: npxRoot,
    env: { npm_config_global_prefix: emptyPrefix },
    platform: 'linux',
    argv1: join(npxRoot, 'scripts', 'native-install.mjs'),
  }), join(emptyPrefix, 'bin'));
});

test('native-install.mjs --postinstall links only on global installs, and --link works standalone', async t => {
  const { home, localBin, prefix, pkgRoot, artifacts, binWx, binInstall } = await createGlobalNpmFixture(t);
  const installScript = join(pkgRoot, 'scripts', 'native-install.mjs');
  const baseEnv = {
    PATH: `${localBin}:/usr/bin:/bin`,
    HOME: home,
    SHELL: '/bin/bash',
    AGENT_WECHAT_NATIVE_ARTIFACT_DIR: artifacts,
    AGENT_WECHAT_NATIVE_BINARY: '',
    AGENT_WECHAT_SKIP_NATIVE_INSTALL: '',
    npm_config_prefix: prefix,
  };

  // 1. Local postinstall (npm_config_global unset) installs runtime but does NOT link ~/.local/bin/wx.
  await exec(process.execPath, [installScript, '--postinstall'], { env: baseEnv });
  await assert.rejects(lstat(join(localBin, 'wx')), /ENOENT/);

  // 2. Global postinstall (npm_config_global=true) links ~/.local/bin/wx -> prefix/bin/wx.
  const globalRes = await exec(process.execPath, [installScript, '--postinstall'], {
    env: { ...baseEnv, npm_config_global: 'true' },
  });
  assert.equal(await readlink(join(localBin, 'wx')), binWx);
  assert.match(globalRes.stderr, /Linked wx into PATH:/);

  // Executing ~/.local/bin/wx runs the installed native runtime!
  const wxOut = await exec(join(localBin, 'wx'), ['--json', 'version'], {
    env: { PATH: `${localBin}:${join(process.execPath, '..')}:/usr/bin:/bin`, HOME: home },
  });
  assert.equal(JSON.parse(wxOut.stdout).version, version);

  // 3. Remove link and verify standalone `wx-native-install --link` (without npm_config_global or npm_config_prefix) recreates it.
  await rm(join(localBin, 'wx'));
  const linkRes = await exec(process.execPath, [binInstall, '--link'], {
    env: { PATH: `${localBin}:/usr/bin:/bin`, HOME: home, SHELL: '/bin/bash' },
  });
  assert.equal(await readlink(join(localBin, 'wx')), binWx);
  assert.match(linkRes.stderr, /Linked wx into PATH:/);

  // 4. --help documents --link and exits 0; unknown flag exits 1.
  const helpRes = await exec(process.execPath, [installScript, '--help']);
  assert.match(helpRes.stdout, /--link/);
  const badFlag = await exec(process.execPath, [installScript, '--bogus']).catch(e => e);
  assert.equal(badFlag.code, 1);
  assert.match(badFlag.stderr, /Unknown option: --bogus/);
});
