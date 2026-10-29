#!/usr/bin/env node
// Changesets owns the npm version; the standalone Rust workspace uses that same release.
import { readFile, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { syncCrateVersions } from './native-crates.mjs';
const { version } = JSON.parse(await readFile('packages/cli/package.json', 'utf8'));
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error('Invalid CLI version');
const path = 'packages/native-client-rust/Cargo.toml';
const old = await readFile(path, 'utf8');
const match = old.match(/(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/);
if (!match) throw new Error('Cannot find native workspace package version');
const updated = old.replace(match[0], `${match[1]}${version}${match[2]}`);
if (old !== updated) await writeFile(path, updated);
await syncCrateVersions(process.cwd(), version);
execFileSync('cargo', ['update', '--workspace', '--manifest-path', path], { stdio: 'inherit' });
// Desktop follows the native release train while remaining a separate package.
const desktopPackage = 'packages/desktop/package.json';
const desktop = JSON.parse(await readFile(desktopPackage, 'utf8'));
desktop.version = version;
await writeFile(desktopPackage, JSON.stringify(desktop, null, 2) + '\n');
const desktopConfig = 'packages/desktop/src-tauri/tauri.conf.json';
const config = JSON.parse(await readFile(desktopConfig, 'utf8'));
config.version = version;
await writeFile(desktopConfig, JSON.stringify(config, null, 2) + '\n');
const manifest = 'packages/desktop/src-tauri/Cargo.toml';
const content = await readFile(manifest, 'utf8');
await writeFile(manifest, content.replace(/(\[package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/, `$1${version}$2`));
execFileSync('cargo', ['update', '--workspace', '--manifest-path', manifest], { stdio: 'inherit' });
console.log(`Native and desktop versions synchronized to ${version}`);
