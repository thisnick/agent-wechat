import assert from 'node:assert/strict';
import test from 'node:test';
import { MARKER, commentBody, parseOutput, publish } from './claude-review-comment.mjs';

const HEAD = 'abcdef0123456789abcdef0123456789abcdef01';
const RUN = 'https://github.com/o/r/actions/runs/1';

test('parseOutput accepts a summary and a finding count', () => {
  assert.deepEqual(parseOutput(JSON.stringify({ summary: ' ## Claude review\n\nNone. ', findings: 0 })),
    { summary: '## Claude review\n\nNone.', findings: 0 });
});

test('parseOutput rejects missing, empty or malformed output', () => {
  for (const value of ['', undefined, 'not json', '{}', JSON.stringify({ summary: '  ', findings: 1 })])
    assert.equal(parseOutput(value), null);
});

test('parseOutput drops a bad count and strips the marker from the summary', () => {
  assert.deepEqual(parseOutput(JSON.stringify({ summary: `x ${MARKER} y`, findings: -1 })),
    { summary: 'x  y', findings: null });
});

test('a finished review posts the summary with the commit and run', () => {
  const body = commentBody({ output: { summary: 'Summary', findings: 1 }, headSha: HEAD, runUrl: RUN });
  assert.ok(body.startsWith(`${MARKER}\nSummary\n`));
  assert.match(body, new RegExp(`Commit \`${HEAD}\` · \\[workflow run\\]\\(${RUN}\\) · 1 finding<`));
});

test('an unfinished review keeps the previous summary below one notice', () => {
  const first = commentBody({ output: null, headSha: HEAD, runUrl: RUN, outcome: 'failure',
    previousBody: `${MARKER}\nOld summary\n` });
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
const output = JSON.stringify({ summary: 'New', findings: 0 });

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
