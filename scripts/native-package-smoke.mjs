#!/usr/bin/env node
// Real tarball/installer check using local artifacts only. Never installs globally.
import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, readFile, rm, writeFile, copyFile, symlink } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { manifestName, validateManifest } from '../packages/cli/scripts/native-runtime.mjs';
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
  for (const name of ['package.json', 'README.md', 'LEGACY.md', 'scripts', 'dist']) await cp(join(source, name), join(staged, name), { recursive: true });
  await writeFile(join(staged, 'native-release.json'), JSON.stringify(manifest));
  const { stdout } = await exec('npm', ['pack', '--json', '--pack-destination', temporary], { cwd: staged, maxBuffer: 1024 * 1024 });
  const [packed] = JSON.parse(stdout);
  const names = packed.files.map(file => file.path);
  for (const required of ['native-release.json', 'LEGACY.md', 'scripts/native-launcher.mjs', 'scripts/native-runtime.mjs', 'scripts/native-install.mjs', 'dist/cli.js']) assert.ok(names.includes(required), `Missing ${required}`);
  assert.ok(!names.some(name => name.startsWith('src/') || name.startsWith('test/') || name.startsWith('native/')));
  const unpacked = join(temporary, 'unpacked');
  await mkdir(unpacked);
  await exec('tar', ['-xzf', join(temporary, packed.filename), '-C', unpacked]);
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
  assert.equal(identity.target, 'x86_64-unknown-linux-gnu');
  assert.ok(identity.capabilities.includes('serve'));
  // Exercise the same canonical npm-bin symlink/payload adapter used by managed
  // OpenClaw hosting; this creates only temporary empty account slots.
  const bin = join(temporary, 'bin'); await mkdir(bin); await symlink(launcher, join(bin, 'wx'));
  await exec(process.execPath, ['--experimental-strip-types', resolve('scripts/native-openclaw-smoke.mjs'), join(bin, 'wx')], { maxBuffer: 1024 * 1024 });
  console.log(`Tarball verified: ${packed.filename}; offline installation and native identity passed. No global installation or network download occurred.`);
} finally { await rm(temporary, { recursive: true, force: true }); }
