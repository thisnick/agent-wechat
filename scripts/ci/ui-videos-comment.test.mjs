import assert from 'node:assert/strict';
import { mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { MARKER, clock, commentBody, plain, publish, readManifest, readMedia } from './ui-videos-comment.mjs';

const HEAD = 'abcdef0123456789abcdef0123456789abcdef01';
const GIF = Buffer.concat([Buffer.from('GIF89a', 'latin1'), Buffer.alloc(16)]);
const MP4 = Buffer.concat([Buffer.from([0, 0, 0, 24]), Buffer.from('ftypisom', 'latin1'), Buffer.alloc(16)]);
const PNG = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(16)]);

/** A manifest entry in the shape packages/client-ui/scripts/videos.mjs writes. */
const entry = (name, fields = {}) => ({
  name,
  description: 'Reply in a chat.',
  viewport: 'desktop',
  size: { width: 1280, height: 800 },
  seconds: 14.8,
  chapters: [{ title: 'A message arrives', description: null, atMs: 700 }, { title: 'Reply', description: null, atMs: 6200 }],
  files: {
    mp4: { name: `${name}.mp4`, bytes: MP4.length },
    preview: { name: `${name}.gif`, bytes: GIF.length, width: 960, fps: 10 },
    frames: { name: `${name}-frames.png`, bytes: PNG.length },
  },
  ...fields,
});

async function folder(entries, files = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'ui-videos-'));
  await writeFile(join(dir, 'manifest.json'), JSON.stringify(entries));
  for (const { name } of entries) {
    await writeFile(join(dir, `${name}.gif`), files.gif ?? GIF);
    await writeFile(join(dir, `${name}.mp4`), files.mp4 ?? MP4);
    await writeFile(join(dir, `${name}-frames.png`), files.png ?? PNG);
  }
  return dir;
}

function fakeGitHub({ headSha = HEAD, state = 'open', comments = [], refExists = false } = {}) {
  const calls = [];
  let blobs = 0;
  const github = {
    calls,
    paginate: async (_method, params) => { calls.push(['listComments', params]); return comments; },
    rest: {
      pulls: { get: async () => ({ data: { state, head: { sha: headSha } } }) },
      git: {
        createBlob: async params => { calls.push(['createBlob', params]); return { data: { sha: `blob${++blobs}` } }; },
        createTree: async params => { calls.push(['createTree', params]); return { data: { sha: 'tree1' } }; },
        createCommit: async params => { calls.push(['createCommit', params]); return { data: { sha: 'c0ffee' } }; },
        updateRef: async params => {
          calls.push(['updateRef', params]);
          if (!refExists) throw Object.assign(new Error('Reference does not exist'), { status: 422 });
        },
        createRef: async params => { calls.push(['createRef', params]); },
      },
      issues: {
        listComments: 'listComments',
        createComment: async params => { calls.push(['createComment', params]); },
        updateComment: async params => { calls.push(['updateComment', params]); },
      },
    },
  };
  return github;
}

const context = (headRepo = 'o/r') => ({
  repo: { owner: 'o', repo: 'r' },
  serverUrl: 'https://github.com',
  runId: 7,
  payload: { pull_request: { number: 12, head: { sha: HEAD, repo: { full_name: headRepo } } } },
});
const core = { messages: [], info(message) { this.messages.push(message); } };

test('readManifest keeps what the comment needs from the recorder manifest', () => {
  const [journey] = readManifest([entry('reply-in-chat')]);
  assert.deepEqual(journey, {
    name: 'reply-in-chat',
    description: 'Reply in a chat.',
    viewport: 'desktop',
    seconds: 14.8,
    chapters: [{ title: 'A message arrives', at: 0.7 }, { title: 'Reply', at: 6.2 }],
    files: { preview: 'reply-in-chat.gif', mp4: 'reply-in-chat.mp4', frames: 'reply-in-chat-frames.png' },
  });
});

test('readManifest rejects empty manifests, odd names, duplicates and unexpected files', () => {
  assert.throws(() => readManifest([]), /manifest/);
  assert.throws(() => readManifest({}), /manifest/);
  assert.throws(() => readManifest([entry('../escape')]), /name/);
  assert.throws(() => readManifest([entry('Reply')]), /name/);
  assert.throws(() => readManifest([entry('a'), entry('a')]), /name/);
  const renamed = entry('a');
  renamed.files.mp4.name = '../a.mp4';
  assert.throws(() => readManifest([renamed]), /mp4/);
});

test('plain text has no markup or line breaks and is bounded', () => {
  assert.equal(plain('<img src=x onerror=1>\nnext `code`'), 'img src=x onerror=1 next code');
  assert.equal(plain('x'.repeat(400)).length, 300);
  assert.equal(plain(undefined), '');
});

test('clock shows minutes and seconds', () => {
  assert.equal(clock(0.7), '0:00');
  assert.equal(clock(65.2), '1:05');
  assert.equal(clock('bad'), '0:00');
});

test('readMedia checks the file type and refuses links', async () => {
  const dir = await folder([entry('a')], { gif: PNG });
  try {
    const kind = { limit: 1024, valid: bytes => bytes.subarray(0, 3).toString('latin1') === 'GIF' };
    await assert.rejects(readMedia(dir, 'a.gif', kind), /expected file type/);
    await writeFile(join(dir, 'b.gif'), GIF);
    await symlink(join(dir, 'b.gif'), join(dir, 'c.gif'));
    await assert.rejects(readMedia(dir, 'c.gif', kind), /Invalid size/);
    assert.equal((await readMedia(dir, 'b.gif', kind)).length, GIF.length);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('commentBody links each preview to its MP4 and lists the chapters', () => {
  const body = commentBody({
    journeys: readManifest([entry('reply-in-chat')]),
    headSha: HEAD,
    runUrl: 'https://github.com/o/r/actions/runs/7',
    url: file => `https://raw.example/${file}`,
  });
  assert.ok(body.startsWith(MARKER));
  assert.match(body, /commit `abcdef0`/);
  assert.match(body, /<a href="https:\/\/raw\.example\/reply-in-chat\.mp4"><img src="https:\/\/raw\.example\/reply-in-chat\.gif"/);
  assert.match(body, /`0:00` A message arrives · `0:06` Reply/);
  assert.match(body, /\[Frames\]\(https:\/\/raw\.example\/reply-in-chat-frames\.png\)/);
});

test('publish commits the files under a PR ref and creates the comment', async () => {
  const dir = await folder([entry('reply-in-chat')]);
  try {
    const github = fakeGitHub();
    await publish({ github, context: context(), core, dir });
    const call = name => github.calls.filter(([method]) => method === name).map(([, params]) => params);
    assert.equal(call('createBlob').length, 3);
    assert.deepEqual(call('createTree')[0].tree.map(item => item.path), ['reply-in-chat.gif', 'reply-in-chat.mp4', 'reply-in-chat-frames.png']);
    assert.deepEqual(call('createCommit')[0].parents, []);
    assert.equal(call('updateRef')[0].ref, 'ui-videos/pr-12');
    assert.equal(call('createRef')[0].ref, 'refs/ui-videos/pr-12');
    const [comment] = call('createComment');
    assert.equal(comment.issue_number, 12);
    assert.match(comment.body, /https:\/\/raw\.githubusercontent\.com\/o\/r\/c0ffee\/reply-in-chat\.gif/);
    assert.match(comment.body, /actions\/runs\/7/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('publish replaces the existing ref and updates the bot comment', async () => {
  const dir = await folder([entry('reply-in-chat')]);
  try {
    const comments = [
      { id: 1, user: { login: 'someone', type: 'User' }, body: `${MARKER} copied` },
      { id: 2, user: { login: 'github-actions[bot]', type: 'Bot' }, body: `${MARKER}\nold` },
    ];
    const github = fakeGitHub({ comments, refExists: true });
    await publish({ github, context: context(), core, dir });
    const methods = github.calls.map(([method]) => method);
    assert.ok(!methods.includes('createRef'));
    assert.ok(!methods.includes('createComment'));
    assert.equal(github.calls.find(([method]) => method === 'updateComment')[1].comment_id, 2);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('publish does nothing for forks or a pull request that moved on', async () => {
  for (const [github, ctx] of [[fakeGitHub(), context('fork/r')], [fakeGitHub({ headSha: 'newer' }), context()], [fakeGitHub({ state: 'closed' }), context()]]) {
    await publish({ github, context: ctx, core, dir: '/nonexistent' });
    assert.deepEqual(github.calls, []);
  }
});

test('publish refuses a file that is not what its name says', async () => {
  const dir = await folder([entry('reply-in-chat')], { mp4: GIF });
  try {
    const github = fakeGitHub();
    await assert.rejects(publish({ github, context: context(), core, dir }), /not the expected file type/);
    assert.ok(!github.calls.some(([method]) => ['createCommit', 'createComment'].includes(method)));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
