#!/usr/bin/env node
// Actual pinned plugin/tool/intake runtime; all account and provider data is synthetic.
import { stagePluginRuntimeDependencies } from './native-smoke-plugin.mjs';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, readFile, copyFile, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const root = resolve(dirname(new URL(import.meta.url).pathname), '..');
const { build } = createRequire(join(root, 'packages/openclaw-extension/package.json'))('esbuild');
const sdk = await realpath(join(root, 'packages/openclaw-extension/node_modules/openclaw'));
assert.equal(JSON.parse(await readFile(join(sdk, 'package.json'), 'utf8')).version, '2026.10.7');
const temporary = await mkdtemp(join(tmpdir(), 'native-recall-model-'));
const token = 'synthetic-native-token-'.repeat(3), serverId = '18446744073709551614';
const messages = new Map(), journal = [], streams = new Set(), posts = [], sends = [], requests = [], acknowledgements = [];
let child, log = '', fixtureError, modelStep = 0, nativeRecalls = 0;
let releaseModel, modelStarted = false, releaseContent, contentStarted = false;
const modelHeld = new Promise(resolve => { releaseModel = resolve; });
const contentHeld = new Promise(resolve => { releaseContent = resolve; });
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const message = (id, body, extra = {}) => ({ id, accountId: 'a', threadId: 'wxid_fixture', serverId: String(9000 + messages.size), senderId: 'wxid_fixture', type: 1,
  direction: 'incoming', body, createdAt: '2026-10-21T12:00:00Z', version: 'v1', media: [],
  mentions: { state: 'unknown', userIds: [], self: null, all: null, source: null }, ...extra });
messages.set('msg_foreign', message('msg_foreign', 'Another thread', { direction: 'outgoing', senderId: 'self', threadId: 'other-peer', serverId: '99' }));
const emit = (kind, data) => {
  const event = { cursor: String(journal.length + 1), kind, data, createdAt: new Date().toISOString() };
  journal.push(event);
  for (const stream of streams) stream.write(`id: ${event.cursor}\nevent: ${kind}\ndata: ${JSON.stringify(event)}\n\n`);
  return event.cursor;
};
const commit = value => { messages.set(value.id, value); return emit('message.committed', { messageId: value.id, threadId: value.threadId, origin: 'live' }); };
const recall = (value, includeId = true) => {
  if (messages.get(value.id)?.serverId === value.serverId) messages.set(value.id, { ...value, type: 10000, direction: null, senderId: 'system', body: '[Message recalled]', quote: null, media: [],
    recall: { observedAt: '2026-10-21T12:01:00Z' } });
  return emit('message.recalled', { ...(includeId ? { messageId: value.id } : {}), threadId: value.threadId, serverId: value.serverId,
    observedAt: '2026-10-21T12:01:00Z', origin: 'live' });
};
const operation = state => ({ id: 'op_recall', accountId: 'a', kind: 'message_recall', state, result: state === 'succeeded'
  ? { messageId: 'msg_own', threadId: 'wxid_fixture', serverId, status: 'recalled', remoteOutcome: 'accepted', serviceCode: 0, retryable: false, profileProjection: 'complete' }
  : null, createdAt: '2026-10-21T12:00:00Z', updatedAt: '2026-10-21T12:00:01Z' });
const toolSteps = [
  ['tool_call', { id: 'message', args: { action: 'send', channel: 'agent-wechat', target: 'wxid_fixture', message: 'Synthetic agent-created recall target' } }],
  ['tool_search', { query: 'wechat_recall recall own sent WeChat message', limit: 4 }],
  ['tool_call', { id: 'wechat_recall', args: { messageId: 'msg_own', serverId } }],
  ['tool_call', { id: 'wechat_recall', args: { messageId: 'msg_own', serverId } }],
  ['tool_call', { id: 'wechat_recall', args: { messageId: 'msg_foreign', serverId: '99' } }],
];
const image = await readFile(join(root, 'packages/openclaw-extension/test/fixtures/native-model.png'));
const imageAsset = { id: 'asset_recalled', accountId: 'a', messageId: 'media-race', kind: 'image', state: 'available', variant: 'original', actualVariant: 'original',
  filename: 'recall-image.png', contentType: 'image/png', byteLength: image.length, sha256: createHash('sha256').update(image).digest('hex'), retryable: false };
const upstream = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://fixture');
    if (url.pathname.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
    const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
    if (url.pathname === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running',
      version: { product: 'agent-wechat', backend: 'native', version: '1.0.0', apiVersion: 1, capabilities: ['serve', 'events', 'profile-read', 'text-send', 'native-login', 'message-recall'] } });
    if (url.pathname === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
    if (url.pathname === '/v1/accounts/a/contacts') return json({ items: [{ id: 'wxid_fixture', username: 'wxid_fixture', displayName: 'Fixture user' }], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
    if (url.pathname === '/v1/accounts/a/groups') return json({ items: [], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
    if (url.pathname === '/v1/accounts/a/threads') return json({ items: [{ id: 'wxid_fixture', readStateVersion: '1' }], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
    if (url.pathname === '/v1/accounts/a/events') {
      const items = journal.filter(event => BigInt(event.cursor) > BigInt(url.searchParams.get('after') ?? '0')).slice(0, Number(url.searchParams.get('limit') ?? 100));
      return json({ items, nextCursor: items.at(-1)?.cursor ?? url.searchParams.get('after') ?? '0' });
    }
    if (url.pathname === '/v1/accounts/a/events/stream') {
      res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res);
      const after = BigInt(url.searchParams.get('after') ?? '0');
      for (const event of journal.filter(event => BigInt(event.cursor) > after)) res.write(`id: ${event.cursor}\nevent: ${event.kind}\ndata: ${JSON.stringify(event)}\n\n`);
      req.once('close', () => streams.delete(res)); return;
    }
    const lookup = /^\/v1\/accounts\/a\/messages\/([^/]+)$/.exec(url.pathname);
    if (lookup && messages.has(decodeURIComponent(lookup[1]))) return json(messages.get(decodeURIComponent(lookup[1])));
    if (url.pathname === '/v1/accounts/a/messages/media-race/media') return json({ items: [imageAsset] });
    if (url.pathname === '/v1/accounts/a/media/asset_recalled/content') {
      contentStarted = true; await contentHeld; res.setHeader('content-type', 'image/png'); res.end(image); return;
    }
    if (url.pathname === '/v1/accounts/a/operations/op_recall') return json(operation('succeeded'));
    let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 4 * 1024 * 1024); }
    if (url.pathname === '/v1/accounts/a/consumers/openclaw/ack') { acknowledgements.push(JSON.parse(raw).cursor); return json({}); }
    if (url.pathname.endsWith('/recall')) {
      assert.equal(req.method, 'POST'); assert.equal(url.pathname, '/v1/accounts/a/messages/msg_own/recall');
      assert.deepEqual(JSON.parse(raw), { serverId }); assert.match(req.headers['idempotency-key'], /^openclaw-recall-[a-f0-9]{64}$/);
      posts.push({ key: req.headers['idempotency-key'], body: JSON.parse(raw) });
      if (nativeRecalls === 0) { nativeRecalls++; recall(messages.get('msg_own')); }
      res.statusCode = 202; return json(operation(posts.length === 1 ? 'queued' : 'succeeded'));
    }
    if (url.pathname === '/v1/accounts/a/sends') {
      const input = JSON.parse(raw); sends.push(input);
      const sent = message(sends.length === 1 ? 'msg_own' : `msg_reply_${sends.length}`, input.text,
        { direction: 'outgoing', senderId: 'self', serverId: sends.length === 1 ? serverId : String(10000 + sends.length) });
      messages.set(sent.id, sent);
      return json({ id: `send_${sends.length}`, accountId: 'a', destination: 'wxid_fixture', state: 'accepted', localMessageId: sent.id, serverId: sent.serverId });
    }
    if (url.pathname === '/model/v1/chat/completions') {
      assert.equal(req.headers.authorization, 'Bearer synthetic-model-key'); const body = JSON.parse(raw); requests.push(body);
      const current = JSON.stringify([...body.messages].reverse().find(value => value.role === 'user'
        && !(typeof value.content === 'string' && value.content.startsWith('OpenClaw runtime context:'))));
      if (current.includes('runtime-recall-blocked-original')) { modelStarted = true; await modelHeld; }
      else assert.ok(current.includes('runtime-recall-tool-input') || current.includes('runtime-recall-replacement'), `Unexpected synthetic model input: ${current.slice(0, 1000)}`);
      const [name, args] = current.includes('runtime-recall-tool-input') ? toolSteps[modelStep] ?? [] : [];
      if (name) assert.ok(body.tools.some(tool => tool.function.name === name), `${name} is missing from the actual Gateway catalog`);
      if (modelStep === 1) {
        const results = JSON.stringify(body.messages.filter(value => value.role === 'tool'));
        assert.ok(results.includes('msg_own') && results.includes(serverId), `Verified message/server receipt missing from actual message-tool result: ${results.slice(-4000)}`);
      }
      if (modelStep === 2) assert.ok(JSON.stringify(body.messages).includes('wechat_recall'), 'Recall missing from actual discovery result');
      if (modelStep === 3 || modelStep === 4) assert.ok(JSON.stringify(body.messages).includes('recalled'), 'Confirmed recall receipt did not reach the provider');
      if (modelStep === 5 && current.includes('runtime-recall-tool-input')) assert.ok(JSON.stringify(body.messages).includes('outside this conversation'), 'Another thread was not rejected by the registered tool');
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const chunk = (delta, finish_reason) => `data: ${JSON.stringify({ id: `fixture-${requests.length}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-model', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
      if (name) { res.write(chunk({ role: 'assistant', tool_calls: [{ index: 0, id: `fixture-recall-${modelStep++}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] }, null)); res.write(chunk({}, 'tool_calls')); }
      else { if (current.includes('runtime-recall-tool-input')) modelStep++; res.write(chunk({ role: 'assistant', content: current.includes('runtime-recall-blocked-original') ? 'Late reply must be suppressed' : current.includes('runtime-recall-replacement') ? 'Fresh replacement delivered' : 'Synthetic recall completed' }, null)); res.write(chunk({}, 'stop')); }
      res.end('data: [DONE]\n\n'); return;
    }
    res.writeHead(404, { 'content-type': 'application/json' }); res.end('{"error":{"code":"fixture_not_found"}}');
  } catch (error) { fixtureError = error; if (!res.headersSent) res.writeHead(500); res.end('fixture failure'); }
});
async function waitFor(predicate, label, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    assert.ifError(fixtureError);
    if (child?.exitCode !== null || child?.signalCode) throw Error(`Gateway exited while waiting for ${label}: ${log}`);
    if (await predicate()) return;
    await sleep(50);
  }
  throw Error(`Deadline waiting for ${label}: ${log}`);
}
const custodyPath = join(temporary, 'consumer', createHash('sha256').update('fixture\0a').digest('hex') + '.json');
const custody = async () => JSON.parse(await readFile(custodyPath, 'utf8'));
const item = async id => { try { return (await custody()).items[id]; } catch { return undefined; } };
try {
  await mkdir(join(temporary, 'plugin/dist'), { recursive: true }); await mkdir(join(temporary, 'workspace'));
  for (const file of ['package.json', 'openclaw.plugin.json']) await copyFile(join(root, 'packages/openclaw-extension', file), join(temporary, 'plugin', file));
  await stagePluginRuntimeDependencies(root, join(temporary, 'plugin'));
  await build({ entryPoints: [join(root, 'packages/openclaw-extension/index.ts')], absWorkingDir: root, bundle: true, format: 'esm', platform: 'node', external: ['openclaw', 'ws'], outfile: join(temporary, 'plugin/dist/index.js') });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening'); const apiPort = upstream.address().port;
  const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening'); const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
  const config = { gateway: { mode: 'local', bind: 'loopback', port, auth: { mode: 'token', token: 'synthetic-gateway-token-'.repeat(3) }, controlUi: { enabled: false } }, browser: { enabled: false }, cron: { enabled: false }, discovery: { mdns: { mode: 'off' } }, update: { checkOnStart: false }, session: { dmScope: 'per-channel-peer' },
    models: { mode: 'merge', providers: { fixture: { baseUrl: `http://127.0.0.1:${apiPort}/model/v1`, api: 'openai-completions', apiKey: 'synthetic-model-key', models: [{ id: 'fixture-model', name: 'Fixture', reasoning: false, input: ['text', 'image'], contextWindow: 32768, maxTokens: 256, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } },
    agents: { defaults: { workspace: join(temporary, 'workspace'), model: { primary: 'fixture/fixture-model', fallbacks: [] } } }, tools: { media: { audio: { enabled: false }, image: { enabled: false }, video: { enabled: false } } },
    plugins: { allow: ['agent-wechat'], load: { paths: [join(temporary, 'plugin')] }, entries: { 'agent-wechat': { enabled: true, hooks: { allowConversationAccess: true }, config: { native: { mode: 'external', accountId: 'a', endpoint: `http://127.0.0.1:${apiPort}`, token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(temporary, 'consumer'), readPolicy: 'never' } } } } },
    channels: { 'agent-wechat': { enabled: true, dmPolicy: 'allowlist', allowFrom: ['wxid_fixture'], groupPolicy: 'allowlist', groupAllowFrom: ['wxid_fixture'] } } };
  const configPath = join(temporary, 'config.json'); await writeFile(configPath, JSON.stringify(config), { mode: 0o600 });
  child = spawn(process.execPath, [join(sdk, 'openclaw.mjs'), 'gateway', 'run'], { env: { PATH: process.env.PATH, HOME: temporary, TMPDIR: temporary, OPENCLAW_STATE_DIR: join(temporary, 'state'), OPENCLAW_CONFIG_PATH: configPath, AGENT_WECHAT_TOKEN: token, NO_COLOR: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk.toString()).slice(-131072); });
  await waitFor(() => streams.size > 0, 'actual channel SSE connection');
  commit(message('tool-trigger', 'runtime-recall-tool-input: recall the exact synthetic own message'));
  await waitFor(async () => (await item('tool-trigger'))?.state === 'completed', 'registered recall tool turn');
  assert.equal(modelStep, 6); assert.equal(posts.length, 2); assert.equal(nativeRecalls, 1); assert.equal(posts[0].key, posts[1].key);
  const completedSends = sends.length; assert.ok(completedSends >= 1 && completedSends <= 2); assert.equal(messages.get('msg_own').body, '[Message recalled]');

  const blocked = message('blocked-turn', 'runtime-recall-blocked-original'); commit(blocked);
  await waitFor(() => modelStarted, 'in-flight provider request');
  const recalledCursor = recall(blocked);
  await waitFor(() => acknowledgements.includes(recalledCursor), 'recall ACK while provider is blocked');
  const blockedCustody = await item(blocked.id);
  assert.equal(blockedCustody.message.body, '[Message recalled]'); assert.equal(blockedCustody.dispatchManifest, undefined);
  assert.equal(blockedCustody.invalidatedByRecall, true); assert.equal(blockedCustody.readPending, false);
  const replacement = { ...blocked, serverId: '18446744073709551610', body: 'runtime-recall-replacement' };
  const replacementCursor = commit(replacement);
  await waitFor(() => acknowledgements.includes(replacementCursor), 'replacement ACK while old provider is blocked');
  assert.equal((await item(blocked.id)).message.serverId, replacement.serverId);
  releaseModel();
  await waitFor(async () => (await item(blocked.id))?.state === 'completed', 'fresh exact source occupant dispatch');
  const replacementRequests = () => requests.filter(request => {
    const user = [...request.messages].reverse().find(value => value.role === 'user' && !(typeof value.content === 'string' && value.content.startsWith('OpenClaw runtime context:')));
    return JSON.stringify(user).includes('runtime-recall-replacement');
  });
  assert.equal(replacementRequests().length, 1, 'The actual host must admit one new exact source occupant');
  assert.equal(sends.length, completedSends + 1); assert.equal(sends.at(-1).text, 'Fresh replacement delivered');
  assert.ok(!sends.some(send => send.text === 'Late reply must be suppressed'));
  const duplicateCursor = commit({ ...replacement });
  await waitFor(() => acknowledgements.includes(duplicateCursor), 'same-target duplicate ACK'); await sleep(200);
  assert.equal(replacementRequests().length, 1); const afterReplacementSends = sends.length;

  const media = message('media-race', null, { type: 3 }); commit(media);
  await waitFor(() => contentStarted, 'held media content'); const mediaCursor = recall(media);
  await waitFor(() => acknowledgements.includes(mediaCursor), 'recall ACK while media is blocked');
  const beforeMedia = requests.length; releaseContent(); await sleep(300);
  assert.equal(requests.length, beforeMedia, 'Late recalled media must not start a model');
  assert.equal((await item(media.id)).message.body, '[Message recalled]'); assert.equal((await item(media.id)).dispatchManifest, undefined);

  const late = message('late-original', 'Late original must not be delivered', { serverId: '18446744073709551613' });
  recall(late, false); const lateCursor = commit(late);
  await waitFor(() => acknowledgements.includes(lateCursor), 'out-of-order original custody'); await sleep(200);
  assert.equal((await item(late.id)).state, 'ignored'); assert.equal((await item(late.id)).message.body, '[Message recalled]');
  assert.equal(requests.length, beforeMedia); assert.equal(sends.length, afterReplacementSends);
  const reused = message('reused-id', 'New exact occupant must remain', { direction: 'outgoing', senderId: 'self', serverId: '18446744073709551612' });
  const reuseCursor = commit(reused); await waitFor(() => acknowledgements.includes(reuseCursor), 'new occupant custody');
  const old = { ...reused, serverId: '18446744073709551611' }; const oldCursor = recall(old);
  await waitFor(() => acknowledgements.includes(oldCursor), 'recycled-ID recall ACK');
  assert.equal((await item(reused.id)).message.serverId, reused.serverId);
  assert.equal((await item(reused.id)).message.body, reused.body);
  const persisted = await readFile(custodyPath, 'utf8');
  assert.ok(!persisted.includes('runtime-recall-blocked-original')); assert.ok(!persisted.includes('Late original must not be delivered'));
  console.log(JSON.stringify({ gatewayVersion: '2026.10.7', actualRegisteredRecallTool: true, actualSendReceiptReachedProvider: true, exactServerIdPreserved: true, sameTargetStableKey: true,
    recallPostAttempts: posts.length, nativeRecallAttempts: nativeRecalls, foreignThreadRejected: true, sseRecallWhileModelBlocked: true,
    lateReplySuppressed: true, freshSourceOccupantDispatchedOnce: true, sseRecallWhileMediaBlocked: true, lateMediaSuppressed: true, outOfOrderRecallSuppressed: true, recycledIdRecallAckPreservesCurrent: true }));
} finally {
  releaseModel(); releaseContent();
  if (child && child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await Promise.race([once(child, 'exit'), sleep(5000)]); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await once(child, 'exit'); } }
  for (const stream of streams) stream.destroy(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve));
  await rm(temporary, { recursive: true, force: true });
}
