import assert from 'node:assert/strict';
import { mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { KINDS, MARKER, clock, commentBody, plain, publish, readManifest, readMedia, readPublished } from './ui-videos-comment.mjs';

const HEAD = 'abcdef0123456789abcdef0123456789abcdef01';
const OLD = '1111111111111111111111111111111111111111';
const RUNS = 'https://github.com/o/r/actions/runs/';
const GIF = Buffer.concat([Buffer.from('GIF89a', 'latin1'), Buffer.alloc(16)]);
const MP4 = Buffer.concat([Buffer.from([0, 0, 0, 24]), Buffer.from('ftypisom', 'latin1'), Buffer.alloc(16)]);
const PNG = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), Buffer.alloc(16)]);
const kind = key => KINDS.find(item => item.key === key);

/** A manifest entry in the shape packages/client-ui/scripts/videos.mjs writes. */
const recorded = (name, fields = {}) => ({
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

/** An entry in the manifest.json an earlier publish stored. */
const earlier = name => ({
  name, description: 'Earlier.', viewport: 'narrow', seconds: 9, chapters: [], width: 390,
  commit: OLD, run: `${RUNS}3`, files: { preview: `${name}.gif`, mp4: `${name}.mp4`, frames: `${name}-frames.png` },
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

/** A GitHub client double. `previous` is the manifest an earlier publish stored, or null for no ref. */
function fakeGitHub({ headSha = HEAD, state = 'open', comments = [], previous = null } = {}) {
  const calls = [];
  let blobs = 0;
  const notFound = () => Promise.reject(Object.assign(new Error('Not Found'), { status: 404 }));
  const previousFiles = previous ? previous.flatMap(entry => Object.values(entry.files).filter(Boolean)) : [];
  return {
    calls,
    paginate: async (_method, params) => { calls.push(['listComments', params]); return comments; },
    rest: {
      pulls: { get: async () => ({ data: { state, head: { sha: headSha } } }) },
      git: {
        getRef: async params => { calls.push(['getRef', params]); return previous ? { data: { object: { sha: OLD } } } : notFound(); },
        getCommit: async () => ({ data: { tree: { sha: 'oldtree' } } }),
        getTree: async () => ({
          data: { tree: [...previousFiles.map(path => ({ path, type: 'blob', sha: `old-${path}` })), { path: 'manifest.json', type: 'blob', sha: 'old-manifest' }] },
        }),
        getBlob: async () => ({ data: { content: Buffer.from(JSON.stringify(previous)).toString('base64') } }),
        createBlob: async params => { calls.push(['createBlob', params]); return { data: { sha: `blob${++blobs}` } }; },
        createTree: async params => { calls.push(['createTree', params]); return { data: { sha: 'tree1' } }; },
        createCommit: async params => { calls.push(['createCommit', params]); return { data: { sha: 'c0ffee' } }; },
        updateRef: async params => { calls.push(['updateRef', params]); },
        createRef: async params => { calls.push(['createRef', params]); },
      },
      issues: {
        listComments: 'listComments',
        createComment: async params => { calls.push(['createComment', params]); },
        updateComment: async params => { calls.push(['updateComment', params]); },
      },
    },
  };
}

const context = (headRepo = 'o/r') => ({
  repo: { owner: 'o', repo: 'r' },
  serverUrl: 'https://github.com',
  runId: 7,
  payload: { pull_request: { number: 12, base: { ref: 'main' }, head: { sha: HEAD, repo: { full_name: headRepo } } } },
});
const core = { messages: [], info(message) { this.messages.push(message); }, warning(message) { this.messages.push(message); } };
const calls = (github, name) => github.calls.filter(([method]) => method === name).map(([, params]) => params);

async function inFolder(entries, files, run) {
  const dir = await folder(entries, files);
  try {
    return await run(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test('readManifest keeps what the comment needs from the recorder manifest', () => {
  const [journey] = readManifest([recorded('reply-in-chat')]);
  assert.deepEqual(journey, {
    name: 'reply-in-chat',
    description: 'Reply in a chat.',
    viewport: 'desktop',
    seconds: 14.8,
    chapters: [{ title: 'A message arrives', at: 0.7 }, { title: 'Reply', at: 6.2 }],
    width: 720,
    files: { preview: 'reply-in-chat.gif', mp4: 'reply-in-chat.mp4', frames: 'reply-in-chat-frames.png' },
  });
  assert.deepEqual(readManifest([]), []);
});

test('readManifest draws a phone-width preview at its own width', () => {
  const narrow = recorded('phone');
  narrow.files.preview.width = 390;
  assert.equal(readManifest([narrow])[0].width, 390);
});

test('readManifest rejects odd names, duplicates and unexpected files', () => {
  assert.throws(() => readManifest({}), /manifest/);
  assert.throws(() => readManifest([recorded('../escape')]), /name/);
  assert.throws(() => readManifest([recorded('Reply')]), /name/);
  assert.throws(() => readManifest([recorded('a'), recorded('a')]), /name/);
  const renamed = recorded('a');
  renamed.files.mp4.name = '../a.mp4';
  assert.throws(() => readManifest([renamed]), /mp4/);
});

test('readPublished accepts what a publish stored and rejects foreign run links', () => {
  assert.equal(readPublished([earlier('a')], RUNS)[0].commit, OLD);
  assert.throws(() => readPublished([{ ...earlier('a'), run: 'https://example.com/actions/runs/3' }], RUNS), /recording details/);
  assert.throws(() => readPublished([{ ...earlier('a'), commit: 'main' }], RUNS), /recording details/);
});

test('plain text has no markup, mentions or line breaks and is bounded', () => {
  assert.equal(plain('<img src=x onerror=1>\nnext `code`'), 'img src=x onerror=1 next code');
  assert.equal(plain('Mention @mei about #12'), 'Mention @⁠mei about #⁠12');
  assert.ok(plain('x'.repeat(400)).length <= 300);
  assert.equal(plain(undefined), '');
});

test('clock shows minutes and seconds', () => {
  assert.equal(clock(0.7), '0:00');
  assert.equal(clock(65.2), '1:05');
  assert.equal(clock('bad'), '0:00');
});

test('readMedia checks the file type, refuses links and leaves out an oversized optional file', async () => {
  await inFolder([recorded('a')], { gif: PNG }, async dir => {
    await assert.rejects(readMedia(dir, 'a.gif', kind('preview')), /expected file type/);
    await writeFile(join(dir, 'b.gif'), GIF);
    await symlink(join(dir, 'b.gif'), join(dir, 'c.gif'));
    await assert.rejects(readMedia(dir, 'c.gif', kind('preview')), /Invalid file/);
    assert.equal((await readMedia(dir, 'b.gif', kind('preview'))).length, GIF.length);
    assert.equal(await readMedia(dir, 'b.gif', { ...kind('preview'), limit: 4 }), null);
    await assert.rejects(readMedia(dir, 'b.gif', { ...kind('mp4'), limit: 4 }), /too large/);
  });
});

test('commentBody links each preview to its MP4, lists chapters and names failures', () => {
  const journeys = readManifest([recorded('reply-in-chat')]).map(journey => ({ ...journey, commit: HEAD, run: `${RUNS}7` }));
  const body = commentBody({ journeys, failed: ['broken'], headSha: HEAD, base: 'main', runUrl: `${RUNS}7`, url: file => `https://raw.example/${file}` });
  assert.ok(body.startsWith(MARKER));
  assert.match(body, /merged with `main`/);
  assert.match(body, /\*\*Not recorded at `abcdef0`:\*\* broken\. See the \[workflow run\]\(https:\/\/github\.com\/o\/r\/actions\/runs\/7\)/);
  assert.match(body, /recorded at \[`abcdef0`\]\(https:\/\/github\.com\/o\/r\/actions\/runs\/7\)/);
  assert.match(body, /<a href="https:\/\/raw\.example\/reply-in-chat\.mp4"><img src="https:\/\/raw\.example\/reply-in-chat\.gif" alt="reply-in-chat" width="720">/);
  assert.match(body, /`0:00` A message arrives · `0:06` Reply/);
  journeys[0].files.preview = null;
  assert.match(commentBody({ journeys, headSha: HEAD, base: 'main', runUrl: '', url: file => file }), /preview is too large/);
});

test('the first publish commits the files under a new PR ref and creates the comment', async () => {
  await inFolder([recorded('reply-in-chat')], {}, async dir => {
    const github = fakeGitHub();
    await publish({ github, context: context(), core, dir, requested: 'reply-in-chat' });
    assert.equal(calls(github, 'createBlob').length, 3);
    const [tree] = calls(github, 'createTree');
    assert.deepEqual(tree.tree.map(item => item.path), ['reply-in-chat.gif', 'reply-in-chat.mp4', 'reply-in-chat-frames.png', 'manifest.json']);
    const stored = JSON.parse(tree.tree.at(-1).content);
    assert.deepEqual([stored[0].commit, stored[0].run], [HEAD, `${RUNS}7`]);
    assert.deepEqual(calls(github, 'createCommit')[0].parents, []);
    assert.equal(calls(github, 'createRef')[0].ref, 'refs/ui-videos/pr-12');
    const [comment] = calls(github, 'createComment');
    assert.equal(comment.issue_number, 12);
    assert.match(comment.body, /https:\/\/raw\.githubusercontent\.com\/o\/r\/c0ffee\/reply-in-chat\.gif/);
  });
});

test('a later publish keeps earlier videos in a fresh parentless commit and updates the bot comment', async () => {
  await inFolder([recorded('reply-in-chat')], {}, async dir => {
    const comments = [
      { id: 1, user: { login: 'someone', type: 'User' }, body: `${MARKER} copied` },
      { id: 2, user: { login: 'github-actions[bot]', type: 'Bot' }, body: `${MARKER}\nold` },
    ];
    const github = fakeGitHub({ comments, previous: [earlier('deleted-journey'), earlier('reply-in-chat')] });
    await publish({ github, context: context(), core, dir, requested: 'reply-in-chat' });
    assert.equal(calls(github, 'createBlob').length, 3, 'earlier files are reused, not uploaded again');
    const paths = calls(github, 'createTree')[0].tree.map(item => item.path);
    assert.ok(paths.includes('deleted-journey.mp4') && paths.includes('reply-in-chat.mp4'));
    const stored = JSON.parse(calls(github, 'createTree')[0].tree.at(-1).content);
    assert.deepEqual(stored.map(journey => [journey.name, journey.commit]), [['deleted-journey', OLD], ['reply-in-chat', HEAD]]);
    assert.deepEqual(calls(github, 'createCommit')[0].parents, []);
    assert.deepEqual([calls(github, 'updateRef')[0].ref, calls(github, 'updateRef')[0].force], ['ui-videos/pr-12', true]);
    assert.equal(paths.filter(path => path === 'reply-in-chat.mp4').length, 1, 'a superseded file is not listed twice');
    assert.deepEqual(calls(github, 'createRef'), []);
    assert.deepEqual(calls(github, 'createComment'), []);
    const [update] = calls(github, 'updateComment');
    assert.equal(update.comment_id, 2);
    assert.match(update.body, /#### deleted-journey/);
  });
});

test('a journey that did not record is reported and earlier videos stay', async () => {
  const github = fakeGitHub({ previous: [earlier('reply-in-chat')] });
  await publish({ github, context: context(), core, dir: '/nonexistent', requested: 'reply-in-chat' });
  assert.deepEqual(calls(github, 'createCommit'), []);
  const [comment] = calls(github, 'createComment');
  assert.match(comment.body, /Not recorded at `abcdef0`:\*\* reply-in-chat/);
  assert.match(comment.body, /raw\.githubusercontent\.com\/o\/r\/1111111111111111111111111111111111111111\/reply-in-chat\.gif/);
});

test('an oversized preview is left out and only the MP4 is linked', async () => {
  await inFolder([recorded('long')], { gif: Buffer.concat([GIF, Buffer.alloc(5 * 1024 * 1024)]) }, async dir => {
    const github = fakeGitHub();
    await publish({ github, context: context(), core, dir, requested: 'long' });
    assert.equal(calls(github, 'createBlob').length, 2);
    assert.match(calls(github, 'createComment')[0].body, /preview is too large/);
  });
});

test('publish does nothing for forks, a pull request that moved on, or an empty run', async () => {
  for (const [github, ctx] of [[fakeGitHub(), context('fork/r')], [fakeGitHub({ headSha: 'newer' }), context()], [fakeGitHub({ state: 'closed' }), context()]]) {
    await publish({ github, context: ctx, core, dir: '/nonexistent', requested: 'a' });
    assert.deepEqual(github.calls, []);
  }
  const github = fakeGitHub();
  await publish({ github, context: context(), core, dir: '/nonexistent', requested: '' });
  assert.deepEqual(github.calls, []);
});

test('publish refuses a file that is not what its name says, and odd requested names', async () => {
  await inFolder([recorded('reply-in-chat')], { mp4: GIF }, async dir => {
    const github = fakeGitHub();
    await assert.rejects(publish({ github, context: context(), core, dir, requested: 'reply-in-chat' }), /not the expected file type/);
    assert.ok(!github.calls.some(([method]) => ['createCommit', 'createComment'].includes(method)));
    await assert.rejects(publish({ github: fakeGitHub(), context: context(), core, dir, requested: 'a;rm' }), /Invalid journey names/);
  });
});

test('a new recording still fits when earlier videos already fill the comment', async () => {
  const full = Array.from({ length: 20 }, (_, index) => earlier(`old-${index}`));
  await inFolder([recorded('fresh')], {}, async dir => {
    const github = fakeGitHub({ previous: full });
    await publish({ github, context: context(), core, dir, requested: 'fresh' });
    const stored = JSON.parse(calls(github, 'createTree')[0].tree.at(-1).content);
    assert.equal(stored.length, 20);
    assert.ok(stored.some(journey => journey.name === 'fresh'));
    assert.ok(!stored.some(journey => journey.name === 'old-0'), 'the oldest earlier video is dropped');
    assert.ok(stored.some(journey => journey.name === 'old-19'));
  });
});
