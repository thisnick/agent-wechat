import test from 'node:test';
import assert from 'node:assert/strict';
import { failureName, runArtifacts, waitForArtifact } from './wait-artifact.mjs';

// A fake clock: sleeping advances time, and `list` answers from a schedule.
function harness(schedule) {
  let time = 0;
  const calls = [];
  return {
    calls,
    options: {
      list: async () => {
        calls.push(time);
        const step = schedule(time);
        if (step instanceof Error) throw step;
        return step;
      },
      sleep: async ms => { time += ms; },
      now: () => time,
      log: () => {},
      intervalMs: 30_000,
    },
  };
}

test('returns once the CLI artifact is uploaded', async () => {
  const h = harness(time => (time >= 90_000 ? [{ name: 'native-release-x', expired: false }] : []));
  await waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '1', timeoutMs: 600_000 });
  assert.deepEqual(h.calls, [0, 30_000, 60_000, 90_000]);
});

test("stops at once when this attempt's failure marker appears", async () => {
  const h = harness(() => [{ name: failureName('native-release-x', '2'), expired: false }]);
  await assert.rejects(
    waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '2', timeoutMs: 600_000 }),
    /failed/,
  );
  assert.equal(h.calls.length, 1);
});

test("a marker from an earlier attempt does not stop a re-run", async () => {
  const marker = { name: failureName('native-release-x', '1'), expired: false };
  const h = harness(time => (time >= 30_000 ? [marker, { name: 'native-release-x', expired: false }] : [marker]));
  await waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '2', timeoutMs: 600_000 });
  assert.equal(h.calls.length, 2);
});

test('ignores expired artifacts and similar names', async () => {
  const h = harness(() => [
    { name: 'native-release-x', expired: true },
    { name: 'native-release-x86', expired: false },
  ]);
  await assert.rejects(
    waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '1', timeoutMs: 60_000 }),
    /not uploaded within 1 minutes/,
  );
});

test('keeps waiting through API errors until the deadline', async () => {
  const h = harness(time => (time < 60_000 ? new Error('HTTP 502') : [{ name: 'native-release-x', expired: false }]));
  await waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '1', timeoutMs: 600_000 });
  assert.equal(h.calls.length, 3);
});

test('gives up at the deadline without sleeping past it', async () => {
  const h = harness(() => []);
  await assert.rejects(
    waitForArtifact({ ...h.options, name: 'native-release-x', attempt: '1', timeoutMs: 45_000 }),
    /not uploaded/,
  );
  assert.deepEqual(h.calls, [0, 30_000, 45_000]);
});

test('lists every page of the run artifacts with the job token', async () => {
  const requests = [];
  const pages = [
    { total_count: 101, artifacts: Array.from({ length: 100 }, (_, i) => ({ name: `a${i}` })) },
    { total_count: 101, artifacts: [{ name: 'last' }] },
  ];
  const list = runArtifacts({
    api: 'https://api.example.test', repository: 'owner/repo', runId: '42', token: 'token-value',
    fetchImpl: async (url, init) => {
      requests.push({ url, authorization: init.headers.authorization });
      return { ok: true, json: async () => pages[requests.length - 1] };
    },
  });
  const artifacts = await list();
  assert.equal(artifacts.length, 101);
  assert.deepEqual(requests.map(r => r.url), [
    'https://api.example.test/repos/owner/repo/actions/runs/42/artifacts?per_page=100&page=1',
    'https://api.example.test/repos/owner/repo/actions/runs/42/artifacts?per_page=100&page=2',
  ]);
  assert.equal(requests[0].authorization, 'Bearer token-value');
});

test('reports a failed API response', async () => {
  const list = runArtifacts({
    api: 'https://api.example.test', repository: 'owner/repo', runId: '42', token: 't',
    fetchImpl: async () => ({ ok: false, status: 403 }),
  });
  await assert.rejects(list(), /HTTP 403/);
});
