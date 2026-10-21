#!/usr/bin/env node
// Package a locally built executable; never publishes or accesses account data.
import { execFileSync } from 'node:child_process';
import { copyFile, mkdir, readdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { assetName, executableName, manifestName, targetFor } from '../packages/cli/scripts/native-runtime.mjs';
import { generateManifest } from './native-release-manifest.mjs';
const version = JSON.parse(execFileSync(process.execPath, ['-p', "JSON.stringify(require('./packages/cli/package.json').version)"], { encoding: 'utf8' }));
const sourceCommit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
const dirty = execFileSync('git', ['status', '--porcelain'], { encoding: 'utf8' }).trim().length > 0;
const output = resolve(process.argv[2] ?? 'artifacts/native');
await mkdir(output, { recursive: true });
if ((await readdir(output)).length) throw new Error('Release output must be empty');
const binary = resolve(`packages/native-client-rust/target/${targetFor().rustTarget}/release/${executableName()}`);
const destination = join(output, assetName(version));
await copyFile(binary, destination);
const manifest = await generateManifest(destination, version, sourceCommit, dirty);
await writeFile(join(output, manifestName(version)), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' });
await copyFile('packages/cli/THIRD_PARTY_NOTICES.txt', join(output, `wx-v${version}-THIRD-PARTY-NOTICES.txt`));
console.log(`Prepared ${destination}; no publication occurred.`);
