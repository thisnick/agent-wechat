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
const allowedDownloadHosts = new Set(['github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com']);

export function targetKey(target) { return [target.platform, target.arch, target.libc].filter(Boolean).join('-'); }
export function executableName(target = targetFor()) { return target.platform === 'win32' ? 'wx.exe' : 'wx'; }
export function targetFor({ platform = process.platform, arch = process.arch, glibc = process.report?.getReport().header.glibcVersionRuntime } = {}) {
  const target = supportedTargets.find(item => item.platform === platform && item.arch === arch);
  if (!target) throw new Error(`Unsupported native wx platform: ${platform}-${arch}`);
  if (platform === 'linux') {
    const [major, minor] = String(glibc ?? '').split('.').map(Number);
    if (!glibc || !Number.isFinite(major) || !Number.isFinite(minor) || major < 2 || (major === 2 && minor < 36)) {
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
export async function readManifest(root = packageRoot) {
  const pkg = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
  let data;
  try { data = await readFile(join(root, 'native-release.json')); }
  catch { throw new Error('This CLI package has no verified native release manifest. Build/provision a matching native release; invocation never downloads automatically.'); }
  if (data.length > 65536) throw new Error('Native release manifest is too large');
  const manifest = JSON.parse(data);
  return { version: pkg.version, manifest, asset: validateManifest(manifest, pkg.version) };
}
export async function fetchBytes(url, maxBytes, { fetchImpl = fetch, allowHttpLoopback = false } = {}) {
  let current = new URL(url);
  if (current.protocol === 'https:' && process.env.NODE_TLS_REJECT_UNAUTHORIZED === '0') throw new Error('Native downloads refuse disabled TLS verification');
  const signal = AbortSignal.timeout(30000);
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
export async function inspectBinary(path, version) {
  if (!isAbsolute(path)) throw new Error('Native binary path must be absolute');
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error('Native binary must be a regular file');
  await access(path, constants.X_OK);
  const { stdout } = await exec(path, ['--json', 'version'], { timeout: 5000, maxBuffer: 65536, windowsHide: true });
  const value = JSON.parse(stdout);
  if (value.product !== 'agent-wechat' || value.backend !== 'native' || value.version !== version
    || value.target !== targetFor().rustTarget || value.apiVersion !== 1 || !value.capabilities?.includes('serve')) {
    throw new Error('Executable is not a matching native wx runtime; no container fallback is performed');
  }
  return value;
}
export function installPath(root, version, target = targetFor()) { return join(root, 'native', version, targetKey(target), executableName(target)); }
export async function installNative({ root = packageRoot, artifactDir, fetchOptions, downloadUrl, inspect = inspectBinary } = {}) {
  targetFor();
  const { version, asset } = await readManifest(root);
  if (process.env.NODE_TLS_REJECT_UNAUTHORIZED === '0') throw new Error('Refusing native installation with TLS verification disabled');
  let bytes;
  if (artifactDir) {
    if (!isAbsolute(artifactDir)) throw new Error('Offline artifact directory must be absolute');
    const localManifest = JSON.parse(await readFile(join(artifactDir, manifestName(version)), 'utf8'));
    const localAsset = validateManifest(localManifest, version);
    if (localAsset.sha256 !== asset.sha256 || localAsset.bytes !== asset.bytes || localManifest.sourceCommit !== (await readManifest(root)).manifest.sourceCommit) {
      throw new Error('Offline manifest differs from the manifest bundled in this npm package');
    }
    const source = join(artifactDir, asset.name);
    const info = await lstat(source);
    if (!info.isFile() || info.isSymbolicLink() || info.size !== asset.bytes) throw new Error('Invalid offline native release file');
    bytes = await readFile(source);
  } else {
    bytes = await fetchBytes(downloadUrl ?? releaseUrl(version, asset.name), asset.bytes, fetchOptions);
  }
  verifyBytes(bytes, asset);
  const destination = installPath(root, version);
  await mkdir(dirname(destination), { recursive: true, mode: 0o755 });
  const temporary = `${destination}.${process.pid}.${createHash('sha256').update(randomUUID()).digest('hex').slice(0, 16)}.tmp${process.platform === 'win32' ? '.exe' : ''}`;
  try {
    await writeFile(temporary, bytes, { flag: 'wx', mode: 0o755 });
    await chmod(temporary, 0o755);
    await inspect(temporary, version);
    await rename(temporary, destination);
  } finally { await rm(temporary, { force: true }); }
  return destination;
}
export async function resolveNative({ root = packageRoot, binaryOverride = process.env.AGENT_WECHAT_NATIVE_BINARY, inspect = inspectBinary } = {}) {
  targetFor();
  if (binaryOverride) {
    const { version } = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'));
    if (!isAbsolute(binaryOverride)) throw new Error('AGENT_WECHAT_NATIVE_BINARY must be an absolute executable path');
    const canonical = await realpath(binaryOverride);
    await inspect(canonical, version);
    return canonical;
  }
  const { version, asset } = await readManifest(root);
  const path = installPath(root, version);
  let bytes;
  try { bytes = await readFile(path); }
  catch { throw new Error('Native wx is not installed. Run wx-native-install, or provision the exact release and set AGENT_WECHAT_NATIVE_BINARY to its absolute path. No download or Docker fallback occurs during invocation.'); }
  verifyBytes(bytes, asset);
  await inspect(path, version);
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
