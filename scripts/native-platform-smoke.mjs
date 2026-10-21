#!/usr/bin/env node
// Actual local executable, synthetic empty accounts, no WeChat login or send.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { targetFor } from '../packages/cli/scripts/native-runtime.mjs';
const exec = promisify(execFile);
const executable = resolve(process.argv[2]);
const root = await mkdtemp(join(tmpdir(), 'wx platform smoke '));
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise(done => reservation.close(done));
const args = ['--data-dir', join(root, 'private data'), '--listen', `127.0.0.1:${port}`];
async function run(...command) {
  const { stdout } = await exec(executable, ['--json', ...args, ...command], { timeout: 35000, windowsHide: true });
  return JSON.parse(stdout);
}
let started = false;
try {
  assert.equal((await run('version')).target, targetFor().rustTarget);
  let serverId, generation;
  for (let i = 0; i < 2; i++) {
    const first = await run('daemon', 'start'); started = true;
    assert.equal(first.state, 'ready');
    assert.equal((await run('daemon', 'start')).pid, first.pid);
    assert.equal((await run('daemon', 'status')).pid, first.pid);
    if (serverId) { assert.equal(first.serverId, serverId); assert.notEqual(first.ownershipGeneration, generation); }
    serverId = first.serverId; generation = first.ownershipGeneration;
    const token = (await readFile(join(root, 'private data', 'api-token'), 'utf8')).trim();
    const base = `http://127.0.0.1:${port}`;
    assert.equal((await fetch(`${base}/v1/status`)).status, 401);
    const wrong = await fetch(`${base}/v1/daemon/shutdown`, { method: 'POST', headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: JSON.stringify({ pid: first.pid, serverId, ownershipGeneration: 'wrong generation' }) });
    assert.equal(wrong.status, 409); await wrong.body?.cancel();
    assert.equal((await run('daemon', 'status')).pid, first.pid);
    const stopped = await run('daemon', 'stop'); started = false;
    assert.equal(stopped.logout, false);
    assert.equal((await run('daemon', 'status')).state, 'stopped');
  }
  console.log(`Platform lifecycle passed on ${targetFor().rustTarget}: identity, duplicate start, authentication, stale generation refusal, graceful stop and restart persistence. No login or send occurred.`);
} finally {
  if (started) await run('daemon', 'stop');
  await rm(root, { recursive: true, force: true });
}
