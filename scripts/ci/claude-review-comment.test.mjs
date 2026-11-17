import assert from 'node:assert/strict';
import test from 'node:test';
import { MARKER, commentBody, parseOutput, publish, scoreLine } from './claude-review-comment.mjs';

const HEAD = 'abcdef0123456789abcdef0123456789abcdef01';
const RUN = 'https://github.com/o/r/actions/runs/1';
const result = fields => ({ confidence: 8, important: 0, nits: 0, preexisting: 0, ...fields });

test('parseOutput accepts a summary, a confidence score and counts', () => {
  assert.deepEqual(
    parseOutput(JSON.stringify({ summary: ' Looks right. ', confidence: 8, important: 1, nits: 2, preexisting: 0 })),
    { summary: 'Looks right.', confidence: 8, important: 1, nits: 2, preexisting: 0 },
  );
});

test('parseOutput rejects missing, empty or malformed output', () => {
  for (const value of ['', undefined, 'not json', '{}', JSON.stringify({ summary: '  ', confidence: 5 })])
    assert.equal(parseOutput(value), null);
});

test('parseOutput drops out-of-range values and strips the marker from the summary', () => {
  assert.deepEqual(
    parseOutput(JSON.stringify({ summary: `x ${MARKER} y`, confidence: 11, important: -1, nits: 1.5 })),
    { summary: 'x  y', confidence: null, important: null, nits: null, preexisting: null },
  );
  assert.equal(parseOutput(JSON.stringify({ summary: 'x', confidence: 0 })).confidence, null);
});

test('scoreLine shows the score and the non-zero counts', () => {
  assert.equal(scoreLine(result({ confidence: 4, important: 1, nits: 2, preexisting: 1 })),
    '**Confidence: 4/10** · 1 important · 2 nits · 1 pre-existing');
  assert.equal(scoreLine(result({ nits: 1 })), '**Confidence: 8/10** · 1 nit');
});

test('scoreLine says plainly when there is nothing to fix', () => {
  assert.equal(scoreLine(result({ confidence: 9 })), '**Confidence: 9/10** · Nothing to fix');
});

test('scoreLine leaves out what it was not given', () => {
  assert.equal(scoreLine({ confidence: null, important: null, nits: null, preexisting: null }), '');
});

test('a finished review posts the heading, score, summary, commit and run', () => {
  const body = commentBody({ output: { ...result({ important: 1 }), summary: 'Summary' }, headSha: HEAD, runUrl: RUN });
  assert.equal(body, `${MARKER}\n## Claude review of \`abcdef0\`\n\n**Confidence: 8/10** · 1 important\n\n`
    + `Summary\n\n<sub>Commit \`${HEAD}\` · [workflow run](${RUN})</sub>\n`);
});

test('an unfinished review keeps the previous summary below one notice', () => {
  const first = commentBody({ output: null, headSha: HEAD, runUrl: RUN, outcome: 'failure',
    previousBody: `${MARKER}\n## Claude review of \`1234567\`\n\nOld summary\n` });
  assert.match(first, /did not finish\*\* \(failure\)/);
  assert.match(first, /Old summary/);
  const second = commentBody({ output: null, headSha: HEAD, runUrl: RUN, outcome: 'failure', previousBody: first });
  assert.equal(second.match(/did not finish/g).length, 1);
  assert.match(second, /Old summary/);
});

test('an unfinished first review posts only the notice', () => {
  const body = commentBody({ output: null, headSha: HEAD, runUrl: RUN, outcome: '' });
  assert.equal(body, `${MARKER}\n> **The Claude review of \`abcdef0\` did not finish** (unknown). See the [workflow run](${RUN}).\n`);
});

function fakeGithub(comments) {
  const calls = [];
  return {
    calls,
    paginate: async () => comments,
    rest: {
      issues: {
        listComments: {},
        updateComment: async args => calls.push(['update', args]),
        createComment: async args => calls.push(['create', args]),
      },
    },
  };
}

const context = { repo: { owner: 'o', repo: 'r' }, payload: { pull_request: { number: 7 } } };
const core = { warning() {} };
const output = JSON.stringify({ summary: 'New', confidence: 9, important: 0, nits: 0, preexisting: 0 });

test('publish updates the workflow bot comment and ignores look-alikes', async () => {
  const github = fakeGithub([
    { id: 1, user: { login: 'someone', type: 'User' }, body: `${MARKER}\nquoted` },
    { id: 2, user: { login: 'github-actions[bot]', type: 'Bot' }, body: `${MARKER}\nOld` },
  ]);
  await publish({ github, context, core, structuredOutput: output, outcome: 'success', headSha: HEAD, runUrl: RUN });
  assert.equal(github.calls.length, 1);
  assert.equal(github.calls[0][0], 'update');
  assert.equal(github.calls[0][1].comment_id, 2);
});

test('publish creates the comment on the first review', async () => {
  const github = fakeGithub([{ id: 1, user: { login: 'someone', type: 'User' }, body: 'hi' }]);
  await publish({ github, context, core, structuredOutput: output, outcome: 'success', headSha: HEAD, runUrl: RUN });
  assert.deepEqual(github.calls.map(([kind, args]) => [kind, args.issue_number]), [['create', 7]]);
});
