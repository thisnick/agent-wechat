import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm, chmod, copyFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { assetName, fetchBytes, inspectBinary, installNative, installPath, manifestName, readManifest,
  resolveNative, supportedTarget, targetFor, validateManifest } from '../scripts/native-runtime.mjs';
import { prepareNativePackage, nativePackageUnpublished } from '../../../scripts/native-prepare-package.mjs';
const exec = promisify(execFile);
const version = '0.16.0';
const identity = { product: 'agent-wechat', backend: 'native', version, apiVersion: 1, target: supportedTarget.rustTarget, capabilities: ['serve'] };
const code = `#!/bin/sh\nexec node - "$@" <<'NATIVE_FIXTURE'\nif(process.argv[2]==='--json')console.log(${JSON.stringify(JSON.stringify(identity))});else if(process.argv[2]==='wait'){console.log('ready');process.on('SIGTERM',()=>process.exit(23));setInterval(()=>{},1000)}else {console.log(JSON.stringify(process.argv.slice(2)));process.exit(7)}\nNATIVE_FIXTURE\n`;
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'wx-native-package-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const artifacts = join(root, 'artifacts');
  await mkdir(artifacts);
  const bytes = Buffer.from(code);
  const asset = { name: assetName(version), ...supportedTarget, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
  const manifest = { schemaVersion: 1, version, repository: 'thisnick/agent-wechat', sourceCommit: 'a'.repeat(40), sourceTreeDirty: false, assets: [asset] };
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
  for (const name of ['native-launcher.mjs', 'native-runtime.mjs', 'native-install.mjs']) {
    await copyFile(new URL(`../scripts/${name}`, import.meta.url), join(root, 'scripts', name));
  }
  return join(root, 'scripts/native-launcher.mjs');
}

test('supported platform matrix rejects macOS, ARM64 and musl explicitly', () => {
  assert.deepEqual(targetFor({ platform: 'linux', arch: 'x64', glibc: '2.36' }), supportedTarget);
  for (const args of [{ platform: 'darwin', arch: 'x64', glibc: '2' }, { platform: 'linux', arch: 'arm64', glibc: '2' }, { platform: 'linux', arch: 'x64', glibc: null }, { platform: 'linux', arch: 'x64', glibc: '2.35' }]) {
    assert.throws(() => targetFor(args), /Linux x64 with glibc/);
  }
});
test('manifest pins exact version, source commit, target, SHA-256 and bounded size', async t => {
  const { root, manifest } = await fixture(t);
  assert.equal((await readManifest(root)).version, version);
  for (const mutation of [m => m.version = 'latest', m => m.sourceCommit = 'main', m => m.assets[0].rustTarget = 'aarch64-unknown-linux-gnu', m => m.assets[0].name = '../wx', m => m.assets[0].sha256 = 'short', m => m.assets[0].bytes = 2 ** 32, m => m.assets.push(m.assets[0])]) {
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
  child.kill('SIGTERM');
  const [code, signal] = await once(child, 'exit');
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
