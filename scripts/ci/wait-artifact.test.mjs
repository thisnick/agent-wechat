import test from 'node:test';
import assert from 'node:assert/strict';
import { githubRun, producers, unstarted, waitForArtifact } from './wait-artifact.mjs';

const target = 'x86_64-pc-windows-msvc';
const cli = (status, conclusion = null) => ({ name: `portable / build (windows-latest, ${target})`, status, conclusion });

// A fake clock: sleeping advances time; `jobs` and `artifact` answer from a
// schedule keyed by the current time.
function harness({ jobs, artifact = () => false }) {
  let time = 0;
  const calls = { jobs: [], artifact: [] };
  const answer = value => { if (value instanceof Error) throw value; return value; };
  return {
    calls,
    options: {
      name: `native-release-${target}`,
      target,
      prefixes: ['build', 'portable / build'],
      listJobs: async () => { calls.jobs.push(time); return answer(jobs(time)); },
      hasArtifact: async () => { calls.artifact.push(time); return answer(artifact(time)); },
      sleep: async ms => { time += ms; },
      now: () => time,
      log: () => {},
      intervalMs: 30_000,
      timeoutMs: 600_000,
    },
  };
}

test('finds only the CLI job for this target, not desktop or test jobs', () => {
  const jobs = [
    { name: `portable / build (windows-latest, ${target})` },
    { name: `portable / platform-tests (windows-latest, ${target})` },
    { name: `desktop / build (windows-latest, ${target}, windows-x64)` },
    { name: 'portable / build (windows-11-arm, aarch64-pc-windows-msvc)' },
    { name: 'build (ubuntu-24.04, x64, x86_64-unknown-linux-gnu)' },
  ];
  assert.deepEqual(producers(jobs, ['build', 'portable / build'], target).map(job => job.name), [jobs[0].name]);
  assert.deepEqual(producers(jobs, ['build'], 'x86_64-unknown-linux-gnu').map(job => job.name), [jobs[4].name]);
  assert.deepEqual(producers([{ name: 'package (ubuntu-24.04, x64, linux/amd64, x86_64-unknown-linux-gnu)' }], ['package'], 'x86_64-unknown-linux-gnu').length, 1);
});

test('returns once the CLI job succeeds, asking for the artifact only then', async () => {
  const h = harness({
    jobs: time => [time < 90_000 ? cli('in_progress') : cli('completed', 'success')],
    artifact: time => time >= 90_000,
  });
  await waitForArtifact(h.options);
  assert.deepEqual(h.calls.jobs, [0, 30_000, 60_000, 90_000]);
  assert.deepEqual(h.calls.artifact, [90_000]);
});

for (const conclusion of ['failure', 'cancelled', 'skipped', 'timed_out']) {
  test(`stops at once when the CLI job ends with ${conclusion}`, async () => {
    const h = harness({ jobs: time => [time < 30_000 ? cli('in_progress') : cli('completed', conclusion)] });
    await assert.rejects(waitForArtifact(h.options), new RegExp(`ended with ${conclusion}`));
    assert.deepEqual(h.calls.jobs, [0, 30_000]);
    assert.deepEqual(h.calls.artifact, []);
  });
}

test('stops at once when a CLI job was skipped or failed before its matrix expanded', async () => {
  for (const name of ['portable', 'build', 'portable / build']) {
    const h = harness({ jobs: () => [{ name, status: 'completed', conclusion: 'skipped' }] });
    await assert.rejects(waitForArtifact(h.options), new RegExp(`"${name}" ended with skipped`));
    assert.equal(h.calls.jobs.length, 1);
  }
  const ignored = [
    { name: 'desktop / build', status: 'completed', conclusion: 'skipped' },
    { name: 'portable', status: 'in_progress', conclusion: null },
    { name: 'build', status: 'completed', conclusion: 'success' },
  ];
  assert.deepEqual(unstarted(ignored, ['build', 'portable / build']), []);
});

test('fails at once on a permission or not-found response', async () => {
  for (const status of [401, 403, 404]) {
    const h = harness({ jobs: () => { throw Object.assign(new Error(`HTTP ${status}`), { status }); } });
    await assert.rejects(waitForArtifact(h.options), /actions: read/);
    assert.equal(h.calls.jobs.length, 1);
  }
});

test('waits out a rate limit until the time GitHub gives', async () => {
  const limited = Object.assign(new Error('HTTP 403'), { status: 403, retryAt: 300_000 });
  const h = harness({ jobs: time => (time < 300_000 ? limited : [cli('completed', 'success')]), artifact: () => true });
  await waitForArtifact(h.options);
  assert.deepEqual(h.calls.jobs, [0, 300_000]);
});

test('fails clearly when no job matching the given names ever appears', async () => {
  const h = harness({ jobs: () => [{ name: 'checks / native (ubuntu-24.04, test)', status: 'in_progress' }] });
  await assert.rejects(waitForArtifact({ ...h.options, unmatchedMs: 90_000 }), /No job named "build \(\.\.\.\)" or "portable \/ build \(\.\.\.\)" builds/);
  assert.deepEqual(h.calls.jobs, [0, 30_000, 60_000, 90_000]);
});

test('a re-run that reuses an earlier attempt\'s CLI accepts its artifact', async () => {
  const h = harness({ jobs: () => [], artifact: () => true });
  await waitForArtifact(h.options);
  assert.equal(h.calls.jobs.length, 1);
});

test('keeps waiting while a re-run rebuilds a CLI that failed before', async () => {
  const failed = cli('completed', 'failure');
  const h = harness({
    jobs: time => [failed, time < 60_000 ? cli('in_progress') : cli('completed', 'success')],
    artifact: () => true,
  });
  await waitForArtifact(h.options);
  assert.deepEqual(h.calls.artifact, [60_000]);
});

test('waits for a CLI job that has not been created yet', async () => {
  const h = harness({ jobs: time => (time < 60_000 ? [] : [cli('completed', 'success')]), artifact: time => time >= 60_000 });
  await waitForArtifact(h.options);
  assert.deepEqual(h.calls.artifact, [0, 30_000, 60_000]);
});

test('fails when the CLI job succeeded but its artifact never appears', async () => {
  const h = harness({ jobs: () => [cli('completed', 'success')] });
  await assert.rejects(waitForArtifact(h.options), /succeeded without uploading/);
  assert.equal(h.calls.artifact.length, 3);
});

test('keeps waiting through API errors', async () => {
  const h = harness({
    jobs: time => (time < 60_000 ? new Error('HTTP 502') : [cli('completed', 'success')]),
    artifact: () => true,
  });
  await waitForArtifact(h.options);
  assert.equal(h.calls.jobs.length, 3);
});

test('gives up at the deadline without sleeping past it', async () => {
  const h = harness({ jobs: () => [cli('queued')] });
  await assert.rejects(waitForArtifact({ ...h.options, timeoutMs: 45_000 }), /not uploaded within 1 minutes/);
  assert.deepEqual(h.calls.jobs, [0, 30_000, 45_000]);
});

test('reads every page of the current attempt\'s jobs and looks up the artifact by name', async () => {
  const requests = [];
  const responses = [
    { total_count: 101, jobs: Array.from({ length: 100 }, (_, i) => ({ name: `job ${i}` })) },
    { total_count: 101, jobs: [{ name: 'last' }] },
    { total_count: 2, artifacts: [{ name: 'native-release-x', expired: true }, { name: 'native-release-x', expired: false }] },
    { total_count: 1, artifacts: [{ name: 'native-release-x', expired: true }] },
  ];
  const run = githubRun({
    api: 'https://api.example.test', repository: 'owner/repo', runId: '42', attempt: '3', token: 'token-value',
    fetchImpl: async (url, init) => {
      requests.push({ url, authorization: init.headers.authorization });
      return { ok: true, json: async () => responses[requests.length - 1] };
    },
  });
  assert.equal((await run.listJobs()).length, 101);
  assert.equal(await run.hasArtifact('native-release-x'), true);
  assert.equal(await run.hasArtifact('native-release-x'), false);
  assert.deepEqual(requests.map(r => r.url), [
    'https://api.example.test/repos/owner/repo/actions/runs/42/attempts/3/jobs?per_page=100&page=1',
    'https://api.example.test/repos/owner/repo/actions/runs/42/attempts/3/jobs?per_page=100&page=2',
    'https://api.example.test/repos/owner/repo/actions/runs/42/artifacts?name=native-release-x&per_page=100',
    'https://api.example.test/repos/owner/repo/actions/runs/42/artifacts?name=native-release-x&per_page=100',
  ]);
  assert.equal(requests[0].authorization, 'Bearer token-value');
});

test('reports a refused request with its status and any retry time', async () => {
  const headers = values => ({ get: name => values[name] });
  const refuse = (status, values = {}) => githubRun({
    api: 'https://api.example.test', repository: 'owner/repo', runId: '42', attempt: '1', token: 't', clock: () => 1_000,
    fetchImpl: async () => ({ ok: false, status, headers: headers(values) }),
  });
  await assert.rejects(refuse(403).listJobs(), error => error.status === 403 && error.retryAt === undefined);
  await assert.rejects(refuse(403, { 'x-ratelimit-remaining': '0', 'x-ratelimit-reset': '50' }).listJobs(), error => error.retryAt === 50_000);
  await assert.rejects(refuse(403, { 'retry-after': '30' }).listJobs(), error => error.retryAt === 31_000);
  await assert.rejects(refuse(429).listJobs(), error => error.retryAt === 61_000);
  await assert.rejects(refuse(502).listJobs(), error => error.status === 502 && error.retryAt === undefined);
});
