#!/usr/bin/env node
// Real tarball/installer check using local artifacts only. Never installs globally.
import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile, copyFile, symlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { assetName, manifestName, targetFor, validateManifest } from '../packages/cli/scripts/native-runtime.mjs';
const exec = promisify(execFile);
const artifactDir = resolve(process.argv[2] ?? 'artifacts/native');
const source = resolve('packages/cli');
const pkg = JSON.parse(await readFile(join(source, 'package.json'), 'utf8'));
const manifest = JSON.parse(await readFile(join(artifactDir, manifestName(pkg.version)), 'utf8'));
validateManifest(manifest, pkg.version);
const temporary = await mkdtemp(join(tmpdir(), 'wx-native-pack-smoke-'));
try {
  const staged = join(temporary, 'source');
  await mkdir(staged);
  for (const name of ['package.json', 'README.md', 'LICENSE', 'THIRD_PARTY_NOTICES.txt', 'scripts']) await cp(join(source, name), join(staged, name), { recursive: true });
  await writeFile(join(staged, 'native-release.json'), JSON.stringify(manifest));
  const { stdout } = await exec(process.platform === 'win32' ? process.execPath : 'npm', [...(process.platform === 'win32' ? [join(process.execPath, '..', 'node_modules', 'npm', 'bin', 'npm-cli.js')] : []), 'pack', '--json', '--pack-destination', temporary], { cwd: staged, maxBuffer: 1024 * 1024 });
  const [packed] = JSON.parse(stdout);
  const names = packed.files.map(file => file.path);
  for (const required of ['native-release.json', 'LICENSE', 'THIRD_PARTY_NOTICES.txt', 'scripts/native-launcher.mjs', 'scripts/native-runtime.mjs', 'scripts/native-install.mjs']) assert.ok(names.includes(required), `Missing ${required}`);
  assert.equal(pkg.bin['wx-legacy'], undefined);
  assert.ok(!names.some(name => name.startsWith('dist/') || name === 'LEGACY.md'));
  assert.ok(!names.some(name => name.startsWith('src/') || name.startsWith('test/') || name.startsWith('native/')));
  const unpacked = join(temporary, 'unpacked');
  await mkdir(unpacked);
  // Git Bash's GNU tar interprets a Windows drive prefix as a remote host.
  // Relative paths work with both that tar and Windows' built-in bsdtar.
  await exec('tar', ['-xzf', packed.filename, '-C', 'unpacked'], { cwd: temporary });
  const root = join(unpacked, 'package');
  const launcher = join(root, 'scripts/native-launcher.mjs');
  await assert.rejects(exec(process.execPath, [launcher, '--json', 'version']), /not installed/);
  await exec(process.execPath, [join(root, 'scripts/native-install.mjs')], {
    env: { ...process.env, AGENT_WECHAT_NATIVE_ARTIFACT_DIR: artifactDir, AGENT_WECHAT_NATIVE_BINARY: '', AGENT_WECHAT_SKIP_NATIVE_INSTALL: '' },
  });
  const result = await exec(process.execPath, [launcher, '--json', 'version'], { env: { ...process.env, AGENT_WECHAT_NATIVE_BINARY: '' } });
  const identity = JSON.parse(result.stdout);
  assert.equal(identity.product, 'agent-wechat');
  assert.equal(identity.backend, 'native');
  assert.equal(identity.version, pkg.version);
  assert.equal(identity.target, targetFor().rustTarget);
  assert.ok(identity.capabilities.includes('serve'));
  if (process.platform === 'linux') {
    // Local call audio loads libpulse when it opens; wx itself must start without it.
    const { stdout: dynamic } = await exec('readelf', ['-d', join(artifactDir, assetName(pkg.version))]);
    assert.doesNotMatch(dynamic, /NEEDED.*libpulse/, 'The Linux wx executable must not link libpulse');
  }
  // Release executables must include local audio, but help must not open a
  // microphone, contact a daemon or require account credentials.
  const audioHelp = await exec(process.execPath, [launcher, 'calls', 'dial', '--help'], { env: { ...process.env, AGENT_WECHAT_NATIVE_BINARY: '' } });
  assert.match(audioHelp.stdout, /--input-device/);
  assert.match(audioHelp.stdout, /--output-device/);
  const devicesHelp = await exec(process.execPath, [launcher, 'calls', 'devices', '--help'], { env: { ...process.env, AGENT_WECHAT_NATIVE_BINARY: '' } });
  assert.match(devicesHelp.stdout, /local microphone\/speaker/i);
  // Exercise the same canonical npm-bin symlink/payload adapter used by managed
  // OpenClaw hosting; this creates only temporary empty account slots.
  const bin = join(temporary, 'bin'); await mkdir(bin); if (process.platform === 'win32') { await cp(root, join(bin, 'node_modules', '@agent-wechat', 'cli'), { recursive: true }); } else await symlink(launcher, join(bin, 'wx'));
  await exec(process.execPath, ['--experimental-strip-types', resolve('scripts/native-openclaw-smoke.mjs'), process.platform === 'win32' ? join(bin, 'node_modules', '@agent-wechat', 'cli', 'scripts', 'native-launcher.mjs') : join(bin, 'wx')], { maxBuffer: 1024 * 1024 });
  console.log(`Tarball verified: ${packed.filename}; offline installation and native identity passed. No global installation or network download occurred.`);
} finally { await rm(temporary, { recursive: true, force: true }); }
