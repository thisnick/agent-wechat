// Publishes the UI journey videos that `pnpm --filter @agent-wechat/client-ui videos`
// recorded in CI, and points one pull request comment at them.
//
// GitHub plays inline video only from its own uploads, which have no API, so the
// comment shows each journey's animated GIF (an image GitHub displays inline) linked
// to the MP4. The files are committed under refs/ui-videos/pr-<number>: a ref
// outside refs/heads, so it is no branch and plain clones don't fetch it. Each
// publish replaces it with one parentless commit holding the PR's current videos:
// new recordings, plus earlier videos of journeys this run didn't record (a journey
// deleted before merge keeps its video), reused from the previous commit without
// uploading them again. Superseded videos are left unreachable, so the ref holds
// only what the comment shows.
//
// ui-screenshots.yml runs this with actions/github-script, for pull requests from
// branches of this repository only. Their authors can already push to it, and the
// workflow and this script come from the pull request itself, so the checks below
// keep malformed recordings out of the comment rather than defend against the PR.
// The files are published only as files and one comment.
import { lstat, readFile } from 'node:fs/promises';
import { join } from 'node:path';

export const MARKER = '<!-- agent-wechat-ui-videos -->';
export const REF_PREFIX = 'ui-videos/pr-';
const NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/;
const SHA = /^[0-9a-f]{40}$/;
const MAX_JOURNEYS = 20;
const MAX_UPLOAD = 90 * 1024 * 1024;
/** GitHub's image proxy may refuse larger images; a bigger preview is left out and only the MP4 linked. */
export const PREVIEW_LIMIT = 5 * 1024 * 1024;
/** The widest a preview is drawn in the comment. */
const PREVIEW_WIDTH = 720;
const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
/** Each published file: its key, name suffix, size limit and leading bytes. An optional file over its limit is left out. */
export const KINDS = [
  { key: 'preview', suffix: '.gif', limit: PREVIEW_LIMIT, optional: true, valid: bytes => ['GIF87a', 'GIF89a'].includes(bytes.subarray(0, 6).toString('latin1')) },
  { key: 'mp4', suffix: '.mp4', limit: 50 * 1024 * 1024, valid: bytes => bytes.subarray(4, 8).toString('latin1') === 'ftyp' },
  { key: 'frames', suffix: '-frames.png', limit: 10 * 1024 * 1024, valid: bytes => bytes.subarray(0, 8).equals(PNG) },
];

/**
 * One line of plain text: no HTML, code spans or line breaks, at most about `limit`
 * characters. A word joiner after `@` and `#` keeps a journey's text from notifying a
 * GitHub user or linking an issue.
 */
export function plain(text, limit = 300) {
  return String(text ?? '')
    .replace(/[<>`\r\n\t]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, limit)
    .replace(/([@#])(?=[\w-])/g, '$1⁠');
}

/** "0:07" for seconds. */
export function clock(seconds) {
  const whole = Math.max(0, Math.floor(Number(seconds) || 0));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`;
}

const filesFor = name => Object.fromEntries(KINDS.map(kind => [kind.key, `${name}${kind.suffix}`]));
const number = (value, fallback = 0) => (Number.isFinite(value) && value >= 0 ? value : fallback);

/** The fields every entry has, from the recorder's manifest or a published one. */
function entry(raw, names) {
  if (typeof raw?.name !== 'string' || !NAME.test(raw.name) || names.has(raw.name)) throw new Error('Invalid journey name');
  names.add(raw.name);
  const chapters = Array.isArray(raw.chapters) ? raw.chapters.slice(0, 20) : [];
  return {
    name: raw.name,
    description: plain(raw.description),
    viewport: plain(raw.viewport, 20),
    seconds: number(raw.seconds),
    chapters: chapters.map(chapter => ({ title: plain(chapter?.title, 80), at: number(chapter?.at ?? Number(chapter?.atMs) / 1000) })),
    width: Math.min(PREVIEW_WIDTH, Math.round(number(raw.width ?? raw.files?.preview?.width, PREVIEW_WIDTH)) || PREVIEW_WIDTH),
  };
}

/** The journeys a run recorded, from the manifest packages/client-ui/scripts/videos.mjs writes. Empty when every journey failed. */
export function readManifest(value) {
  if (!Array.isArray(value) || value.length > MAX_JOURNEYS) throw new Error('Invalid video manifest');
  const names = new Set();
  return value.map(raw => {
    const journey = entry(raw, names);
    for (const kind of KINDS)
      if (raw.files?.[kind.key]?.name !== `${journey.name}${kind.suffix}`) throw new Error(`Unexpected ${kind.key} file for ${journey.name}`);
    return { ...journey, files: filesFor(journey.name) };
  });
}

/** The journeys an earlier publish listed in its manifest.json, with the commit and run that recorded each. */
export function readPublished(value, runPrefix) {
  if (!Array.isArray(value) || value.length > MAX_JOURNEYS) throw new Error('Invalid published manifest');
  const names = new Set();
  return value.map(raw => {
    const journey = entry(raw, names);
    const run = String(raw.run ?? '');
    if (!SHA.test(raw.commit ?? '') || !run.startsWith(runPrefix) || !/^\d+$/.test(run.slice(runPrefix.length)))
      throw new Error(`Invalid recording details for ${journey.name}`);
    const files = filesFor(journey.name);
    if (raw.files?.preview === null) files.preview = null;
    return { ...journey, commit: raw.commit, run, files };
  });
}

/** Reads one file and checks its size and type. Resolves to null for an optional file over its limit. */
export async function readMedia(dir, name, kind) {
  const path = join(dir, name);
  const stat = await lstat(path);
  if (!stat.isFile() || stat.size === 0) throw new Error(`Invalid file ${name}`);
  if (stat.size > kind.limit) {
    if (kind.optional) return null;
    throw new Error(`${name} is too large`);
  }
  const bytes = await readFile(path);
  if (!kind.valid(bytes)) throw new Error(`${name} is not the expected file type`);
  return bytes;
}

/** The recorder's manifest in `dir`, or no journeys when the run produced none. */
async function recordedIn(dir) {
  if (!dir) return [];
  const path = join(dir, 'manifest.json');
  const stat = await lstat(path).catch(error => (error.code === 'ENOENT' ? null : Promise.reject(error)));
  if (!stat) return [];
  if (!stat.isFile() || stat.size > 200000) throw new Error('Invalid video manifest file');
  return readManifest(JSON.parse(await readFile(path, 'utf8')));
}

/** The commit, files and journeys the PR's ref holds from earlier publishes. */
async function published({ github, context, core, ref, runPrefix }) {
  let commitSha;
  try {
    commitSha = (await github.rest.git.getRef({ ...context.repo, ref })).data.object.sha;
  } catch (error) {
    if (error.status === 404) return { commit: null, files: new Map(), journeys: [] };
    throw error;
  }
  const { data: commit } = await github.rest.git.getCommit({ ...context.repo, commit_sha: commitSha });
  const { data: tree } = await github.rest.git.getTree({ ...context.repo, tree_sha: commit.tree.sha });
  const files = new Map(tree.tree.filter(item => item.type === 'blob').map(item => [item.path, item.sha]));
  let journeys = [];
  if (files.has('manifest.json')) {
    try {
      const { data: blob } = await github.rest.git.getBlob({ ...context.repo, file_sha: files.get('manifest.json') });
      journeys = readPublished(JSON.parse(Buffer.from(blob.content, 'base64').toString('utf8')), runPrefix);
    } catch (error) {
      core.warning(`Not keeping earlier videos: ${error.message}`);
    }
  }
  return { commit: commitSha, files, journeys };
}

/** The comment body. `url(file)` gives a published file's address. */
export function commentBody({ journeys, failed = [], headSha, base, runUrl, url }) {
  const lines = [
    MARKER,
    '### UI videos',
    '',
    `Journeys from \`packages/client-ui/journeys/\`, recorded against the synthetic simulator hub, each on its commit merged with \`${plain(base, 100)}\`. `
      + 'Each preview links to its MP4, which downloads. Delete the journey files before merging; the videos here stay.',
    '',
  ];
  if (failed.length) lines.push(`> **Not recorded at \`${headSha.slice(0, 7)}\`:** ${failed.join(', ')}. See the [workflow run](${runUrl}).`, '');
  for (const journey of journeys) {
    lines.push(`#### ${journey.name}`, '', `${journey.description} (${journey.viewport}, ${clock(journey.seconds)}, recorded at [\`${journey.commit.slice(0, 7)}\`](${journey.run}))`);
    if (journey.chapters.length) lines.push('', journey.chapters.map(chapter => `\`${clock(chapter.at)}\` ${chapter.title}`).join(' · '));
    lines.push('', journey.files.preview
      ? `<a href="${url(journey.files.mp4)}"><img src="${url(journey.files.preview)}" alt="${journey.name}" width="${journey.width}"></a>`
      : '_The preview is too large to show here; download the MP4._');
    lines.push('', `[MP4](${url(journey.files.mp4)}) · [Frames](${url(journey.files.frames)})`, '');
  }
  return `${lines.join('\n')}\n`;
}

/**
 * Publishes what `dir` holds and updates the comment. `requested` names the journeys
 * the run tried to record; any of them missing from `dir` is reported as not recorded.
 */
export async function publish({ github, context, core, dir, requested }) {
  const pr = context.payload.pull_request;
  const repository = `${context.repo.owner}/${context.repo.repo}`;
  if (pr?.head?.repo?.full_name !== repository) return core.info('Not a branch of this repository; nothing published.');
  const { data: current } = await github.rest.pulls.get({ ...context.repo, pull_number: pr.number });
  if (current.state !== 'open' || current.head.sha !== pr.head.sha) return core.info('The pull request moved on; a newer run publishes.');
  const names = String(requested ?? '').split(/\s+/).filter(Boolean);
  if (names.some(name => !NAME.test(name))) throw new Error('Invalid journey names requested');

  const runPrefix = `${context.serverUrl}/${repository}/actions/runs/`;
  const runUrl = `${runPrefix}${context.runId}`;
  const ref = `${REF_PREFIX}${pr.number}`;
  const recorded = await recordedIn(dir);
  const failed = names.filter(name => !recorded.some(journey => journey.name === name));
  if (!recorded.length && !failed.length) return core.info('Nothing was recorded.');
  const earlier = await published({ github, context, core, ref, runPrefix });

  // New recordings replace earlier ones of the same name; the rest are kept.
  const tree = [];
  let uploaded = 0;
  for (const journey of recorded) {
    journey.commit = pr.head.sha;
    journey.run = runUrl;
    for (const kind of KINDS) {
      const bytes = await readMedia(dir, journey.files[kind.key], kind);
      if (!bytes) {
        core.warning(`${journey.files[kind.key]} is over ${kind.limit} bytes; linking only the MP4.`);
        journey.files[kind.key] = null;
        continue;
      }
      uploaded += bytes.length;
      if (uploaded > MAX_UPLOAD) throw new Error('The videos are too large to publish');
      const { data: blob } = await github.rest.git.createBlob({ ...context.repo, content: bytes.toString('base64'), encoding: 'base64' });
      tree.push({ path: journey.files[kind.key], mode: '100644', type: 'blob', sha: blob.sha });
    }
  }
  const fresh = new Map(recorded.map(journey => [journey.name, journey]));
  const kept = earlier.journeys.filter(journey =>
    !fresh.has(journey.name) && earlier.files.has(journey.files.mp4) && earlier.files.has(journey.files.frames));
  for (const journey of kept)
    if (journey.files.preview && !earlier.files.has(journey.files.preview)) journey.files.preview = null;
  // This run's recordings always fit; the most recently added earlier videos fill
  // what room is left, and the oldest are dropped.
  const order = earlier.journeys.map(journey => journey.name);
  const rank = journey => (order.includes(journey.name) ? order.indexOf(journey.name) : order.length);
  const room = Math.max(0, MAX_JOURNEYS - recorded.length);
  const journeys = [...kept.slice(kept.length - Math.min(room, kept.length)), ...recorded].sort((a, b) => rank(a) - rank(b));
  const listed = new Set(journeys.flatMap(journey => Object.values(journey.files).filter(Boolean)));

  let commitSha = earlier.commit;
  if (recorded.length) {
    for (const journey of journeys) {
      if (fresh.has(journey.name)) continue;
      for (const name of Object.values(journey.files))
        if (name) tree.push({ path: name, mode: '100644', type: 'blob', sha: earlier.files.get(name) });
    }
    tree.push({ path: 'manifest.json', mode: '100644', type: 'blob', content: `${JSON.stringify(journeys, null, 2)}\n` });
    const { data: createdTree } = await github.rest.git.createTree({
      ...context.repo,
      tree: tree.filter(item => item.path === 'manifest.json' || listed.has(item.path)),
    });
    const { data: commit } = await github.rest.git.createCommit({
      ...context.repo,
      message: `UI videos for #${pr.number} at ${pr.head.sha}`,
      tree: createdTree.sha,
      parents: [],
    });
    if (earlier.commit) await github.rest.git.updateRef({ ...context.repo, ref, sha: commit.sha, force: true });
    else await github.rest.git.createRef({ ...context.repo, ref: `refs/${ref}`, sha: commit.sha });
    commitSha = commit.sha;
    core.info(`Published ${recorded.length} video(s) to refs/${ref} at ${commit.sha}.`);
  }

  const raw = `https://raw.githubusercontent.com/${repository}/${commitSha}`;
  const body = commentBody({ journeys: commitSha ? journeys : [], failed, headSha: pr.head.sha, base: pr.base.ref, runUrl, url: file => `${raw}/${file}` });
  const comments = await github.paginate(github.rest.issues.listComments, { ...context.repo, issue_number: pr.number, per_page: 100 });
  const existing = comments.find(c => c.user?.login === 'github-actions[bot]' && c.user?.type === 'Bot' && c.body?.startsWith(MARKER));
  if (existing) await github.rest.issues.updateComment({ ...context.repo, comment_id: existing.id, body });
  else await github.rest.issues.createComment({ ...context.repo, issue_number: pr.number, body });
}
