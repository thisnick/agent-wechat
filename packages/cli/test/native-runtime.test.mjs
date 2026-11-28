import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm, chmod, copyFile, lstat, realpath, rename, symlink, utimes } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { assetName, fetchBytes, inspectBinary, installNative, installPath, manifestName, overrideStampPath, readManifest,
  resolveNative, stampPath, targetKey, supportedTargets, targetFor, validateManifest, verifyBytes } from '../scripts/native-runtime.mjs';
import { prepareNativePackage, nativePackageUnpublished } from '../../../scripts/release/prepare-package.mjs';
const exec = promisify(execFile);
const version = '0.16.0';
const supportedTarget = targetFor();
const identity = { product: 'agent-wechat', backend: 'native', version, apiVersion: 1, target: supportedTarget.rustTarget, capabilities: ['serve'] };
const code = `#!/bin/sh\nexec node - "$@" <<'NATIVE_FIXTURE'\nif(process.argv[2]==='--json')console.log(${JSON.stringify(JSON.stringify(identity))});else if(process.argv[2]==='wait'){process.on('SIGTERM',()=>process.exit(23));setInterval(()=>{},1000);console.log('ready')}else {console.log(JSON.stringify(process.argv.slice(2)));process.exit(7)}\nNATIVE_FIXTURE\n`;
async function fixture(t) {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'wx-native-package-')));
  t.after(() => rm(root, { recursive: true, force: true }));
  const artifacts = join(root, 'artifacts');
  await mkdir(artifacts);
  const bytes = Buffer.from(code);
  const asset = { name: assetName(version), ...supportedTarget, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
  const manifest = { schemaVersion: 1, version, repository: 'thisnick/agent-wechat', sourceCommit: 'a'.repeat(40), sourceTreeDirty: false, assets: [asset, ...supportedTargets.filter(target => targetKey(target) !== targetKey(supportedTarget)).map(target => ({ bytes: asset.bytes, sha256: asset.sha256, ...target, name: assetName(version, target) }))] };
  await writeFile(join(root, 'package.json'), JSON.stringify({ version, type: 'module' }));
  await writeFile(join(root, 'native-release.json'), JSON.stringify(manifest));
  await writeFile(join(artifacts, manifestName(version)), JSON.stringify(manifest));
  await writeFile(join(artifacts, asset.name), bytes, { mode: 0o755 });
  return { root, artifacts, bytes, manifest, asset };
}
async function server(t, handler) {
  const http = createServer(handler);
  http.listen(0, '127.0.0.1'); await once(http, 'listening');
  t.after(() => { http.closeAllConnections(); return new Promise(done => http.close(done)); });
  return `http://127.0.0.1:${http.address().port}`;
}
async function copyLauncher(root) {
  await mkdir(join(root, 'scripts'));
  for (const name of ['native-launcher.mjs', 'native-runtime.mjs', 'native-install.mjs', 'native-path.mjs', 'stable-binary.mjs']) {
    await copyFile(new URL(`../scripts/${name}`, import.meta.url), join(root, 'scripts', name));
  }
  return join(root, 'scripts/native-launcher.mjs');
}

test('platform matrix keeps OS and architecture distinct and only checks glibc on Linux', () => {
  for (const target of supportedTargets) assert.deepEqual(targetFor({ ...target, glibc: target.platform === 'linux' ? '2.36' : null }), target);
  for (const glibc of [null, '2', '2.35']) assert.throws(() => targetFor({ platform: 'linux', arch: 'x64', glibc }), /glibc/);
  assert.throws(() => targetFor({ platform: 'freebsd', arch: 'x64' }), /Unsupported/);
  assert.throws(() => targetFor({ platform: 'darwin', arch: 'x64' }), /Unsupported/);
  const windows = targetFor({ platform: 'win32', arch: 'arm64' });
  assert.match(assetName(version, windows), /win32-arm64\.exe$/);
  assert.match(installPath('/fixture', version, windows), /wx\.exe$/);
});
test('manifest pins exact version, source commit, target, SHA-256 and bounded size', async t => {
  const { root, manifest } = await fixture(t);
  assert.equal((await readManifest(root)).version, version);
  for (const mutation of [m => m.version = 'latest', m => m.sourceCommit = 'main', m => m.assets[0].rustTarget = 'incorrect-target', m => m.assets[0].name = '../wx', m => m.assets[0].sha256 = 'short', m => m.assets[0].bytes = 2 ** 32, m => m.assets.push(m.assets[0])]) {
    const value = structuredClone(manifest); mutation(value);
    assert.throws(() => validateManifest(value, version), /Invalid/);
  }
});
test('offline exact artifact installs atomically and validates native identity', async t => {
  const { root, artifacts, bytes } = await fixture(t);
  const destination = await installNative({ root, artifactDir: artifacts });
  assert.equal(destination, installPath(root, version));
  assert.deepEqual(await readFile(destination), bytes);
  assert.equal(await resolveNative({ root }), destination);
});
test('changed binary is rejected without replacing an already installed runtime', async t => {
  const { root, artifacts, asset, bytes } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });
  await writeFile(join(artifacts, asset.name), Buffer.alloc(asset.bytes, 1));
  await assert.rejects(installNative({ root, artifactDir: artifacts }), /SHA-256/);
  assert.deepEqual(await readFile(path), bytes);
});
test('invalid executable identity is rejected before atomic publication', async t => {
  const { root, artifacts } = await fixture(t);
  await assert.rejects(installNative({ root, artifactDir: artifacts, inspect: async () => { throw new Error('wrong identity'); } }), /wrong identity/);
  await assert.rejects(readFile(installPath(root, version)), /ENOENT/);
});
test('offline manifest cannot change bundled hashes and cannot use a relative directory', async t => {
  const { root, artifacts, manifest } = await fixture(t);
  manifest.assets[0].sha256 = 'b'.repeat(64);
  await writeFile(join(artifacts, manifestName(version)), JSON.stringify(manifest));
  await assert.rejects(installNative({ root, artifactDir: artifacts }), /differs/);
  await assert.rejects(installNative({ root, artifactDir: 'relative' }), /absolute/);
});
test('invocation has no download or legacy container fallback', async t => {
  const { root } = await fixture(t);
  const launcher = await copyLauncher(root);
  const { stderr } = await exec(process.execPath, [launcher, '--json', 'version']).catch(e => e);
  assert.match(stderr, /No download or Docker fallback/);
});
test('installed binary is rechecked and corruption is never executed', async t => {
  const { root, artifacts } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });
  await writeFile(path, 'corrupt');
  await assert.rejects(resolveNative({ root }), /SHA-256/);
});
test('explicitly provisioned executable requires absolute path and correct native version', async t => {
  const { root, artifacts, asset } = await fixture(t);
  await assert.rejects(resolveNative({ root, binaryOverride: 'wx' }), /absolute/);
  const path = join(artifacts, asset.name);
  assert.equal(await resolveNative({ root, binaryOverride: path }), path);
  await writeFile(path, `#!/bin/sh\nprintf '{"product":"agent-wechat","backend":"container"}\\n'\n`);
  await assert.rejects(inspectBinary(path, version), /matching native/);
});
test('launcher forwards literal arguments, standard output and nonzero exit code', async t => {
  const { root, artifacts } = await fixture(t);
  await installNative({ root, artifactDir: artifacts });
  const launcher = await copyLauncher(root);
  const result = await exec(process.execPath, [launcher, 'echo', 'a b', '$(touch nope)', '--flag']).catch(e => e);
  assert.equal(result.code, 7);
  assert.deepEqual(JSON.parse(result.stdout), ['echo', 'a b', '$(touch nope)', '--flag']);
});
test('launcher forwards SIGTERM to a managed foreground runtime', { timeout: 10000 }, async t => {
  const { root, artifacts } = await fixture(t);
  await installNative({ root, artifactDir: artifacts });
  const launcher = await copyLauncher(root);
  const child = spawn(process.execPath, [launcher, 'wait'], { stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => { if (child.exitCode === null) child.kill('SIGKILL'); });
  await once(child.stdout, 'data');
  const exit = once(child, 'exit');
  child.kill('SIGTERM');
  const [code, signal] = await exit;
  assert.equal(code, 23); assert.equal(signal, null);
});
test('installer fetches pinned bytes from a local HTTP fixture and validates before install', async t => {
  const { root, bytes } = await fixture(t);
  const url = await server(t, (_req, res) => { res.end(bytes); });
  await installNative({ root, downloadUrl: url, fetchOptions: { allowHttpLoopback: true } });
  assert.equal(await resolveNative({ root }), installPath(root, version));
});
test('HTTP is rejected in production and redirect hosts cannot escape the allowlist', async t => {
  await assert.rejects(fetchBytes('http://127.0.0.1/native', 10), /approved HTTPS/);
  await assert.rejects(fetchBytes('https://evil.example/native', 10), /approved HTTPS/);
  await assert.rejects(fetchBytes('https://github.com/native', 10, { fetchImpl: async () => new Response(null, { status: 302, headers: { location: 'https://evil.example/native' } }) }), /approved HTTPS/);
  await assert.rejects(fetchBytes('https://github.com/native', 10, { fetchImpl: async () => new Response(null, { status: 302, headers: { location: 'https://github.com/native' } }) }), /excessive/);
});
test('download bounds declared and streamed sizes and rejects missing release assets', async t => {
  const url = await server(t, (req, res) => {
    if (req.url === '/large') { res.setHeader('Content-Length', '999'); res.end('x'); }
    else if (req.url === '/stream') { res.write('012345'); res.end('678901'); }
    else { res.statusCode = 404; res.end('missing'); }
  });
  for (const path of ['/large', '/stream']) await assert.rejects(fetchBytes(`${url}${path}`, 10, { allowHttpLoopback: true }), /size bound/);
  await assert.rejects(fetchBytes(`${url}/404`, 10, { allowHttpLoopback: true }), /HTTP 404/);
});
test('package publication requires prepared release from the exact source commit', async t => {
  const { root, manifest, bytes } = await fixture(t);
  const fetchImpl = async url => String(url).endsWith('.json') ? new Response(JSON.stringify(manifest)) : new Response(bytes);
  await prepareNativePackage({ root, sourceCommit: manifest.sourceCommit, fetchOptions: { fetchImpl } });
  await assert.rejects(prepareNativePackage({ root, sourceCommit: 'c'.repeat(40), fetchOptions: { fetchImpl } }), /publication source commit/);
});

test('ordinary pushes skip native preparation after exact CLI version is published', async t => {
  const { root } = await fixture(t);
  await writeFile(join(root, 'package.json'), JSON.stringify({ name: '@agent-wechat/cli', version }));
  assert.equal(await nativePackageUnpublished({ root, fetchImpl: async () => ({ status: 200, json: async () => ({ name: '@agent-wechat/cli', version }) }) }), false);
  assert.equal(await nativePackageUnpublished({ root, fetchImpl: async () => ({ status: 404 }) }), true);
  await assert.rejects(nativePackageUnpublished({ root, fetchImpl: async () => ({ status: 503 }) }), /HTTP 503/);
});

test('publication requires every target; an ARM manifest selects only the ARM bytes', async t => {
  const { manifest } = await fixture(t);
  const arm = supportedTargets.find(target => target.arch === 'arm64');
  const x64 = supportedTargets.find(target => target.arch === 'x64');
  assert.equal(validateManifest(manifest, version, { target: arm, requireAllTargets: true }).rustTarget, arm.rustTarget);
  assert.notEqual(assetName(version, arm), assetName(version, x64));
  assert.notEqual(installPath('/fixture', version, arm), installPath('/fixture', version, x64));
  const fragment = { ...manifest, assets: [manifest.assets.find(asset => asset.arch === 'arm64')] };
  assert.throws(() => validateManifest(fragment, version, { target: arm, requireAllTargets: true }), /every supported target/);
  assert.throws(() => validateManifest(fragment, version, { target: x64 }), /no asset for linux-x64/);
});

test('install writes verification stamp and matching launch skips hashing and identity exec', async t => {
  const { root, artifacts, asset } = await fixture(t);
  const destination = await installNative({ root, artifactDir: artifacts });
  const stampFile = stampPath(destination);
  const stamp = JSON.parse(await readFile(stampFile, 'utf8'));
  const info = await lstat(destination, { bigint: true });
  assert.equal(stamp.schemaVersion, 1);
  assert.equal(stamp.mode, 'installed');
  assert.equal(stamp.path, destination);
  assert.equal(stamp.version, version);
  assert.equal(stamp.target, supportedTarget.rustTarget);
  assert.equal(stamp.sha256, asset.sha256);
  assert.deepEqual(stamp.stat, {
    size: String(info.size),
    mtimeNs: String(info.mtimeNs),
    ctimeNs: String(info.ctimeNs),
    ino: String(info.ino),
    dev: String(info.dev),
  });
  assert.deepEqual(stamp.identity, identity);

  let verifyCalls = 0;
  let inspectCalls = 0;
  const resolved = await resolveNative({
    root,
    verify: (bytes, expected) => { verifyCalls++; return verifyBytes(bytes, expected); },
    inspect: async (p, v) => { inspectCalls++; return inspectBinary(p, v); },
  });
  assert.equal(resolved, destination);
  assert.equal(verifyCalls, 0);
  assert.equal(inspectCalls, 0);
});

test('stamp mismatch after size, same-size content, mtime, ctime, inode or device change re-verifies', async t => {
  const { root, artifacts, bytes } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });
  const stampFile = stampPath(path);

  let verifyCalls = 0;
  let inspectCalls = 0;
  const spies = {
    verify: (b, expected) => { verifyCalls++; return verifyBytes(b, expected); },
    inspect: async (p, v) => { inspectCalls++; return inspectBinary(p, v); },
  };

  // mtime change on a valid binary forces full verification and refreshes the stamp.
  const origStat = await lstat(path, { bigint: true });
  const shiftedMtime = new Date(Number(origStat.mtimeNs / 1_000_000n) - 5000);
  await utimes(path, new Date(Number(origStat.atimeNs / 1_000_000n)), shiftedMtime);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 1);
  assert.equal(inspectCalls, 1);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 1);
  assert.equal(inspectCalls, 1);

  // ctime change (e.g. chmod back and forth after a clock tick) forces full verification.
  await new Promise(done => setTimeout(done, 15));
  await chmod(path, 0o700);
  await chmod(path, 0o755);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 2);
  assert.equal(inspectCalls, 2);

  // inode change with identical bytes and preserved mtime forces full verification.
  const beforeReplace = await lstat(path, { bigint: true });
  const replacement = `${path}.new-inode`;
  await writeFile(replacement, bytes, { mode: 0o755 });
  await chmod(replacement, 0o755);
  await utimes(replacement, new Date(Number(beforeReplace.atimeNs / 1_000_000n)), new Date(Number(beforeReplace.mtimeNs / 1_000_000n)));
  await rename(replacement, path);
  const afterReplace = await lstat(path, { bigint: true });
  assert.notEqual(afterReplace.ino, beforeReplace.ino);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 3);
  assert.equal(inspectCalls, 3);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 3);
  assert.equal(inspectCalls, 3);

  // device mismatch in stamp forces full verification and refreshes the stamp.
  const devMismatch = JSON.parse(await readFile(stampFile, 'utf8'));
  devMismatch.stat.dev = String(BigInt(devMismatch.stat.dev) + 1n);
  await writeFile(stampFile, JSON.stringify(devMismatch));
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 4);
  assert.equal(inspectCalls, 4);

  // size mismatch in stamp alone forces full verification and refreshes the stamp.
  const sizeMismatch = JSON.parse(await readFile(stampFile, 'utf8'));
  sizeMismatch.stat.size = String(BigInt(sizeMismatch.stat.size) + 1n);
  await writeFile(stampFile, JSON.stringify(sizeMismatch));
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 5);
  assert.equal(inspectCalls, 5);

  // Size change on disk against a matching stamp is rejected without executing the binary.
  await writeFile(path, Buffer.concat([bytes, Buffer.from('\n')]));
  await assert.rejects(resolveNative({ root, ...spies }), /SHA-256/);
  assert.equal(verifyCalls, 6);
  assert.equal(inspectCalls, 5);

  // Restore valid binary & matching stamp before testing same-size corruption with restored mtime.
  await writeFile(path, bytes);
  assert.equal(await resolveNative({ root, ...spies }), path);
  assert.equal(verifyCalls, 7);
  assert.equal(inspectCalls, 6);

  // Same-size corruption with restored mtime still misses via ctime and is rejected before execution.
  const preCorrupt = await lstat(path, { bigint: true });
  const mutated = Buffer.from(bytes);
  mutated[mutated.length - 1] = mutated[mutated.length - 1] === 0x0a ? 0x20 : 0x0a;
  assert.equal(mutated.length, bytes.length);
  await new Promise(done => setTimeout(done, 15));
  await writeFile(path, mutated);
  await utimes(path, new Date(Number(preCorrupt.atimeNs / 1_000_000n)), new Date(Number(preCorrupt.mtimeNs / 1_000_000n)));
  await assert.rejects(resolveNative({ root, ...spies }), /SHA-256/);
  assert.equal(verifyCalls, 8);
  assert.equal(inspectCalls, 6);
});

test('missing, corrupted, invalid or symlink stamp falls back to full verification', async t => {
  const { root, artifacts, manifest } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });
  const stampFile = stampPath(path);
  const validStamp = JSON.parse(await readFile(stampFile, 'utf8'));

  const mutations = [
    async () => rm(stampFile, { force: true }),
    async () => writeFile(stampFile, '{truncated'),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, schemaVersion: 99 })),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, mode: 'override' })),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, sha256: '0'.repeat(64) })),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, path: `${path}.other` })),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, stat: { ...validStamp.stat, size: Number(validStamp.stat.size) } })),
    async () => writeFile(stampFile, JSON.stringify({ ...validStamp, identity: { ...validStamp.identity, capabilities: [] } })),
    async () => {
      const target = join(root, 'external-stamp.json');
      await writeFile(target, JSON.stringify(validStamp));
      await rm(stampFile, { force: true });
      await symlink(target, stampFile);
    },
  ];

  for (const mutate of mutations) {
    await mutate();
    let verifyCalls = 0;
    let inspectCalls = 0;
    assert.equal(await resolveNative({
      root,
      verify: (b, expected) => { verifyCalls++; return verifyBytes(b, expected); },
      inspect: async (p, v) => { inspectCalls++; return inspectBinary(p, v); },
    }), path);
    assert.equal(verifyCalls, 1);
    assert.equal(inspectCalls, 1);
    const repaired = await lstat(stampFile);
    assert.equal(repaired.isFile(), true);
    assert.equal(repaired.isSymbolicLink(), false);
  }

  // Updating the bundled manifest hash invalidates the existing stamp and rejects the old binary.
  const updatedManifest = structuredClone(manifest);
  updatedManifest.assets[0].sha256 = 'f'.repeat(64);
  await writeFile(join(root, 'native-release.json'), JSON.stringify(updatedManifest));
  await assert.rejects(resolveNative({ root }), /SHA-256/);
});

test('symlink or non-executable binary never passes cached resolution', async t => {
  const { root, artifacts, bytes } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });

  await chmod(path, 0o644);
  await assert.rejects(resolveNative({ root }), error => error?.code === 'EACCES');

  await chmod(path, 0o755);
  assert.equal(await resolveNative({ root }), path);

  const other = join(root, 'other-wx');
  await writeFile(other, bytes, { mode: 0o755 });
  await rm(path);
  await symlink(other, path);
  await assert.rejects(resolveNative({ root }), /regular file/);
});

test('read-only install directory hits cache and falls back cleanly when stamp is missing', async t => {
  if (process.getuid?.() === 0) return;
  const { root, artifacts } = await fixture(t);
  const path = await installNative({ root, artifactDir: artifacts });
  const dir = dirname(path);

  try {
    await chmod(dir, 0o555);
    let verifyCalls = 0;
    let inspectCalls = 0;
    assert.equal(await resolveNative({
      root,
      verify: (b, expected) => { verifyCalls++; return verifyBytes(b, expected); },
      inspect: async (p, v) => { inspectCalls++; return inspectBinary(p, v); },
    }), path);
    assert.equal(verifyCalls, 0);
    assert.equal(inspectCalls, 0);

    await chmod(dir, 0o755);
    await rm(stampPath(path));
    await chmod(dir, 0o555);
    assert.equal(await resolveNative({
      root,
      verify: (b, expected) => { verifyCalls++; return verifyBytes(b, expected); },
      inspect: async (p, v) => { inspectCalls++; return inspectBinary(p, v); },
    }), path);
    assert.equal(verifyCalls, 1);
    assert.equal(inspectCalls, 1);
  } finally {
    await chmod(dir, 0o755).catch(() => {});
  }
});

test('AGENT_WECHAT_NATIVE_BINARY caches identity in package native folder and re-checks on change', async t => {
  const { root, artifacts, asset, bytes } = await fixture(t);
  const overridePath = join(artifacts, asset.name);
  let inspectCalls = 0;
  const spyInspect = async (p, v) => { inspectCalls++; return inspectBinary(p, v); };

  assert.equal(await resolveNative({ root, binaryOverride: overridePath, inspect: spyInspect }), overridePath);
  assert.equal(inspectCalls, 1);
  // Override stamp stays inside package root, never next to operator binaries.
  await assert.rejects(lstat(stampPath(overridePath)), /ENOENT/);
  const stamp = JSON.parse(await readFile(overrideStampPath(root, version), 'utf8'));
  assert.equal(stamp.mode, 'override');
  assert.equal(stamp.path, overridePath);
  assert.equal(stamp.sha256, null);

  assert.equal(await resolveNative({ root, binaryOverride: overridePath, inspect: spyInspect }), overridePath);
  assert.equal(inspectCalls, 1);

  // Modifying the override binary invalidates the cache and rejects an invalid identity.
  await writeFile(overridePath, `#!/bin/sh\nprintf '{"product":"agent-wechat","backend":"container"}\\n'\n`);
  await assert.rejects(resolveNative({ root, binaryOverride: overridePath, inspect: spyInspect }), /matching native/);
  assert.equal(inspectCalls, 2);

  // Re-pointing a symlink override to another valid binary misses on canonical path and re-verifies.
  const second = join(artifacts, 'second-wx');
  await writeFile(second, bytes, { mode: 0o755 });
  const link = join(artifacts, 'wx-link');
  await symlink(second, link);
  assert.equal(await resolveNative({ root, binaryOverride: link, inspect: spyInspect }), second);
  assert.equal(inspectCalls, 3);
  assert.equal(await resolveNative({ root, binaryOverride: link, inspect: spyInspect }), second);
  assert.equal(inspectCalls, 3);

  // Corrupted override stamp forces re-verification and repairs the stamp.
  const overrideStampFile = overrideStampPath(root, version);
  await writeFile(overrideStampFile, '{corrupt');
  assert.equal(await resolveNative({ root, binaryOverride: link, inspect: spyInspect }), second);
  assert.equal(inspectCalls, 4);

  // Read-only package native directory falls back to identity verification cleanly.
  if (process.getuid?.() !== 0) {
    const stampDir = dirname(overrideStampFile);
    try {
      await rm(overrideStampFile);
      await chmod(stampDir, 0o555);
      assert.equal(await resolveNative({ root, binaryOverride: link, inspect: spyInspect }), second);
      assert.equal(inspectCalls, 5);
    } finally {
      await chmod(stampDir, 0o755).catch(() => {});
    }
  }
});
