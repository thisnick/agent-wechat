#!/usr/bin/env node
// Real pinned Gateway, per-agent policies and provider tool calls; all data is synthetic.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, copyFile, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const root = resolve(dirname(new URL(import.meta.url).pathname), '..');
const { build } = createRequire(join(root, 'packages/openclaw-extension/package.json'))('esbuild');
const sdk = await realpath(join(root, 'packages/openclaw-extension/node_modules/openclaw'));
assert.equal(JSON.parse(await readFile(join(sdk, 'package.json'), 'utf8')).version, '2026.10.2');
const temporary = await mkdtemp(join(tmpdir(), 'native-archive-policy-'));
const token = 'synthetic-native-token-'.repeat(3), streams = new Set(), requests = [], accesses = [];
const roles = ['ungranted', 'ordinary', 'privileged'], inputRechecks = new Set();
const stages = { ungranted: 0, ordinary: 0, privileged: 0 };
let child, fixtureError, log = '', activeRole, eventReplayReads = 0;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const image = await readFile(join(root, 'packages/openclaw-extension/test/fixtures/native-model.png'));
const file = Buffer.from('Synthetic archive file');
const assets = new Map([['archive-image', { bytes: image, kind: 'image', mime: 'image/png' }], ['archive-file', { bytes: file, kind: 'file', mime: 'text/plain' }]]);
const page = (items, nextCursor = null) => ({ items, nextCursor, sourceGeneration: 'fixture', coverage: { history: 'partial' } });
const message = (id, threadId = 'private-peer') => ({ id, accountId: 'a', threadId, senderId: 'historical-sender-not-admitted', serverId: '9007199254740999',
  direction: 'incoming', type: id === 'archive-image' ? 3 : id === 'archive-file' ? 49 : 1, body: id.startsWith('archive-') ? '<private transport XML>' : `project note ${id}`, version: 'v1', media: [] });
const deniedSteps = [
  ['tool_search', { query: 'wechat_archive_search', limit: 10 }],
  ['tool_call', { id: 'wechat_archive_search', args: { query: 'project' } }],
  ['tool_call', { id: 'wechat_archive_message', args: { messageId: 'archive-image' } }],
];
const allowedSteps = [
  ['tool_search', { query: 'wechat_archive_search', limit: 1 }],
  ['tool_call', { id: 'wechat_archive_threads', args: { query: 'project' } }],
  ['tool_call', { id: 'wechat_archive_search', args: { query: 'project', limit: 1 } }],
  ['tool_call', { id: 'wechat_archive_search', args: { query: 'project', limit: 1, cursor: 'archive-next' } }],
  ['tool_call', { id: 'wechat_archive_history', args: { threadId: 'private-peer', limit: 2 } }],
  ['tool_call', { id: 'wechat_archive_message', args: { messageId: 'archive-file' } }],
  ['tool_call', { id: 'wechat_message', args: { messageId: 'archive-image' } }],
  ['wechat_media', { messageId: 'archive-image' }],
  ['wechat_archive_media', { messageId: 'archive-image', download: false }],
  ['wechat_archive_media', { messageId: 'archive-file', download: false }],
];
const upstream = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://fixture');
    const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
    if (url.pathname.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
    if (url.pathname === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running',
      version: { product: 'agent-wechat', backend: 'native', version: '1.0.0', apiVersion: 1, capabilities: ['serve', 'events', 'profile-read', 'text-send', 'native-login'] } });
    if (url.pathname === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
    if (url.pathname === '/v1/accounts/a/events') {
      assert.equal(req.method, 'GET'); eventReplayReads++;
      return json({ items: [], nextCursor: url.searchParams.get('after') ?? '0' });
    }
    if (url.pathname === '/v1/accounts/a/events/stream') {
      assert.ok(eventReplayReads > 0, 'Channel must replay durable events before opening its live stream');
      res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res); req.once('close', () => streams.delete(res)); return;
    }
    if (url.pathname === '/v1/accounts/a/threads') {
      if (!url.searchParams.has('query')) return json(page([]));
      accesses.push({ role: activeRole, kind: 'threads' });
      return json(page([{ id: 'private-peer', accountId: 'a', peerId: 'private-peer', displayName: 'Project notes', kind: 'direct', unreadCount: 5 }]));
    }
    if (url.pathname === '/v1/accounts/a/messages') {
      accesses.push({ role: activeRole, kind: 'search', query: Object.fromEntries(url.searchParams) });
      return json(url.searchParams.has('cursor') ? page([message('older-match', 'other@chatroom')]) : page([message('first-match')], 'archive-next'));
    }
    if (url.pathname === '/v1/accounts/a/threads/private-peer/messages') {
      accesses.push({ role: activeRole, kind: 'history' }); return json(page([message('archive-image'), message('archive-file')]));
    }
    const trigger = /^\/v1\/accounts\/a\/messages\/trigger-(ungranted|ordinary|privileged)$/.exec(url.pathname);
    if (trigger) {
      assert.equal(req.method, 'GET'); const role = trigger[1]; inputRechecks.add(role);
      return json({ ...message(`trigger-${role}`, `${role}-peer`), senderId: `${role}-peer`,
        serverId: String(roles.indexOf(role) + 1), body: `archive-case:${role}` });
    }
    const exact = /^\/v1\/accounts\/a\/messages\/(archive-image|archive-file)$/.exec(url.pathname);
    if (exact) { accesses.push({ role: activeRole, kind: 'message', id: exact[1] }); return json(message(exact[1])); }
    const media = /^\/v1\/accounts\/a\/messages\/(archive-image|archive-file)\/media$/.exec(url.pathname);
    if (media) {
      accesses.push({ role: activeRole, kind: 'media', id: media[1] });
      const value = assets.get(media[1]);
      return json({ items: [{ id: `asset-${media[1]}`, accountId: 'a', messageId: media[1], kind: value.kind, variant: value.kind === 'image' ? 'original' : 'normal',
        actualVariant: value.kind === 'image' ? 'original' : 'normal', state: 'available', contentType: value.mime, byteLength: value.bytes.length,
        sha256: createHash('sha256').update(value.bytes).digest('hex'), filename: `${media[1]}.${value.mime.split('/')[1]}`, retryable: false }] });
    }
    const content = /^\/v1\/accounts\/a\/media\/asset-(archive-image|archive-file)\/content$/.exec(url.pathname);
    if (content) { const value = assets.get(content[1]); accesses.push({ role: activeRole, kind: 'content', id: content[1] }); res.setHeader('content-type', value.mime); res.end(value.bytes); return; }
    let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 8 * 1024 * 1024); }
    if (url.pathname === '/v1/accounts/a/sends') return json({ id: 'fixture-send', state: 'accepted' });
    if (url.pathname === '/model/v1/chat/completions') {
      const body = JSON.parse(raw), role = /archive-case:(ungranted|ordinary|privileged)/.exec(JSON.stringify(body.messages))?.[1];
      assert.ok(role); activeRole = role; requests.push({ role, body });
      const steps = role === 'privileged' ? allowedSteps : deniedSteps, stage = stages[role]++;
      const [toolName, args] = steps[stage] ?? [];
      const toolResults = body.messages.filter(item => item.role === 'tool');
      if (role !== 'privileged') {
        assert.ok(!body.tools.some(tool => tool.function.name.startsWith('wechat_archive_')), 'Denied or unselected archive tool exposed directly');
        if (stage === 1) assert.ok(JSON.parse(toolResults.at(-1).content).every(tool => !tool.name.startsWith('wechat_archive_')), 'Archive tool appeared in discovery');
        if (stage >= 2) assert.match(JSON.stringify(toolResults.at(-1)?.content), /unknown|not found|unavailable|not available/i, 'Guessed archive name was not rejected');
      } else {
        assert.ok(body.tools.some(tool => tool.function.name === 'wechat_archive_media'), 'Granted media tool missing');
        if (stage === 1) assert.equal(JSON.parse(toolResults.at(-1).content)[0]?.name, 'wechat_archive_search', 'Granted search not discoverable');
        if (stage === 4) assert.ok(JSON.stringify(toolResults).includes('other@chatroom'), 'Cross-thread continuation did not reach provider');
        if (stage === 8) assert.ok(JSON.stringify(toolResults.at(-1)?.content).includes('outside the permitted conversation'), 'Default media tool widened for a privileged agent');
      }
      if (toolName) assert.ok(body.tools.some(tool => tool.function.name === toolName), `Missing ${toolName}`);
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const chunk = (delta, finishReason) => `data: ${JSON.stringify({ id: `fixture-${requests.length}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-model',
        choices: [{ index: 0, delta, finish_reason: finishReason }] })}\n\n`;
      if (toolName) {
        res.write(chunk({ role: 'assistant', tool_calls: [{ index: 0, id: `${role}-${stage}`, type: 'function', function: { name: toolName, arguments: JSON.stringify(args) } }] }, null));
        res.write(chunk({}, 'tool_calls'));
      } else { res.write(chunk({ role: 'assistant', content: 'Archive policy validation complete.' }, null)); res.write(chunk({}, 'stop')); }
      res.end('data: [DONE]\n\n'); return;
    }
    throw Error(`Unexpected fixture endpoint: ${req.method} ${url.pathname}`);
  } catch (error) { fixtureError = error; log += `\nFixture upstream: ${error}`; if (!res.headersSent) res.writeHead(500); res.end('fixture failure'); }
});
try {
  await mkdir(join(temporary, 'plugin/dist'), { recursive: true }); await mkdir(join(temporary, 'workspace'));
  for (const name of ['package.json', 'openclaw.plugin.json']) await copyFile(join(root, 'packages/openclaw-extension', name), join(temporary, 'plugin', name));
  await build({ entryPoints: [join(root, 'packages/openclaw-extension/test/fixtures/native-archive-model-plugin.ts')], absWorkingDir: root, bundle: true, format: 'esm', platform: 'node', external: ['openclaw'], outfile: join(temporary, 'plugin/dist/index.js') });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening'); const apiPort = upstream.address().port;
  const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening'); const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
  const native = { mode: 'external', accountId: 'a', endpoint: `http://127.0.0.1:${apiPort}`, token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(temporary, 'consumer'), readPolicy: 'never' };
  const config = {
    gateway: { mode: 'local', bind: 'loopback', port, auth: { mode: 'token', token: 'synthetic-gateway-token-'.repeat(3) }, controlUi: { enabled: false } },
    browser: { enabled: false }, cron: { enabled: false }, discovery: { mdns: { mode: 'off' } }, update: { checkOnStart: false }, session: { dmScope: 'per-channel-peer' },
    models: { mode: 'merge', providers: { fixture: { baseUrl: `http://127.0.0.1:${apiPort}/model/v1`, api: 'openai-completions', apiKey: 'synthetic-model-key', models: [{ id: 'fixture-model', name: 'Fixture', reasoning: false, input: ['text', 'image'], contextWindow: 32768, maxTokens: 256, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } },
    agents: { ownership: 'explicit', defaults: { workspace: join(temporary, 'workspace'), model: { primary: 'fixture/fixture-model', fallbacks: [] } }, entries: {
      ungranted: {}, ordinary: { tools: { profile: 'full', deny: ['wechat_archive_*'] } }, privileged: { tools: { alsoAllow: ['wechat_archive_*'] } },
    } },
    bindings: roles.map(role => ({ agentId: role, match: { channel: 'agent-wechat', peer: { kind: 'direct', id: `${role}-peer` } } })),
    plugins: { allow: ['agent-wechat'], load: { paths: [join(temporary, 'plugin')] }, entries: { 'agent-wechat': { enabled: true, config: { native } } } },
    channels: { 'agent-wechat': { enabled: true, dmPolicy: 'allowlist', allowFrom: roles.map(role => `${role}-peer`), groupPolicy: 'disabled' } },
  };
  const configPath = join(temporary, 'config.json'); await writeFile(configPath, JSON.stringify(config), { mode: 0o600 });
  child = spawn(process.execPath, [join(sdk, 'openclaw.mjs'), 'gateway', 'run'], { env: { PATH: process.env.PATH, HOME: temporary, TMPDIR: temporary, OPENCLAW_STATE_DIR: join(temporary, 'state'), OPENCLAW_CONFIG_PATH: configPath, NATIVE_ARCHIVE_FIXTURE_DIR: temporary, AGENT_WECHAT_TOKEN: token, NO_COLOR: '1' }, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk.toString()).slice(-131072); });
  let result;
  for (let attempt = 0; attempt < 1200; attempt++) {
    if (fixtureError) break;
    if (child.exitCode !== null || child.signalCode) throw Error(`Gateway exited: ${log}`);
    try { result = JSON.parse(await readFile(join(temporary, 'result.json'), 'utf8')); break; } catch { await sleep(100); }
  }
  if (process.env.NATIVE_ARCHIVE_PROOF_DIR) {
    await mkdir(process.env.NATIVE_ARCHIVE_PROOF_DIR, { recursive: true, mode: 0o700 });
    await writeFile(join(process.env.NATIVE_ARCHIVE_PROOF_DIR, 'observations.json'), JSON.stringify({ result, requests, accesses, log }), { mode: 0o600 });
  }
  assert.ifError(fixtureError); assert.ok(result, `Gateway deadline: ${log}`); assert.equal(result.error, undefined, `${result.error}; ${log}`);
  assert.ok(eventReplayReads > 0 && streams.size > 0, 'Channel event replay and live intake must be active');
  assert.deepEqual([...inputRechecks].sort(), [...roles].sort(), 'Every dispatched role must recheck its current input');
  for (const role of roles) { assert.equal(result.outcomes[role], 'completed'); assert.equal(stages[role], (role === 'privileged' ? allowedSteps : deniedSteps).length + 1); }
  assert.ok(accesses.length > 0); assert.ok(accesses.every(access => access.role === 'privileged'), 'Unprivileged invocation reached the archive API');
  assert.deepEqual(accesses.filter(access => access.kind === 'search').map(access => access.query.cursor), [undefined, 'archive-next']);
  assert.deepEqual(accesses.filter(access => access.kind === 'media').map(access => access.id), ['archive-image', 'archive-file']);
  const imageBlocks = requests.filter(request => request.role === 'privileged').flatMap(request => request.body.messages.flatMap(message => Array.isArray(message.content) ? message.content.filter(block => block.type === 'image_url') : []));
  assert.ok(imageBlocks.some(block => createHash('sha256').update(Buffer.from(block.image_url.url.split(',')[1], 'base64')).digest('hex') === createHash('sha256').update(image).digest('hex')), 'Archive image bytes did not reach provider vision input');
  const fileResult = JSON.parse(requests.at(-1).body.messages.filter(message => message.role === 'tool').at(-1).content);
  assert.equal(fileResult.messageId, 'archive-file'); assert.equal(fileResult.threadId, 'private-peer'); assert.equal(fileResult.status, 'ready');
  assert.ok(fileResult.attachment.path.startsWith(join(temporary, 'state/media') + '/'), 'Archive file path must be in managed media storage');
  assert.deepEqual(await readFile(fileResult.attachment.path), file, 'Provider file path must resolve to the verified attachment bytes');
  assert.ok(!JSON.stringify(requests).includes('private transport XML'));
  console.log(JSON.stringify({ gatewayVersion: '2026.10.2', startupEventReplay: true, sourceInputRechecks: true, optionalToolsAbsentWithoutGrant: true, denyOverridesFullProfile: true, guessedArchiveCallsRejected: true,
    accountWideSearchAndPagination: true, otherSenderHistoryAndMedia: true, defaultToolsRemainCurrentThread: true, archiveImageHashReachedProvider: true, archiveFilePathReachedProvider: true }));
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await Promise.race([once(child, 'exit'), sleep(5000)]); if (child.exitCode === null) child.kill('SIGKILL'); }
  for (const stream of streams) stream.destroy(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve));
  await rm(temporary, { recursive: true, force: true });
}
