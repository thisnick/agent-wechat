import test from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { lstat, mkdir, mkdtemp, readlink, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
  compareSemver,
  detectOutdatedDaemon,
  formatUpgradeNotice,
  isOlderVersion,
  parseServerConfig,
  resolveDataDir,
  resolveServerConfigPath,
  shouldUpdateStableBinary,
  stableBinaryPath,
  updateStableBinary,
  versionFromBinaryPath,
} from '../scripts/stable-binary.mjs';

async function tempDir(t) {
  const dir = await mkdtemp(join(tmpdir(), 'wx-stable-binary-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  return dir;
}

async function loopbackServer(t, handler) {
  const server = createServer(handler);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => {
    server.closeAllConnections();
    return new Promise(done => server.close(done));
  });
  return server.address().port;
}

test('resolveDataDir and stableBinaryPath match XDG and env precedence', () => {
  assert.equal(resolveDataDir({ env: { HOME: '/home/alice' }, platform: 'linux' }), '/home/alice/.local/share/agent-wechat');
  assert.equal(resolveDataDir({ env: { HOME: '/home/alice', XDG_DATA_HOME: '/data' }, platform: 'linux' }), '/data/agent-wechat');
  assert.equal(resolveDataDir({ env: { HOME: '/home/alice', XDG_DATA_HOME: 'relative' }, platform: 'linux' }), '/home/alice/.local/share/agent-wechat');
  assert.equal(resolveDataDir({ env: { HOME: '/home/alice', AGENT_WECHAT_DATA_DIR: '/custom/wx' }, platform: 'linux' }), '/custom/wx');
  assert.equal(stableBinaryPath({ env: { HOME: '/home/alice' }, platform: 'linux' }), '/home/alice/.local/share/agent-wechat/bin/wx');
  assert.equal(resolveServerConfigPath({ env: { HOME: '/home/alice' }, platform: 'linux' }), '/home/alice/.config/agent-wechat/server/config.toml');
  assert.equal(resolveServerConfigPath({ env: { HOME: '/home/alice', XDG_CONFIG_HOME: '/cfg' }, platform: 'linux' }), '/cfg/agent-wechat/server/config.toml');
  assert.equal(resolveDataDir({ env: { LOCALAPPDATA: 'C:\\Users\\alice\\AppData\\Local' }, platform: 'win32' }), 'C:\\Users\\alice\\AppData\\Local\\agent-wechat');
  assert.equal(stableBinaryPath({ env: { LOCALAPPDATA: 'C:\\Users\\alice\\AppData\\Local' }, platform: 'win32' }), 'C:\\Users\\alice\\AppData\\Local\\agent-wechat\\bin\\wx.exe');
  assert.equal(resolveDataDir({ env: {}, platform: 'linux' }), null);
});

test(
  'updateStableBinary creates private directories and atomically updates symlink across upgrades',
  { skip: process.platform === 'win32' },
  async t => {
    const root = await tempDir(t);
    const env = { HOME: root, XDG_DATA_HOME: join(root, 'xdg-data') };
    const v1 = join(root, 'native', '3.2.0', 'linux-x64-gnu', 'wx');
    const v2 = join(root, 'native', '3.2.1', 'linux-x64-gnu', 'wx');
    await mkdir(join(root, 'native', '3.2.0', 'linux-x64-gnu'), { recursive: true });
    await mkdir(join(root, 'native', '3.2.1', 'linux-x64-gnu'), { recursive: true });
    await writeFile(v1, 'v1', { mode: 0o755 });
    await writeFile(v2, 'v2', { mode: 0o755 });

    const link = await updateStableBinary(v1, { env, platform: 'linux' });
    assert.equal(link, join(root, 'xdg-data', 'agent-wechat', 'bin', 'wx'));
    assert.equal(await readlink(link), v1);
    const dataInfo = await stat(join(root, 'xdg-data', 'agent-wechat'));
    assert.equal(dataInfo.mode & 0o077, 0, 'data directory must be private (0700)');
    const binInfo = await stat(join(root, 'xdg-data', 'agent-wechat', 'bin'));
    assert.equal(binInfo.mode & 0o077, 0, 'bin directory must be private (0700)');

    const updated = await updateStableBinary(v2, { env, platform: 'linux' });
    assert.equal(updated, link);
    assert.equal(await readlink(link), v2);
    assert.ok((await lstat(link)).isSymbolicLink());

    // Never downgrade a live stable link to an older version (e.g. npx or older install).
    assert.equal(await updateStableBinary(v1, { env, platform: 'linux' }), null);
    assert.equal(await readlink(link), v2);

    // If the newer target was deleted (dangling symlink), replace it cleanly.
    await rm(v2);
    assert.equal(await updateStableBinary(v1, { env, platform: 'linux' }), link);
    assert.equal(await readlink(link), v1);
  },
);

test('shouldUpdateStableBinary restricts postinstall link updates to global installs', () => {
  assert.equal(shouldUpdateStableBinary({ isPostinstall: true, env: { npm_config_global: 'true' } }), true);
  assert.equal(shouldUpdateStableBinary({ isPostinstall: true, env: { npm_config_global: 'false' } }), false);
  assert.equal(shouldUpdateStableBinary({ isPostinstall: true, env: {} }), false);
  assert.equal(shouldUpdateStableBinary({ isPostinstall: false, env: {} }), true);
  assert.equal(versionFromBinaryPath('/opt/pkg/native/3.2.1/linux-x64-gnu/wx'), '3.2.1');
  assert.equal(versionFromBinaryPath('/usr/local/bin/wx'), null);
});

test(
  'updateStableBinary rejects relative targets, symlinked data dirs, and skips sudo root installs into a user HOME',
  { skip: process.platform === 'win32' },
  async t => {
    const root = await tempDir(t);
    await assert.rejects(updateStableBinary('relative/wx', { env: { HOME: root }, platform: 'linux' }), /absolute/);

    const realTarget = join(root, 'real');
    await mkdir(realTarget);
    const fakeXdg = join(root, 'xdg');
    await mkdir(fakeXdg);
    await symlink(realTarget, join(fakeXdg, 'agent-wechat'));
    await assert.rejects(
      updateStableBinary(join(root, 'wx'), { env: { HOME: root, XDG_DATA_HOME: fakeXdg }, platform: 'linux' }),
      /symlink/,
    );
    assert.equal(
      await updateStableBinary(join(root, 'wx'), {
        env: { HOME: root, XDG_DATA_HOME: join(root, 'sudo-data'), SUDO_UID: '1000' },
        platform: 'linux',
        euid: 0,
      }),
      null,
    );
    await assert.rejects(lstat(join(root, 'sudo-data')), /ENOENT/);
    assert.equal(await updateStableBinary('C:\\wx\\wx.exe', { env: { LOCALAPPDATA: 'C:\\Local' }, platform: 'win32' }), null);
  },
);

test('semver comparison distinguishes upgrades, equal versions, downgrades and prereleases', () => {
  assert.equal(compareSemver('3.2.0', '3.2.1'), -1);
  assert.equal(compareSemver('3.2.1', '3.2.1'), 0);
  assert.equal(compareSemver('3.3.0', '3.2.1'), 1);
  assert.equal(compareSemver('3.2.1-beta.1', '3.2.1'), -1);
  assert.equal(compareSemver('3.2.1-beta.2', '3.2.1-beta.10'), -1);
  assert.equal(compareSemver('invalid', '3.2.1'), null);
  assert.equal(isOlderVersion('3.2.0', '3.2.1'), true);
  assert.equal(isOlderVersion('3.2.1', '3.2.1'), false);
  assert.equal(isOlderVersion('3.3.0', '3.2.1'), false);
});

test('parseServerConfig reads root keys and resolves relative paths against config dir', () => {
  const parsed = parseServerConfig(
    `schemaVersion = 1\ndataDir = "data" # inline comment\nlisten = '127.0.0.1:6180'\ntokenFile = "/abs/token"\n[other]\nlisten = "127.0.0.1:9999"\n`,
    '/etc/agent-wechat/server',
    { platform: 'linux' },
  );
  assert.deepEqual(parsed, {
    dataDir: '/etc/agent-wechat/server/data',
    listen: '127.0.0.1:6180',
    tokenFile: '/abs/token',
  });
});

test('detectOutdatedDaemon reports an older running daemon without touching or restarting it', async t => {
  const root = await tempDir(t);
  const xdgConfig = join(root, 'config');
  const xdgData = join(root, 'data');
  const dataDir = join(xdgData, 'agent-wechat');
  await mkdir(join(xdgConfig, 'agent-wechat', 'server'), { recursive: true });
  await mkdir(dataDir, { recursive: true, mode: 0o700 });
  const token = 'test-token-1234567890';
  await writeFile(join(dataDir, 'api-token'), `${token}\n`, { mode: 0o600 });

  let runningVersion = '3.2.0';
  let reportedDataDir = dataDir;
  let calls = 0;
  const port = await loopbackServer(t, (req, res) => {
    calls++;
    assert.equal(req.url, '/v1/status');
    assert.equal(req.headers.authorization, `Bearer ${token}`);
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({
      state: 'ready',
      pid: 4242,
      dataDir: reportedDataDir,
      version: { product: 'agent-wechat', backend: 'native', version: runningVersion },
    }));
  });

  await writeFile(
    join(xdgConfig, 'agent-wechat', 'server', 'config.toml'),
    `schemaVersion = 1\nlisten = "127.0.0.1:${port}"\n`,
  );
  const env = { HOME: root, XDG_CONFIG_HOME: xdgConfig, XDG_DATA_HOME: xdgData };

  const outdated = await detectOutdatedDaemon('3.2.1', { env });
  assert.deepEqual(outdated, {
    runningVersion: '3.2.0',
    installedVersion: '3.2.1',
    pid: 4242,
    dataDir,
  });
  assert.match(formatUpgradeNotice(outdated), /v3\.2\.0.*v3\.2\.1.*wx daemon restart/);

  // Same version produces no notice.
  runningVersion = '3.2.1';
  assert.equal(await detectOutdatedDaemon('3.2.1', { env }), null);

  // Different dataDir is ignored.
  runningVersion = '3.1.0';
  reportedDataDir = join(root, 'other-data');
  assert.equal(await detectOutdatedDaemon('3.2.1', { env }), null);
  assert.equal(calls, 3);
});

test('detectOutdatedDaemon uses process.json endpoint when server config omits listen', async t => {
  const root = await tempDir(t);
  const xdgData = join(root, 'data');
  const dataDir = join(xdgData, 'agent-wechat');
  await mkdir(join(dataDir, 'daemon'), { recursive: true, mode: 0o700 });
  const token = 'process-json-token';
  await writeFile(join(dataDir, 'api-token'), token, { mode: 0o600 });

  const port = await loopbackServer(t, (_req, res) => {
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify({
      state: 'ready',
      pid: 5151,
      dataDir,
      version: { product: 'agent-wechat', backend: 'native', version: '3.1.0' },
    }));
  });
  await writeFile(
    join(dataDir, 'daemon', 'process.json'),
    JSON.stringify({ endpoint: `http://127.0.0.1:${port}` }),
    { mode: 0o600 },
  );

  const env = { HOME: root, XDG_CONFIG_HOME: join(root, 'empty-config'), XDG_DATA_HOME: xdgData };
  const outdated = await detectOutdatedDaemon('3.2.1', { env });
  assert.equal(outdated?.runningVersion, '3.1.0');
  assert.equal(outdated?.pid, 5151);
});
