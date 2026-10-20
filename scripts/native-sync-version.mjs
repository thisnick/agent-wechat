#!/usr/bin/env node
// Changesets owns the npm version; the standalone Rust workspace uses that same release.
import { readFile, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
const { version } = JSON.parse(await readFile('packages/cli/package.json', 'utf8'));
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error('Invalid CLI version');
const path = 'packages/native-client-rust/Cargo.toml';
const old = await readFile(path, 'utf8');
const match = old.match(/(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/);
if (!match) throw new Error('Cannot find native workspace package version');
const updated = old.replace(match[0], `${match[1]}${version}${match[2]}`);
if (old !== updated) await writeFile(path, updated);
execFileSync('cargo', ['update', '--workspace', '--manifest-path', path], { stdio: 'inherit' });
console.log(`Native workspace version synchronized to ${version}`);
