// Downloads a pinned media helper from media-assets.json and checks its SHA-256.
//
// GitHub's release download links (github.com/OWNER/REPO/releases/download/...)
// sometimes fail with HTTP 5xx for hours while the REST API still serves the same
// file. Each attempt tries the release link first, then the API asset URL, and the
// SHA-256 check applies to whichever source answers.
import { createHash } from "node:crypto";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The REST API URL for an asset pinned with its GitHub release asset ID. */
export function apiAssetUrl(asset) {
  const match = /^https:\/\/github\.com\/([^/]+)\/([^/]+)\/releases\/download\//.exec(asset.url);
  if (!match || !Number.isSafeInteger(asset.assetId)) return null;
  return `https://api.github.com/repos/${match[1]}/${match[2]}/releases/assets/${asset.assetId}`;
}

async function fromRelease(asset, fetchImpl) {
  const response = await fetchImpl(asset.url, { signal: AbortSignal.timeout(300000) });
  if (!response.ok) throw new Error(`release link: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

async function fromApi(url, token, fetchImpl) {
  // Follow the redirect by hand so the token never reaches the storage host.
  const headers = { Accept: "application/octet-stream", "X-GitHub-Api-Version": "2022-11-28" };
  if (token) headers.Authorization = `Bearer ${token}`;
  const redirect = await fetchImpl(url, { headers, redirect: "manual", signal: AbortSignal.timeout(30000) });
  const location = redirect.headers.get("location");
  if (redirect.status < 300 || redirect.status > 399 || !location)
    throw new Error(`API: HTTP ${redirect.status}`);
  const response = await fetchImpl(location, { signal: AbortSignal.timeout(300000) });
  if (!response.ok) throw new Error(`API download: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

/**
 * Returns the asset's bytes once their SHA-256 matches the pin. Tries the release link,
 * then the API, up to `attempts` times. A hash mismatch stops at once.
 */
export async function downloadVerified(asset, {
  fetchImpl = fetch,
  token = process.env.GH_TOKEN ?? process.env.GITHUB_TOKEN,
  attempts = 3,
  wait = (attempt) => sleep(10000 * 3 ** (attempt - 1)),
} = {}) {
  const api = apiAssetUrl(asset);
  const sources = [() => fromRelease(asset, fetchImpl), ...(api ? [() => fromApi(api, token, fetchImpl)] : [])];
  const errors = [];
  for (let attempt = 1; attempt <= attempts; attempt++) {
    if (attempt > 1) await wait(attempt - 1);
    for (const source of sources) {
      let bytes;
      try {
        bytes = await source();
      } catch (error) {
        errors.push(error.message);
        continue;
      }
      if (createHash("sha256").update(bytes).digest("hex") !== asset.sha256)
        throw new Error(`Integrity check failed for ${asset.name}`);
      return bytes;
    }
  }
  throw new Error(`Cannot download ${asset.name} after ${attempts} attempts: ${errors.join("; ")}`);
}
