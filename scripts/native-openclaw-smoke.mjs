#!/usr/bin/env node
// Local process/API integration only; no credentials, WeChat login or messages.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { mkdtemp, rm, readFile, realpath, stat, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, toNamespacedPath } from 'node:path';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { startNativeHost } from '../packages/openclaw-extension/src/native-host.ts';
const executable = resolve(process.argv[2] ?? 'packages/native-client-rust/target/debug/wx');
const directory = await mkdtemp(join(tmpdir(), 'native-openclaw-process-smoke-'));
let owned;
const previousToken = process.env.AGENT_WECHAT_TOKEN;
try {
  const token = randomBytes(32).toString('hex');
  process.env.AGENT_WECHAT_TOKEN = token;
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port; await new Promise(done => reservation.close(done));
  const config = { mode: 'managed', executable, accountId: 'smoke-a', dataDir: join(directory, 'data') + '/',
    token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(directory, 'consumer'), listen: `127.0.0.1:${port}`, readPolicy: 'never' };
  owned = await startNativeHost(config);
  const first = await owned.client.status();
  if (process.platform === 'linux') {
    assert.equal((await readFile(`/proc/${first.pid}/cmdline`, 'utf8')).includes(token), false);
    assert.equal((await readFile(`/proc/${first.pid}/environ`, 'utf8')).includes(token), false);
  }
  await assert.rejects(access(join(config.dataDir, 'api-token')));
  assert.equal(first.version.backend, 'native'); assert.equal(first.state, 'ready');
  const canonicalData = await realpath(join(directory, 'data'));
  assert.equal(first.dataDir, process.platform === 'win32' ? toNamespacedPath(canonicalData) : canonicalData);
  const a = await owned.client.createAccount('smoke-a'); const b = await owned.client.createAccount('smoke-b');
  const accounts = await owned.client.accounts(); assert.equal(accounts.items.length, 2);
  const account = await owned.client.account('smoke-a'); assert.equal(account.authenticated, false); assert.equal(account.connection, 'disconnected');
  const external = await startNativeHost({ mode: 'external', accountId: 'smoke-a', endpoint: `http://127.0.0.1:${port}`, token: config.token, consumerStateDir: config.consumerStateDir });
  await external.stop(); assert.equal((await owned.client.status()).pid, first.pid);
  await assert.rejects(startNativeHost(config), /native_owner_conflict/);
  await owned.stop(); owned = undefined;
  assert.ok((await stat(config.dataDir)).isDirectory());
  owned = await startNativeHost(config);
  const second = await owned.client.status();
  assert.equal(second.serverId, first.serverId); assert.notEqual(second.ownershipGeneration, first.ownershipGeneration);
  assert.equal((await owned.client.accounts()).items.length, 2);
  console.log('Native managed/external host smoke passed: env SecretRef, private credential pipe with no token file/argv/environment, owned PID/store identity, occupied-port rejection, account isolation, external stop preservation and restart persistence. No login or send occurred.');
} finally {
  if (previousToken === undefined) delete process.env.AGENT_WECHAT_TOKEN; else process.env.AGENT_WECHAT_TOKEN = previousToken;
  if (owned) await owned.stop();
  await rm(directory, { recursive: true, force: true });
}
