// Publishes the UI journey videos that `pnpm --filter @agent-wechat/client-ui videos`
// recorded in CI, and points one pull request comment at them.
//
// GitHub plays inline video only from its own uploads, which have no API, so the
// comment shows each journey's animated GIF (an image GitHub displays inline) linked
// to the MP4. The files are committed under refs/ui-videos/pr-<number>: a ref
// outside refs/heads, so it is no branch and plain clones don't fetch it. Each
// publish replaces the ref with one parentless commit, and the comment links the
// files by that commit's SHA.
//
// ui-videos.yml runs this with actions/github-script for pull requests from
// branches of this repository only. The recording job ran the PR's code, so the
// downloaded files are treated as data: only the expected names, sizes and file
// types are published, and only as files and one comment.
import { lstat, readFile } from 'node:fs/promises';
import { join } from 'node:path';

export const MARKER = '<!-- agent-wechat-ui-videos -->';
export const REF_PREFIX = 'ui-videos/pr-';
const NAME = /^[a-z0-9]+(-[a-z0-9]+)*$/;
const MAX_JOURNEYS = 20;
const MAX_TOTAL = 90 * 1024 * 1024;
/** Each published file: its manifest key, name suffix, size limit and leading bytes. */
const KINDS = [
  { key: 'preview', suffix: '.gif', limit: 10 * 1024 * 1024, valid: bytes => ['GIF87a', 'GIF89a'].includes(bytes.subarray(0, 6).toString('latin1')) },
  { key: 'mp4', suffix: '.mp4', limit: 50 * 1024 * 1024, valid: bytes => bytes.subarray(4, 8).toString('latin1') === 'ftyp' },
  { key: 'frames', suffix: '-frames.png', limit: 10 * 1024 * 1024, valid: bytes => bytes.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) },
];

/** One line of plain text: no HTML, no line breaks, at most `limit` characters. */
export function plain(text, limit = 300) {
  return String(text ?? '').replace(/[<>`\r\n\t]/g, ' ').replace(/\s+/g, ' ').trim().slice(0, limit);
}

/** "0:07" for seconds. */
export function clock(seconds) {
  const whole = Math.max(0, Math.floor(Number(seconds) || 0));
  return `${Math.floor(whole / 60)}:${String(whole % 60).padStart(2, '0')}`;
}

/** The journeys in a manifest written by packages/client-ui/scripts/videos.mjs, checked and reduced to what the comment uses. */
export function readManifest(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_JOURNEYS) throw new Error('Invalid video manifest');
  const names = new Set();
  return value.map(entry => {
    if (typeof entry?.name !== 'string' || !NAME.test(entry.name) || names.has(entry.name)) throw new Error('Invalid journey name');
    names.add(entry.name);
    const files = {};
    for (const kind of KINDS) {
      const expected = `${entry.name}${kind.suffix}`;
      if (entry.files?.[kind.key]?.name !== expected) throw new Error(`Unexpected ${kind.key} file for ${entry.name}`);
      files[kind.key] = expected;
    }
    const chapters = Array.isArray(entry.chapters) ? entry.chapters.slice(0, 20) : [];
    return {
      name: entry.name,
      description: plain(entry.description),
      viewport: plain(entry.viewport, 20),
      seconds: Number.isFinite(entry.seconds) && entry.seconds >= 0 ? entry.seconds : 0,
      chapters: chapters.map(chapter => ({ title: plain(chapter?.title, 80), at: (Number(chapter?.atMs) || 0) / 1000 })),
      files,
    };
  });
}

/** Reads one file and checks its size and type before it may be published. */
export async function readMedia(dir, name, kind) {
  const path = join(dir, name);
  const stat = await lstat(path);
  if (!stat.isFile() || stat.size === 0 || stat.size > kind.limit) throw new Error(`Invalid size for ${name}`);
  const bytes = await readFile(path);
  if (!kind.valid(bytes)) throw new Error(`${name} is not the expected file type`);
  return bytes;
}

/** The comment body. `url(file)` gives a published file's address. */
export function commentBody({ journeys, headSha, runUrl, url }) {
  const sections = journeys.map(journey => {
    const chapters = journey.chapters.length
      ? `\n${journey.chapters.map(chapter => `\`${clock(chapter.at)}\` ${chapter.title}`).join(' · ')}\n`
      : '';
    return `#### ${journey.name}\n\n${journey.description} (${journey.viewport}, ${clock(journey.seconds)})\n${chapters}\n`
      + `<a href="${url(journey.files.mp4)}"><img src="${url(journey.files.preview)}" alt="${journey.name}" width="720"></a>\n\n`
      + `[MP4](${url(journey.files.mp4)}) · [Frames](${url(journey.files.frames)})\n`;
  });
  return `${MARKER}\n### UI videos\n\n`
    + `Recorded from \`packages/client-ui/journeys/\` against the synthetic simulator hub at commit \`${headSha.slice(0, 7)}\` `
    + `([workflow run](${runUrl})). Each preview links to its MP4, which downloads. `
    + 'Delete the journey files before merging; the videos here stay.\n\n'
    + `${sections.join('\n')}`;
}

export async function publish({ github, context, core, dir }) {
  const pr = context.payload.pull_request;
  const repository = `${context.repo.owner}/${context.repo.repo}`;
  if (pr?.head?.repo?.full_name !== repository) return core.info('Not a branch of this repository; nothing published.');
  const { data: current } = await github.rest.pulls.get({ ...context.repo, pull_number: pr.number });
  if (current.state !== 'open' || current.head.sha !== pr.head.sha) return core.info('The pull request moved on; a newer run publishes.');

  const manifestPath = join(dir, 'manifest.json');
  const manifestStat = await lstat(manifestPath);
  if (!manifestStat.isFile() || manifestStat.size > 200000) throw new Error('Invalid video manifest file');
  const journeys = readManifest(JSON.parse(await readFile(manifestPath, 'utf8')));

  const tree = [];
  let total = 0;
  for (const journey of journeys) {
    for (const kind of KINDS) {
      const name = journey.files[kind.key];
      const bytes = await readMedia(dir, name, kind);
      total += bytes.length;
      if (total > MAX_TOTAL) throw new Error('The videos are too large to publish');
      const { data: blob } = await github.rest.git.createBlob({ ...context.repo, content: bytes.toString('base64'), encoding: 'base64' });
      tree.push({ path: name, mode: '100644', type: 'blob', sha: blob.sha });
    }
  }
  const { data: createdTree } = await github.rest.git.createTree({ ...context.repo, tree });
  const { data: commit } = await github.rest.git.createCommit({
    ...context.repo,
    message: `UI videos for #${pr.number} at ${pr.head.sha}`,
    tree: createdTree.sha,
    parents: [],
  });
  const ref = `${REF_PREFIX}${pr.number}`;
  try {
    await github.rest.git.updateRef({ ...context.repo, ref, sha: commit.sha, force: true });
  } catch (error) {
    if (error.status !== 404 && error.status !== 422) throw error;
    await github.rest.git.createRef({ ...context.repo, ref: `refs/${ref}`, sha: commit.sha });
  }

  const raw = `https://raw.githubusercontent.com/${repository}/${commit.sha}`;
  const runUrl = `${context.serverUrl}/${repository}/actions/runs/${context.runId}`;
  const body = commentBody({ journeys, headSha: pr.head.sha, runUrl, url: file => `${raw}/${file}` });
  const comments = await github.paginate(github.rest.issues.listComments, { ...context.repo, issue_number: pr.number, per_page: 100 });
  const existing = comments.find(c => c.user?.login === 'github-actions[bot]' && c.user?.type === 'Bot' && c.body?.startsWith(MARKER));
  if (existing) await github.rest.issues.updateComment({ ...context.repo, comment_id: existing.id, body });
  else await github.rest.issues.createComment({ ...context.repo, issue_number: pr.number, body });
  core.info(`Published ${journeys.length} journey video(s) to refs/${ref} at ${commit.sha}.`);
}
