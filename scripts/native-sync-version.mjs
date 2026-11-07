#!/usr/bin/env node
// Changesets versions applications and published Rust libraries independently.
import { readFile, writeFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { syncCrateVersions } from './native-crates.mjs';
import { readCrateVersion } from './native-crates-version.mjs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export async function syncVersions(root = process.cwd(), { runCargo = args => execFileSync('cargo', args, { cwd: root, stdio: 'inherit' }) } = {}) {
  const { version } = JSON.parse(await readFile(join(root, 'packages/cli/package.json'), 'utf8'));
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) throw new Error('Invalid CLI version');
  const path = join(root, 'packages/native-client-rust/Cargo.toml');
  const old = await readFile(path, 'utf8');
  const match = old.match(/(\[workspace\.package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/);
  if (!match) throw new Error('Cannot find native workspace package version');
  const updated = old.replace(match[0], `${match[1]}${version}${match[2]}`);
  if (old !== updated) await writeFile(path, updated);
  const crateVersion = await readCrateVersion({ root });
  await syncCrateVersions(root, crateVersion);
  runCargo(['update', '--workspace', '--manifest-path', path]);
  // Desktop follows the native release train while remaining a separate package.
  const desktopPackage = join(root, 'packages/desktop/package.json');
  const desktop = JSON.parse(await readFile(desktopPackage, 'utf8'));
  desktop.version = version;
  await writeFile(desktopPackage, JSON.stringify(desktop, null, 2) + '\n');
  const desktopConfig = join(root, 'packages/desktop/src-tauri/tauri.conf.json');
  const config = JSON.parse(await readFile(desktopConfig, 'utf8'));
  config.version = version;
  await writeFile(desktopConfig, JSON.stringify(config, null, 2) + '\n');
  const manifest = join(root, 'packages/desktop/src-tauri/Cargo.toml');
  const content = await readFile(manifest, 'utf8');
  await writeFile(manifest, content.replace(/(\[package\][\s\S]*?\nversion\s*=\s*")[^"]+("\s*\n)/, `$1${version}$2`));
  runCargo(['update', '--workspace', '--manifest-path', manifest]);
  return { applicationVersion: version, crateVersion };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  console.log(JSON.stringify(await syncVersions()));
}
