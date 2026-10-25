// Inspect a built package without installing it or changing the host filesystem.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
const path = process.argv[2];
if (!path) throw new Error('Pass the built DEB path');
const files = execFileSync('dpkg-deb', ['--fsys-tarfile', path], { maxBuffer: 512 * 1024 * 1024 });
const names = execFileSync('tar', ['-t'], { input: files, encoding: 'utf8' }).split('\n');
assert.ok(names.some((name) => name.endsWith('/usr/bin/agent-wechat') || name === 'usr/bin/agent-wechat'));
for (const helper of ['wx', 'ffmpeg', 'ffprobe']) {
  assert.ok(names.some((name) => name.endsWith(`/usr/lib/AgentWechat/bin/${helper}`) || name === `usr/lib/AgentWechat/bin/${helper}`));
  assert.ok(!names.some((name) => name.endsWith(`/usr/bin/${helper}`) || name === `usr/bin/${helper}`), 'Helpers must not replace system commands');
}
console.log('Desktop DEB contains private helpers without replacing system CLI or media tools');
