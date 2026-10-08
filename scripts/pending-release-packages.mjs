import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function pendingReleasePackages({
  cwd = process.cwd(),
  env = process.env,
  fetchImpl = fetch,
} = {}) {
  const packagesDir = join(cwd, 'packages');
  const entries = await readdir(packagesDir, { withFileTypes: true });
  const manifests = await Promise.all(entries.filter(entry => entry.isDirectory()).map(async entry =>
    JSON.parse(await readFile(join(packagesDir, entry.name, 'package.json'), 'utf8'))));
  const packages = manifests.filter(pkg => !pkg.private).map(({ name, version }) => ({ name, version }));
  if (!packages.length || packages.some(pkg => !/^@agent-wechat\/[a-z0-9-]+$/.test(pkg.name)
    || !/^\d+\.\d+\.\d+$/.test(pkg.version) || pkg.version !== packages[0].version)) {
    throw new Error('Expected public release packages with one shared version');
  }
  if (!/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/.test(env.GITHUB_REPOSITORY ?? '')) {
    throw new Error('GITHUB_REPOSITORY must identify the release repository');
  }

  const response = await fetchImpl(
    `https://api.github.com/repos/${env.GITHUB_REPOSITORY}/releases/tags/v${packages[0].version}`,
    { headers: { Accept: 'application/vnd.github+json',
      ...(env.GH_TOKEN ? { Authorization: `Bearer ${env.GH_TOKEN}` } : {}) },
    signal: AbortSignal.timeout(10000) });
  if (response.status === 200) return [];
  if (response.status !== 404) {
    throw new Error(`Unable to check the pending GitHub release (HTTP ${response.status})`);
  }
  console.error(`Resuming missing release v${packages[0].version} after npm publication`);
  return packages;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  console.log(JSON.stringify(await pendingReleasePackages()));
}
