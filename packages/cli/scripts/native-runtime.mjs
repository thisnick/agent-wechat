import { createHash, randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { access, chmod, lstat, mkdir, readFile, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { dirname, isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const exec = promisify(execFile);
export const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));
export const releaseRepository = 'thisnick/agent-wechat';
export const supportedTargets = Object.freeze([
  Object.freeze({ platform: 'linux', arch: 'x64', libc: 'gnu', minimumGlibc: '2.36', rustTarget: 'x86_64-unknown-linux-gnu' }),
  Object.freeze({ platform: 'linux', arch: 'arm64', libc: 'gnu', minimumGlibc: '2.36', rustTarget: 'aarch64-unknown-linux-gnu' }),
  Object.freeze({ platform: 'darwin', arch: 'arm64', rustTarget: 'aarch64-apple-darwin' }),
  Object.freeze({ platform: 'win32', arch: 'x64', rustTarget: 'x86_64-pc-windows-msvc' }),
  Object.freeze({ platform: 'win32', arch: 'arm64', rustTarget: 'aarch64-pc-windows-msvc' }),
]);
export const supportedTarget = supportedTargets[0];
const maxBinaryBytes = 256 * 1024 * 1024;
const stampSchemaVersion = 1;
const maxStampBytes = 65536;
const allowedDownloadHosts = new Set(['github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com']);

export function targetKey(target) { return [target.platform, target.arch, target.libc].filter(Boolean).join('-'); }
export function executableName(target = targetFor()) { return target.platform === 'win32' ? 'wx.exe' : 'wx'; }
export function targetFor({ platform = process.platform, arch = process.arch, glibc } = {}) {
  const target = supportedTargets.find(item => item.platform === platform && item.arch === arch);
  if (!target) throw new Error(`Unsupported native wx platform: ${platform}-${arch}`);
  if (platform === 'linux') {
    const runtimeGlibc = glibc === undefined ? process.report?.getReport().header.glibcVersionRuntime : glibc;
    const [major, minor] = String(runtimeGlibc ?? '').split('.').map(Number);
    if (!runtimeGlibc || !Number.isFinite(major) || !Number.isFinite(minor) || major < 2 || (major === 2 && minor < 36)) {
      throw new Error('Native wx on Linux requires glibc 2.36 or newer; musl is unsupported.');
    }
  }
  return target;
}
export function assetName(version, target = targetFor()) { return `wx-v${version}-${targetKey(target)}${target.platform === 'win32' ? '.exe' : ''}`; }
export function manifestName(version) { return `wx-v${version}-checksums.json`; }
export function releaseUrl(version, name) {
  return `https://github.com/${releaseRepository}/releases/download/v${version}/${name}`;
}
export function validateManifest(manifest, version, { target = targetFor(), requireAllTargets = false } = {}) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version) || manifest?.schemaVersion !== 1
    || manifest.version !== version || manifest.repository !== releaseRepository
    || !/^[a-f0-9]{40}$/.test(manifest.sourceCommit ?? '') || typeof manifest.sourceTreeDirty !== 'boolean' || !Array.isArray(manifest.assets) || manifest.assets.length < 1 || manifest.assets.length > supportedTargets.length) {
    throw new Error('Invalid or mismatched native release manifest');
  }
  const seen = new Set();
  for (const asset of manifest.assets) {
    const expected = supportedTargets.find(item => targetKey(item) === targetKey(asset));
    if (!expected || seen.has(targetKey(asset)) || Object.entries(expected).some(([key, value]) => asset[key] !== value)
      || asset.name !== assetName(version, expected) || !/^[a-f0-9]{64}$/.test(asset.sha256 ?? '')
      || !Number.isSafeInteger(asset.bytes) || asset.bytes < 1 || asset.bytes > maxBinaryBytes) {
      throw new Error('Invalid native release asset, duplicate or unsupported target');
    }
    seen.add(targetKey(asset));
  }
  if (requireAllTargets && seen.size !== supportedTargets.length) throw new Error('Native publication requires every supported target');
  const asset = manifest.assets.find(item => targetKey(item) === targetKey(target));
  if (!asset) throw new Error(`Native manifest has no asset for ${targetKey(target)}`);
  return asset;
}
export async function readManifest(root = packageRoot, { target = targetFor() } = {}) {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  let data;
  try { data = await readFile(join(root, 'native-release.json')); }
  catch { throw new Error('This CLI package has no verified native release manifest. Build/provision a matching native release; invocation never downloads automatically.'); }
  if (data.length > 65536) throw new Error('Native release manifest is too large');
  const manifest = JSON.parse(data);
  return { version: pkg.version, manifest, asset: validateManifest(manifest, pkg.version, { target }) };
}
export async function fetchBytes(url, maxBytes, { fetchImpl = fetch, allowHttpLoopback = false, timeoutMs = 30_000 } = {}) {
  let current = new URL(url);
  if (current.protocol === 'https:' && process.env.NODE_TLS_REJECT_UNAUTHORIZED === '0') throw new Error('Native downloads refuse disabled TLS verification');
  const signal = AbortSignal.timeout(timeoutMs);
  for (let redirects = 0; redirects <= 3; redirects++) {
    const testLocal = allowHttpLoopback && current.protocol === 'http:' && ['127.0.0.1', '[::1]'].includes(current.hostname);
    if (current.username || current.password || current.hash || (!testLocal && (current.protocol !== 'https:' || !allowedDownloadHosts.has(current.hostname) || current.port))) {
      throw new Error('Native downloads require an approved HTTPS release host');
    }
    const response = await fetchImpl(current, { redirect: 'manual', signal });
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      await response.body?.cancel();
      const location = response.headers.get('location');
      if (!location || redirects === 3) throw new Error('Invalid or excessive native release redirects');
      current = new URL(location, current);
      continue;
    }
    if (!response.ok) { await response.body?.cancel(); throw new Error(`Native release download failed (HTTP ${response.status})`); }
    const declared = response.headers.get('content-length');
    if (declared && (!/^\d+$/.test(declared) || Number(declared) > maxBytes)) {
      await response.body?.cancel(); throw new Error('Native release asset exceeds its size bound');
    }
    const chunks = [];
    let total = 0;
    if (!response.body) throw new Error('Native release asset has no body');
    for await (const chunk of response.body) {
      total += chunk.byteLength;
      if (total > maxBytes) throw new Error('Native release asset exceeds its size bound');
      chunks.push(chunk);
    }
    return Buffer.concat(chunks);
  }
  throw new Error('Native release redirect failed');
}
export function verifyBytes(bytes, asset) {
  if (bytes.length !== asset.bytes || createHash('sha256').update(bytes).digest('hex') !== asset.sha256) {
    throw new Error('Native binary size or SHA-256 does not match the manifest bundled in this npm package');
  }
}
function isValidIdentity(value, version, target) {
  return Boolean(
    value && typeof value === 'object' && !Array.isArray(value)
    && value.product === 'agent-wechat'
    && value.backend === 'native'
    && value.version === version
    && value.target === target.rustTarget
    && value.apiVersion === 1
    && Array.isArray(value.capabilities)
    && value.capabilities.every(item => typeof item === 'string')
    && value.capabilities.includes('serve'),
  );
}
export async function inspectBinary(path, version, { target = targetFor() } = {}) {
  if (!isAbsolute(path)) throw new Error('Native binary path must be absolute');
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error('Native binary must be a regular file');
  await access(path, constants.X_OK);
  const { stdout } = await exec(path, ['--json', 'version'], { timeout: 5000, maxBuffer: 65536, windowsHide: true });
  const value = JSON.parse(stdout);
  if (!isValidIdentity(value, version, target)) {
    throw new Error('Executable is not a matching native wx runtime; no container fallback is performed');
  }
  return value;
}
export function installPath(root, version, target = targetFor()) { return join(root, 'native', version, targetKey(target), executableName(target)); }
export function stampPath(binaryPath) { return `${binaryPath}.verified.json`; }
export function overrideStampPath(root, version, target = targetFor()) { return join(root, 'native', version, targetKey(target), 'override.verified.json'); }
function statSnapshot(info) {
  return {
    size: String(info.size),
    mtimeNs: String(info.mtimeNs),
    ctimeNs: String(info.ctimeNs),
    ino: String(info.ino),
    dev: String(info.dev),
  };
}
function statSnapshotsEqual(left, right) {
  return left.size === right.size
    && left.mtimeNs === right.mtimeNs
    && left.ctimeNs === right.ctimeNs
    && left.ino === right.ino
    && left.dev === right.dev;
}
function statSnapshotMatches(recorded, current) {
  if (!recorded || typeof recorded !== 'object' || Array.isArray(recorded)) return false;
  for (const key of ['size', 'ino', 'dev']) {
    if (typeof recorded[key] !== 'string' || !/^\d+$/.test(recorded[key])) return false;
  }
  for (const key of ['mtimeNs', 'ctimeNs']) {
    if (typeof recorded[key] !== 'string' || !/^-?\d+$/.test(recorded[key])) return false;
  }
  return statSnapshotsEqual(recorded, statSnapshot(current));
}
function buildStamp({ mode, path, version, target, sha256, info, identity }) {
  return {
    schemaVersion: stampSchemaVersion,
    mode,
    path,
    version,
    target: target.rustTarget,
    sha256,
    stat: statSnapshot(info),
    identity,
  };
}
async function readValidStamp(file, expected) {
  try {
    const stampInfo = await lstat(file);
    if (!stampInfo.isFile() || stampInfo.isSymbolicLink() || stampInfo.size < 2 || stampInfo.size > maxStampBytes) return false;
    const raw = await readFile(file, 'utf8');
    if (Buffer.byteLength(raw) > maxStampBytes) return false;
    const stamp = JSON.parse(raw);
    if (!stamp || typeof stamp !== 'object' || Array.isArray(stamp)) return false;
    if (stamp.schemaVersion !== stampSchemaVersion || stamp.mode !== expected.mode
      || stamp.path !== expected.path || stamp.version !== expected.version || stamp.target !== expected.target.rustTarget) {
      return false;
    }
    if (expected.mode === 'installed') {
      if (typeof stamp.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(stamp.sha256) || stamp.sha256 !== expected.sha256) return false;
      if (expected.info.size !== BigInt(expected.bytes)) return false;
    } else if (expected.mode === 'override') {
      if (stamp.sha256 !== null) return false;
    } else {
      return false;
    }
    if (!statSnapshotMatches(stamp.stat, expected.info)) return false;
    return isValidIdentity(stamp.identity, expected.version, expected.target);
  } catch {
    return false;
  }
}
async function writeStamp(file, stamp) {
  await mkdir(dirname(file), { recursive: true, mode: 0o755 });
  const temporary = `${file}.${process.pid}.${createHash('sha256').update(randomUUID()).digest('hex').slice(0, 16)}.tmp`;
  try {
    await writeFile(temporary, `${JSON.stringify(stamp, null, 2)}\n`, { flag: 'wx', mode: 0o644 });
    await chmod(temporary, 0o644);
    await rename(temporary, file);
  } finally {
    await rm(temporary, { force: true });
  }
}
async function tryWriteStamp(file, stamp) {
  try { await writeStamp(file, stamp); } catch {}
}
export async function installNative({ root = packageRoot, artifactDir, fetchOptions, downloadUrl, inspect = inspectBinary } = {}) {
  const target = targetFor();
  const { version, manifest, asset } = await readManifest(root, { target });
  if (process.env.NODE_TLS_REJECT_UNAUTHORIZED === '0') throw new Error('Refusing native installation with TLS verification disabled');
  let bytes;
  if (artifactDir) {
    if (!isAbsolute(artifactDir)) throw new Error('Offline artifact directory must be absolute');
    const localManifest = JSON.parse(await readFile(join(artifactDir, manifestName(version)), 'utf8'));
    const localAsset = validateManifest(localManifest, version, { target });
    if (localAsset.sha256 !== asset.sha256 || localAsset.bytes !== asset.bytes || localManifest.sourceCommit !== manifest.sourceCommit) {
      throw new Error('Offline manifest differs from the manifest bundled in this npm package');
    }
    const source = join(artifactDir, asset.name);
    const info = await lstat(source);
    if (!info.isFile() || info.isSymbolicLink() || info.size !== asset.bytes) throw new Error('Invalid offline native release file');
    bytes = await readFile(source);
  } else {
    bytes = await fetchBytes(downloadUrl ?? releaseUrl(version, asset.name), asset.bytes, {
      ...fetchOptions, timeoutMs: 300_000,
    });
  }
  verifyBytes(bytes, asset);
  const destination = installPath(root, version, target);
  await mkdir(dirname(destination), { recursive: true, mode: 0o755 });
  const temporary = `${destination}.${process.pid}.${createHash('sha256').update(randomUUID()).digest('hex').slice(0, 16)}.tmp${process.platform === 'win32' ? '.exe' : ''}`;
  let identity;
  let beforeRename;
  try {
    await writeFile(temporary, bytes, { flag: 'wx', mode: 0o755 });
    await chmod(temporary, 0o755);
    identity = await (inspect === inspectBinary ? inspectBinary(temporary, version, { target }) : inspect(temporary, version));
    beforeRename = await lstat(temporary, { bigint: true });
    await rename(temporary, destination);
  } finally { await rm(temporary, { force: true }); }
  const stampFile = stampPath(destination);
  const afterRename = await lstat(destination, { bigint: true });
  if (afterRename.isFile() && !afterRename.isSymbolicLink()
    && afterRename.ino === beforeRename.ino && afterRename.dev === beforeRename.dev
    && afterRename.size === BigInt(asset.bytes) && afterRename.mtimeNs === beforeRename.mtimeNs
    && isValidIdentity(identity, version, target)) {
    await writeStamp(stampFile, buildStamp({
      mode: 'installed', path: destination, version, target, sha256: asset.sha256, info: afterRename, identity,
    }));
  } else {
    await rm(stampFile, { force: true });
  }
  return destination;
}
export async function resolveNative({
  root = packageRoot,
  binaryOverride = process.env.AGENT_WECHAT_NATIVE_BINARY,
  inspect = inspectBinary,
  verify = verifyBytes,
} = {}) {
  const target = targetFor();
  if (binaryOverride) {
    const { version } = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
    if (!isAbsolute(binaryOverride)) throw new Error('AGENT_WECHAT_NATIVE_BINARY must be an absolute executable path');
    const canonical = await realpath(binaryOverride);
    const before = await lstat(canonical, { bigint: true });
    if (!before.isFile() || before.isSymbolicLink()) throw new Error('Native binary must be a regular file');
    // Keep the override stamp inside the package's own native/ folder rather than
    // writing next to arbitrary operator paths (such as /usr/local/bin or /tmp).
    const stampFile = overrideStampPath(root, version, target);
    if (await readValidStamp(stampFile, { mode: 'override', path: canonical, version, target, sha256: null, info: before })) {
      await access(canonical, constants.X_OK);
      return canonical;
    }
    const identity = await (inspect === inspectBinary ? inspectBinary(canonical, version, { target }) : inspect(canonical, version));
    try {
      const after = await lstat(canonical, { bigint: true });
      if (after.isFile() && !after.isSymbolicLink() && statSnapshotsEqual(statSnapshot(before), statSnapshot(after))
        && isValidIdentity(identity, version, target)) {
        await tryWriteStamp(stampFile, buildStamp({
          mode: 'override', path: canonical, version, target, sha256: null, info: after, identity,
        }));
      }
    } catch {}
    return canonical;
  }
  const { version, asset } = await readManifest(root, { target });
  const path = installPath(root, version, target);
  const notInstalledError = () => new Error('Native wx is not installed. Run wx-native-install, or provision the exact release and set AGENT_WECHAT_NATIVE_BINARY to its absolute path. No download or Docker fallback occurs during invocation.');
  let before;
  try { before = await lstat(path, { bigint: true }); }
  catch (error) {
    if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') throw notInstalledError();
    throw error;
  }
  if (!before.isFile() || before.isSymbolicLink()) throw new Error('Native binary must be a regular file');
  const stampFile = stampPath(path);
  if (await readValidStamp(stampFile, { mode: 'installed', path, version, target, sha256: asset.sha256, bytes: asset.bytes, info: before })) {
    await access(path, constants.X_OK);
    return path;
  }
  let bytes;
  try { bytes = await readFile(path); }
  catch (error) {
    if (error?.code === 'ENOENT' || error?.code === 'ENOTDIR') throw notInstalledError();
    throw error;
  }
  verify(bytes, asset);
  const identity = await (inspect === inspectBinary ? inspectBinary(path, version, { target }) : inspect(path, version));
  try {
    const after = await lstat(path, { bigint: true });
    if (after.isFile() && !after.isSymbolicLink() && statSnapshotsEqual(statSnapshot(before), statSnapshot(after))
      && after.size === BigInt(asset.bytes) && isValidIdentity(identity, version, target)) {
      await tryWriteStamp(stampFile, buildStamp({
        mode: 'installed', path, version, target, sha256: asset.sha256, info: after, identity,
      }));
    }
  } catch {}
  return path;
}
export function runNative(path, args, { spawnImpl = spawn, processObject = process } = {}) {
  const child = spawnImpl(path, args, { stdio: 'inherit', shell: false, windowsHide: true });
  const handlers = new Map();
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    const handler = () => {
      // Windows broadcasts console Ctrl+C to both processes; Node's kill(SIGINT)
      // would terminate the child before Rust can finish graceful shutdown.
      if (!child.killed && !(process.platform === 'win32' && signal === 'SIGINT')) child.kill(signal);
    };
    handlers.set(signal, handler);
    processObject.on(signal, handler);
  }
  const cleanup = () => { for (const [signal, handler] of handlers) processObject.off(signal, handler); };
  child.once('error', () => { cleanup(); processObject.exitCode = 1; console.error('Unable to launch native wx'); });
  child.once('exit', (code, signal) => {
    cleanup();
    if (signal) processObject.kill(processObject.pid, signal);
    else processObject.exitCode = code ?? 1;
  });
  return child;
}
