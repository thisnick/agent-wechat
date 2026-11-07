// The private Changesets package versions public Rust libraries independently of apps.
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { releaseChannel } from './native-release-policy.mjs';

export const crateReleaseTag = version => `crates-v${version}`;

export const cratePackage = { directory: 'packages/rust-crates', name: '@agent-wechat/rust-crates' };

export async function readCrateVersion({ root = process.cwd(), version } = {}) {
  const pkg = JSON.parse(await readFile(join(root, cratePackage.directory, 'package.json'), 'utf8'));
  if (pkg.name !== cratePackage.name || pkg.private !== true || (version !== undefined && pkg.version !== version)) {
    throw new Error('Rust library package/version mismatch');
  }
  // The committed library version can remain a beta while the apps leave beta.
  const tag = pkg.version?.split('-')[1]?.split('.')[0];
  releaseChannel(pkg.version, tag ? { mode: 'pre', tag } : undefined);
  return pkg.version;
}

export async function readCratePolicy(options = {}) {
  const version = await readCrateVersion(options);
  const tag = version.split('-')[1]?.split('.')[0];
  return releaseChannel(version, tag ? { mode: 'pre', tag } : undefined);
}
