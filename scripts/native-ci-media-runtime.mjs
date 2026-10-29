#!/usr/bin/env node
// Use the desktop's pinned helpers for native simulator media checks as well.
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { EOL } from 'node:os';

const directory = process.argv[2];
if (!directory) throw new Error('Usage: native-ci-media-runtime.mjs OUTPUT_DIRECTORY');
const platform = `${process.platform}-${process.platform === 'win32' ? 'x64' : process.arch}`;
const assets = JSON.parse(await readFile(new URL('../packages/desktop/media-assets.json', import.meta.url), 'utf8'));
const output = resolve(directory);
await mkdir(output, { recursive: true });
for (const name of ['ffmpeg', 'ffprobe']) {
  // Windows ARM64 uses the same x64 helpers as the desktop, through emulation.
  const asset = assets.find(asset => asset.name === `${name}-${platform}`);
  if (!asset || !/^[0-9a-f]{64}$/.test(asset.sha256)) throw new Error(`No pinned ${name} for ${platform}`);
  const response = await fetch(asset.url, { signal: AbortSignal.timeout(300000) });
  if (!response.ok) throw new Error(`Cannot download ${asset.name}: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (createHash('sha256').update(bytes).digest('hex') !== asset.sha256) throw new Error(`Integrity check failed for ${asset.name}`);
  const destination = join(output, `${name}${process.platform === 'win32' ? '.exe' : ''}`);
  await writeFile(destination, bytes, { mode: 0o755 });
  await chmod(destination, 0o755);
  execFileSync(destination, ['-version'], { stdio: 'inherit' });
}
if (process.env.GITHUB_PATH) await writeFile(process.env.GITHUB_PATH, `${output}${EOL}`, { flag: 'a' });
