// Inspect a built package without installing it or changing the host filesystem.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
const path = process.argv[2];
if (!path) throw new Error('Pass the built DEB path');
const files = execFileSync('dpkg-deb', ['--fsys-tarfile', path], { maxBuffer: 512 * 1024 * 1024 });
const names = execFileSync('tar', ['-t'], { input: files, encoding: 'utf8' }).split('\n');
const architecture = execFileSync('dpkg-deb', ['--field', path, 'Architecture'], { encoding: 'utf8' }).trim();
const machine = { amd64: 62, arm64: 183 }[architecture];
assert.ok(machine, `Unsupported DEB architecture: ${architecture}`);
for (const suffix of ['usr/bin/agent-wechat', ...['wx', 'ffmpeg', 'ffprobe'].map(name => `usr/lib/AgentWechat/bin/${name}`)]) {
  const entry = names.find(name => name === suffix || name.endsWith('/' + suffix));
  assert.ok(entry, `Missing executable: ${suffix}`);
  const bytes = execFileSync('tar', ['-xOf', '-', entry], { input: files, maxBuffer: 256 * 1024 * 1024 });
  assert.equal(bytes.subarray(0, 4).toString('hex'), '7f454c46');
  assert.equal(bytes[4], 2, 'Expected a 64-bit executable');
  assert.equal(bytes[5], 1, 'Expected little-endian ELF');
  assert.equal(bytes.readUInt16LE(18), machine, `Architecture mismatch: ${suffix}`);
}
assert.ok(names.some((name) => name.endsWith('/usr/bin/agent-wechat') || name === 'usr/bin/agent-wechat'));
for (const helper of ['wx', 'ffmpeg', 'ffprobe']) {
  assert.ok(names.some((name) => name.endsWith(`/usr/lib/AgentWechat/bin/${helper}`) || name === `usr/lib/AgentWechat/bin/${helper}`));
  assert.ok(!names.some((name) => name.endsWith(`/usr/bin/${helper}`) || name === `usr/bin/${helper}`), 'Helpers must not replace system commands');
}
const soundEntry = names.find(name => name.endsWith('/usr/lib/AgentWechat/sounds/AgentWechatSoftPair.wav') || name === 'usr/lib/AgentWechat/sounds/AgentWechatSoftPair.wav');
assert.ok(soundEntry, 'Missing bundled notification sound');
assert.deepEqual(execFileSync('tar', ['-xOf', '-', soundEntry], { input: files }), readFileSync(new URL('../src-tauri/sounds/AgentWechatSoftPair.wav', import.meta.url)));
console.log('Desktop DEB contains private helpers and the notification sound without replacing system commands');
