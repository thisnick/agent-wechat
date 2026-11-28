import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile, readlink, realpath, rename, rm, symlink } from 'node:fs/promises';
import { posix, win32 } from 'node:path';

function pathFor(platform) {
  return platform === 'win32' ? win32 : posix;
}

function nonEmptyAbsolute(value, pathLib) {
  return typeof value === 'string' && value.length > 0 && pathLib.isAbsolute(value) ? value : null;
}

export function resolveDataDir({ env = process.env, platform = process.platform } = {}) {
  const pathLib = pathFor(platform);
  const explicit = nonEmptyAbsolute(env.AGENT_WECHAT_DATA_DIR, pathLib);
  if (explicit) return explicit;
  const home = nonEmptyAbsolute(env.HOME, pathLib)
    ?? (platform === 'win32' ? nonEmptyAbsolute(env.USERPROFILE, pathLib) : null);
  const dataHome = nonEmptyAbsolute(env.XDG_DATA_HOME, pathLib)
    ?? (platform === 'win32' ? nonEmptyAbsolute(env.LOCALAPPDATA, pathLib) : null)
    ?? (home ? pathLib.join(home, '.local', 'share') : null);
  return dataHome ? pathLib.join(dataHome, 'agent-wechat') : null;
}

export function resolveServerConfigPath({ env = process.env, platform = process.platform } = {}) {
  const pathLib = pathFor(platform);
  const explicit = nonEmptyAbsolute(env.AGENT_WECHAT_SERVER_CONFIG, pathLib);
  if (explicit) return explicit;
  const home = nonEmptyAbsolute(env.HOME, pathLib)
    ?? (platform === 'win32' ? nonEmptyAbsolute(env.USERPROFILE, pathLib) : null);
  const configHome = nonEmptyAbsolute(env.XDG_CONFIG_HOME, pathLib)
    ?? (platform === 'win32' ? nonEmptyAbsolute(env.APPDATA, pathLib) : null)
    ?? (home ? pathLib.join(home, '.config') : null);
  return configHome ? pathLib.join(configHome, 'agent-wechat', 'server', 'config.toml') : null;
}

export function stableBinaryPath({ dataDir, env = process.env, platform = process.platform } = {}) {
  const pathLib = pathFor(platform);
  const dir = dataDir ?? resolveDataDir({ env, platform });
  if (!dir) return null;
  return pathLib.join(dir, 'bin', platform === 'win32' ? 'wx.exe' : 'wx');
}

async function nearestExistingAncestor(path, pathLib) {
  let current = path;
  while (true) {
    try {
      return { path: current, info: await lstat(current) };
    } catch (error) {
      if (error?.code !== 'ENOENT') throw error;
      const parent = pathLib.dirname(current);
      if (parent === current) return null;
      current = parent;
    }
  }
}

async function ensurePrivateDir(path, pathLib, euid) {
  await mkdir(pathLib.dirname(path), { recursive: true });
  try {
    await mkdir(path, { mode: 0o700 });
  } catch (error) {
    if (error?.code !== 'EEXIST') throw error;
  }
  const info = await lstat(path);
  if (!info.isDirectory() || info.isSymbolicLink()) {
    throw new Error(`Expected a real directory, not a symlink: ${path}`);
  }
  if (typeof euid === 'number' && info.uid !== euid) {
    throw new Error(`Directory is not owned by the current user: ${path}`);
  }
}

export function shouldUpdateStableBinary({ isPostinstall = false, env = process.env } = {}) {
  if (!isPostinstall) return true;
  return env.npm_config_global === 'true';
}

export function versionFromBinaryPath(binaryPath) {
  const match = /[/\\]native[/\\]([^/\\]+)[/\\][^/\\]+[/\\]wx(?:\.exe)?$/.exec(String(binaryPath ?? ''));
  return match && parseSemver(match[1]) ? match[1] : null;
}

export async function updateStableBinary(
  targetBinary,
  { version, dataDir, env = process.env, platform = process.platform, euid = process.geteuid?.() } = {},
) {
  const pathLib = pathFor(platform);
  if (!nonEmptyAbsolute(targetBinary, pathLib)) {
    throw new Error('Stable binary target must be an absolute path');
  }
  if (platform === 'win32') return null;
  // Avoid creating root-owned 0700 files inside a non-root user's HOME when
  // installed via `sudo npm install -g` with preserved HOME.
  if (euid === 0 && env.SUDO_UID && env.SUDO_UID !== '0') {
    return null;
  }
  const dir = dataDir ?? resolveDataDir({ env, platform });
  if (!dir) return null;
  if (typeof euid === 'number') {
    const ancestor = await nearestExistingAncestor(dir, pathLib);
    if (ancestor && ancestor.info.uid !== euid) {
      return null;
    }
  }
  await ensurePrivateDir(dir, pathLib, euid);
  const binDir = pathLib.join(dir, 'bin');
  await ensurePrivateDir(binDir, pathLib, euid);
  const destination = pathLib.join(binDir, 'wx');
  const nextVersion = version ?? versionFromBinaryPath(targetBinary);
  try {
    const existing = await lstat(destination);
    if (existing.isDirectory()) {
      throw new Error(`Stable binary path is a directory: ${destination}`);
    }
    if (existing.isSymbolicLink() && nextVersion) {
      const currentTarget = await readlink(destination);
      const currentVersion = versionFromBinaryPath(currentTarget);
      if (currentVersion && isOlderVersion(nextVersion, currentVersion)) {
        let targetAlive = false;
        try {
          targetAlive = (await lstat(currentTarget)).isFile();
        } catch {}
        if (targetAlive) return null;
      }
    }
  } catch (error) {
    if (error?.code !== 'ENOENT') throw error;
  }
  const temporary = pathLib.join(binDir, `.wx.${process.pid}.${randomUUID()}.tmp`);
  try {
    await symlink(targetBinary, temporary);
    await rename(temporary, destination);
  } finally {
    await rm(temporary, { force: true });
  }
  return destination;
}

function unquoteTomlString(raw) {
  const trimmed = raw.trim();
  if (trimmed.length >= 2 && trimmed.startsWith("'") && trimmed.endsWith("'")) {
    return trimmed.slice(1, -1);
  }
  if (trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try {
      return JSON.parse(trimmed);
    } catch {
      return null;
    }
  }
  return null;
}

function stripTomlComment(line) {
  let quote = null;
  let escaped = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (quote === '"') {
      if (escaped) escaped = false;
      else if (ch === '\\') escaped = true;
      else if (ch === '"') quote = null;
    } else if (quote === "'") {
      if (ch === "'") quote = null;
    } else if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '#') {
      return line.slice(0, i);
    }
  }
  return line;
}

export function parseServerConfig(text, configDir, { platform = process.platform } = {}) {
  const pathLib = pathFor(platform);
  const parsed = {};
  let inRoot = true;
  for (const rawLine of text.split(/\r?\n/)) {
    const line = stripTomlComment(rawLine).trim();
    if (!line) continue;
    if (line.startsWith('[')) {
      inRoot = false;
      continue;
    }
    if (!inRoot) continue;
    const eq = line.indexOf('=');
    if (eq === -1) continue;
    const key = line.slice(0, eq).trim();
    const value = unquoteTomlString(line.slice(eq + 1));
    if (value === null) continue;
    if (key === 'dataDir' || key === 'tokenFile') {
      if (value.startsWith('~') || value.length === 0) continue;
      parsed[key] = pathLib.isAbsolute(value) ? value : pathLib.resolve(configDir, value);
    } else if (key === 'listen') {
      parsed.listen = value;
    }
  }
  return parsed;
}

function parseSemver(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$/.exec(String(version ?? '').trim());
  if (!match) return null;
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease: match[4] ? match[4].split('.') : [],
  };
}

export function compareSemver(a, b) {
  const left = parseSemver(a);
  const right = parseSemver(b);
  if (!left || !right) return null;
  for (const field of ['major', 'minor', 'patch']) {
    if (left[field] !== right[field]) return left[field] < right[field] ? -1 : 1;
  }
  const lp = left.prerelease;
  const rp = right.prerelease;
  if (lp.length === 0 && rp.length === 0) return 0;
  if (lp.length === 0) return 1;
  if (rp.length === 0) return -1;
  const len = Math.max(lp.length, rp.length);
  for (let i = 0; i < len; i++) {
    if (i >= lp.length) return -1;
    if (i >= rp.length) return 1;
    const li = lp[i];
    const ri = rp[i];
    if (li === ri) continue;
    const lNum = /^\d+$/.test(li);
    const rNum = /^\d+$/.test(ri);
    if (lNum && rNum) return Number(li) < Number(ri) ? -1 : 1;
    if (lNum !== rNum) return lNum ? -1 : 1;
    return li < ri ? -1 : 1;
  }
  return 0;
}

export function isOlderVersion(running, installed) {
  return compareSemver(running, installed) === -1;
}

function isLoopbackUrl(url) {
  return url.protocol === 'http:'
    && !url.username
    && !url.password
    && ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname);
}

async function readBoundedRegularFile(path, maxBytes) {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size > maxBytes) return null;
  const bytes = await readFile(path);
  if (bytes.length > maxBytes) return null;
  return bytes;
}

export async function detectOutdatedDaemon(
  installedVersion,
  { env = process.env, platform = process.platform, fetchImpl = fetch, timeoutMs = 1500 } = {},
) {
  if (!parseSemver(installedVersion)) return null;
  const pathLib = pathFor(platform);
  let fileConfig = {};
  const configPath = resolveServerConfigPath({ env, platform });
  if (configPath) {
    try {
      const bytes = await readBoundedRegularFile(configPath, 64 * 1024);
      if (bytes) {
        fileConfig = parseServerConfig(bytes.toString('utf8'), pathLib.dirname(configPath), { platform });
      }
    } catch {}
  }
  const dataDir = nonEmptyAbsolute(env.AGENT_WECHAT_DATA_DIR, pathLib)
    ?? fileConfig.dataDir
    ?? resolveDataDir({ env, platform });
  if (!dataDir) return null;

  const tokenFile = fileConfig.tokenFile ?? pathLib.join(dataDir, 'api-token');
  let token;
  try {
    const bytes = await readBoundedRegularFile(tokenFile, 4096);
    token = bytes?.toString('utf8').trim();
  } catch {
    return null;
  }
  if (!token) return null;

  let endpoint = `http://${fileConfig.listen ?? '127.0.0.1:6174'}`;
  if (!fileConfig.listen) {
    try {
      const recordBytes = await readBoundedRegularFile(pathLib.join(dataDir, 'daemon', 'process.json'), 16 * 1024);
      if (recordBytes) {
        const record = JSON.parse(recordBytes.toString('utf8'));
        if (typeof record?.endpoint === 'string') endpoint = record.endpoint;
      }
    } catch {}
  }

  let statusUrl;
  try {
    statusUrl = new URL('/v1/status', endpoint);
  } catch {
    return null;
  }
  if (!isLoopbackUrl(statusUrl)) return null;

  let response;
  try {
    response = await fetchImpl(statusUrl, {
      headers: { Authorization: `Bearer ${token}` },
      redirect: 'manual',
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch {
    return null;
  }
  if (!response?.ok) {
    await response?.body?.cancel?.().catch(() => {});
    return null;
  }
  let payload;
  try {
    const text = await response.text();
    if (text.length > 64 * 1024) return null;
    payload = JSON.parse(text);
  } catch {
    return null;
  }
  const version = payload?.version;
  if (version?.product !== 'agent-wechat' || version?.backend !== 'native' || typeof version?.version !== 'string') {
    return null;
  }
  if (typeof payload.dataDir === 'string') {
    try {
      const [expectedReal, actualReal] = await Promise.all([realpath(dataDir), realpath(payload.dataDir)]);
      if (expectedReal !== actualReal) return null;
    } catch {
      if (payload.dataDir !== dataDir) return null;
    }
  }
  if (!isOlderVersion(version.version, installedVersion)) return null;
  return {
    runningVersion: version.version,
    installedVersion,
    pid: typeof payload.pid === 'number' ? payload.pid : null,
    dataDir: payload.dataDir ?? dataDir,
  };
}

export function formatUpgradeNotice(outdated) {
  return `A local agent-wechat daemon is still running v${outdated.runningVersion} (installed v${outdated.installedVersion}). Run \`wx daemon restart\` to switch to the new version.`;
}
