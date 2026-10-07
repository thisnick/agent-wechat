import { pathToFileURL } from 'node:url';

export async function verifyNpmReleases(packages, {
  fetchImpl = fetch,
  attempts = 12,
  delay = () => new Promise(resolve => setTimeout(resolve, 5000)),
} = {}) {
  if (!Array.isArray(packages) || !packages.length || packages.some(pkg =>
    !/^@agent-wechat\/[a-z0-9-]+$/.test(pkg?.name) || !/^\d+\.\d+\.\d+$/.test(pkg?.version))) {
    throw new Error('Invalid published package list');
  }
  let missing = packages;
  for (let attempt = 0; attempt < attempts; attempt++) {
    const remaining = [];
    for (const pkg of missing) {
      try {
        const response = await fetchImpl(`https://registry.npmjs.org/${encodeURIComponent(pkg.name)}/${pkg.version}`, {
          headers: { 'Cache-Control': 'no-cache' }, signal: AbortSignal.timeout(10000),
        });
        const metadata = response.ok ? await response.json() : null;
        if (metadata?.name === pkg.name && metadata?.version === pkg.version) continue;
      } catch { /* Retry a bounded registry propagation/network delay. */ }
      remaining.push(pkg);
    }
    missing = remaining;
    if (!missing.length) return;
    if (attempt + 1 < attempts) await delay();
  }
  throw new Error(`npm publication not verified: ${missing.map(pkg => `${pkg.name}@${pkg.version}`).join(', ')}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await verifyNpmReleases(JSON.parse(process.argv[2]));
  console.log('Published package versions verified in the npm registry');
}
