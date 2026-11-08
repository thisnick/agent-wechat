// Minimal GitHub REST helpers for the interface review workflows.
import { readFileSync } from 'node:fs';

export function pullRequestEvent(env = process.env) {
  if (!env.GITHUB_EVENT_PATH) throw new Error('GITHUB_EVENT_PATH is not set');
  return JSON.parse(readFileSync(env.GITHUB_EVENT_PATH, 'utf8'));
}

export async function api(path, { method = 'GET', body, env = process.env, fetchImpl = fetch } = {}) {
  if (!env.GITHUB_TOKEN || !env.GITHUB_REPOSITORY) throw new Error('GITHUB_TOKEN and GITHUB_REPOSITORY are required');
  const response = await fetchImpl(`https://api.github.com/repos/${env.GITHUB_REPOSITORY}${path}`, {
    method,
    headers: { Authorization: `Bearer ${env.GITHUB_TOKEN}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28',
      ...(body ? { 'Content-Type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(30000),
  });
  if (response.status === 404 && method === 'DELETE') return null;
  if (!response.ok) throw new Error(`GitHub ${method} ${path} failed with HTTP ${response.status}: ${(await response.text()).slice(0, 300)}`);
  return response.status === 204 ? null : response.json();
}

export async function findComment(marker, { issue, ...options }) {
  for (let page = 1; page <= 20; page += 1) {
    const comments = await api(`/issues/${issue}/comments?per_page=100&page=${page}`, options);
    const found = comments.find(comment => comment.body?.includes(marker));
    if (found) return found;
    if (comments.length < 100) return undefined;
  }
  return undefined;
}

/** Updates the comment carrying `marker`, or creates it when `createIfMissing`. */
export async function upsertComment({ marker, body, createIfMissing = true, issue, ...options }) {
  const number = issue ?? pullRequestEvent(options.env).pull_request.number;
  const existing = await findComment(marker, { issue: number, ...options });
  if (existing) return api(`/issues/comments/${existing.id}`, { method: 'PATCH', body: { body }, ...options });
  if (createIfMissing) return api(`/issues/${number}/comments`, { method: 'POST', body: { body }, ...options });
  return null;
}

export async function removeLabel(issue, label, options = {}) {
  return api(`/issues/${issue}/labels/${encodeURIComponent(label)}`, { method: 'DELETE', ...options });
}
