import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { validReport, publish } from './publish-comment.mjs';
const marker = '<!-- agent-wechat-interfaces -->';
const report = { number: 441, head: 'abc123', changed: true, body: `${marker}\nReport` };
const run = { id: 1, workflow_id: 2, run_attempt: 1, event: 'pull_request', head_sha: 'abc123', head_branch: 'feature', head_repository: { id: 7 }, html_url: 'https://github.com/owner/repo/actions/runs/1', pull_requests: [] };
const pr = { number: 441, state: 'open', base: { repo: { full_name: 'owner/repo' } }, head: { sha: 'abc123', ref: 'feature', repo: { id: 7 } } };

test('fork reports with no run PR association are accepted only for the matching open head', () => {
  assert.equal(validReport(report, run, pr, 'owner/repo'), true);
  for (const bad of [
    { ...pr, state: 'closed' }, { ...pr, number: 442 },
    { ...pr, base: { repo: { full_name: 'other/repo' } } },
    ...[{ sha: 'new' }, { ref: 'other' }, { repo: { id: 8 } }, { repo: null }]
      .map(head => ({ ...pr, head: { ...pr.head, ...head } })),
  ]) assert.equal(validReport(report, run, bad, 'owner/repo'), false);
  for (const bad of [{ ...report, head: 'new' }, { ...report, body: 'no marker' },
    { ...report, body: marker + 'x'.repeat(60000) }, { ...report, changed: 'yes' }])
    assert.equal(validReport(bad, run, pr, 'owner/repo'), false);
});

async function harness(t, { comments = [], changed = true, currentAttempt = 1, currentPr = pr, workflowPath = '.github/workflows/interfaces.yml' } = {}) {
  const dir = await mkdtemp(join(tmpdir(), 'interface-comment-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const path = join(dir, 'report.json');
  await writeFile(path, JSON.stringify({ ...report, changed }));
  const writes = [];
  const github = {
    rest: {
      actions: {
        getWorkflow: async () => ({ data: { path: workflowPath } }),
        getWorkflowRun: async () => ({ data: { run_attempt: currentAttempt } }),
      },
      pulls: { get: async () => ({ data: currentPr }) },
      issues: {
        listComments: () => {},
        createComment: async args => writes.push({ action: 'create', ...args }),
        updateComment: async args => writes.push({ action: 'update', ...args }),
      },
    },
    paginate: async () => comments,
  };
  return { writes, execute: () => publish({ github, context: { repo: { owner: 'owner', repo: 'repo' }, payload: { workflow_run: run } }, core: { info() {} }, path }) };
}

test('posts a fork report with source attribution even if the producer had failed checks', async t => {
  const h = await harness(t);
  await h.execute();
  assert.equal(h.writes[0].issue_number, 441);
  assert.match(h.writes[0].body, /abc123/);
  assert.match(h.writes[0].body, /workflow run/);
});
test('updates only the Actions bot comment, ignoring a user-supplied marker', async t => {
  const h = await harness(t, { comments: [
    { id: 10, body: marker, user: { login: 'someone', type: 'User' } },
    { id: 11, body: marker, user: { login: 'github-actions[bot]', type: 'Bot' } },
  ] });
  await h.execute();
  assert.equal(h.writes[0].action, 'update');
  assert.equal(h.writes[0].comment_id, 11);
});
test('does not post obsolete attempts, stale heads, or a new unchanged report', async t => {
  for (const options of [{ currentAttempt: 2 }, { currentPr: { ...pr, head: { ...pr.head, sha: 'new' } } }, { changed: false }]) {
    const h = await harness(t, options);
    await h.execute();
    assert.deepEqual(h.writes, []);
  }
});
test('rejects a report from another producer workflow', async t => {
  const h = await harness(t, { workflowPath: '.github/workflows/other.yml' });
  await assert.rejects(h.execute, /Unexpected producer/);
  assert.deepEqual(h.writes, []);
});
