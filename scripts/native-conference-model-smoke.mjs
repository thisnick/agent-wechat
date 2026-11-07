#!/usr/bin/env node
// Real pinned Gateway and tools; the model and native API are loopback fixtures.
import { stagePluginRuntimeDependencies } from './native-smoke-plugin.mjs';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, copyFile, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
const root = resolve(dirname(new URL(import.meta.url).pathname), '..');
const { build } = createRequire(join(root, 'packages/openclaw-extension/package.json'))('esbuild');
const sdk = await realpath(process.env.OPENCLAW_TEST_SDK_ROOT ?? join(root, 'packages/openclaw-extension/node_modules/openclaw'));
assert.ok(['2026.10.9', '2026.10.10'].includes(JSON.parse(await readFile(join(sdk, 'package.json'), 'utf8')).version));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const temporary = await mkdtemp(join(tmpdir(), 'native-conference-model-'));
const requests = [], streams = new Set(); let child, log = '';
const token = 'synthetic-native-token-'.repeat(3);
const upstream = createServer(async (req, res) => {
  try {
    const path = new URL(req.url, 'http://fixture').pathname;
    const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
    if (path.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
    if (path === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running', version: { product: 'agent-wechat', backend: 'native', version: '1.5.1', apiVersion: 1, capabilities: ['serve', 'events', 'profile-read', 'text-send', 'native-login'] } });
    if (path === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
    if (path === '/v1/accounts/a/events') return json({ items: [], nextCursor: '0' });
    if (path === '/v1/accounts/a/threads') return json({ items: [], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
    if (path === '/v1/accounts/a/events/stream') { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res); req.once('close', () => streams.delete(res)); return; }
    if (path === '/model/v1/chat/completions') {
      assert.equal(req.headers.authorization, 'Bearer synthetic-model-key');
      let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 4 * 1024 * 1024); }
      const body = JSON.parse(raw); requests.push(body);
      const text = content => typeof content === 'string' ? content : Array.isArray(content) ? content.map(part => part.text ?? '').join('\n') : '';
      const current = body.messages.filter(message => message.role === 'user').map(message => text(message.content)).join('\n');
      const ownerQuestion = /Conference question:\s*group-owner-probe\b|Caller question:\s*direct-owner-probe\b/.test(current);
      const hangupQuestion = /Conference question:\s*group-owner-hangup-probe\b/.test(current);
      const revokedQuestion = /Conference question:\s*group-revoked-probe\b/.test(current);
      const names = (body.tools ?? []).map(tool => tool.function?.name);
      const toolReply = body.messages.some(message => message.role === 'tool');
      if (revokedQuestion) { await writeFile(join(temporary, 'revoked'), 'revoked'); await sleep(150); }
      const tool = !toolReply && (ownerQuestion || revokedQuestion || hangupQuestion);
      const toolName = hangupQuestion ? 'wechat_call' : 'conference_owner_probe';
      if (tool && !revokedQuestion && !names.includes(toolName)) {
        log += `\nUnexpected model tool names: ${JSON.stringify(names)}`;
        return json({ choices: [{ message: { role: 'assistant', content: 'Fixture owner tool missing' }, finish_reason: 'stop' }] });
      }
      if (hangupQuestion && !toolReply) assert.ok(names.includes('wechat_call'), 'owner call-control tool was not advertised to the model');
      if (current.includes('group-read-only-probe') || current.includes('group-restricted-probe') || current.includes('agent-conference-read-only-probe')) {
        assert.ok(!names.includes('conference_owner_probe'), 'restricted group received the owner-only fixture tool');
        assert.ok(!names.includes('wechat_call'), 'restricted group received call-control tools');
      }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: 'synthetic-probe-call', type: 'function', function: { name: toolName, arguments: hangupQuestion ? '{"action":"hangup"}' : '{}' } }] }
        : { role: 'assistant', content: 'Local conference fixture reply' };
      const chunk = (delta, finish_reason) => `data: ${JSON.stringify({ id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'fixture-model', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
      res.write(chunk(delta, null)); res.write(chunk({}, tool ? 'tool_calls' : 'stop')); res.end('data: [DONE]\n\n'); return;
    }
    res.writeHead(404); res.end('fixture not found');
  } catch (error) { if (!res.headersSent) res.writeHead(500); res.end('fixture failure'); log += `\nFixture failure: ${error}`; }
});
try {
  await mkdir(join(temporary, 'plugin/dist'), { recursive: true }); await mkdir(join(temporary, 'workspace'));
  for (const file of ['package.json', 'openclaw.plugin.json']) await copyFile(join(root, 'packages/openclaw-extension', file), join(temporary, 'plugin', file));
  const fixtureManifestPath = join(temporary, 'plugin/openclaw.plugin.json');
  const fixtureManifest = JSON.parse(await readFile(fixtureManifestPath, 'utf8'));
  fixtureManifest.contracts.tools.push('conference_owner_probe');
  fixtureManifest.contracts.speechProviders = ['fixture-speech'];
  fixtureManifest.contracts.realtimeTranscriptionProviders = ['fixture-stt'];
  fixtureManifest.toolMetadata.conference_owner_probe = { optional: true };
  await writeFile(fixtureManifestPath, JSON.stringify(fixtureManifest));
  await stagePluginRuntimeDependencies(root, join(temporary, 'plugin'));
  await build({ entryPoints: [join(root, 'packages/openclaw-extension/test/fixtures/native-conference-model-plugin.ts')], absWorkingDir: root, bundle: true, format: 'esm', platform: 'node', external: ['openclaw', 'ws'], outfile: join(temporary, 'plugin/dist/index.js') });
  upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening'); const nativePort = upstream.address().port;
  const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening'); const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
  await mkdir(join(temporary, 'workspace'), { recursive: true });
  await writeFile(join(temporary, 'workspace/USER.md'), 'Synthetic voice profile user');
  await writeFile(join(temporary, 'workspace/SOUL.md'), 'Synthetic voice profile persona');
  await writeFile(join(temporary, 'workspace/AGENTS.md'), 'private-agents-profile-sentinel');
  const config = { gateway: { mode: 'local', bind: 'loopback', port, auth: { mode: 'token', token: 'synthetic-gateway-token-'.repeat(3) }, controlUi: { enabled: false } }, browser: { enabled: false }, cron: { enabled: false }, discovery: { mdns: { mode: 'off' } }, update: { checkOnStart: false },
    commands: { ownerAllowFrom: ['agent-wechat:synthetic-human-owner', 'agent-wechat:synthetic-group@chatroom'] }, tools: { toolSearch: false, codeMode: false, profile: 'coding', alsoAllow: ['conference_owner_probe'] },
    models: { mode: 'merge', providers: { fixture: { baseUrl: `http://127.0.0.1:${nativePort}/model/v1`, api: 'openai-completions', apiKey: 'synthetic-model-key', models: [{ id: 'fixture-model', name: 'Fixture', reasoning: false, input: ['text'], contextWindow: 262144, maxTokens: 256, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } },
    agents: { defaults: { workspace: join(temporary, 'workspace'), model: { primary: 'fixture/fixture-model', fallbacks: [] } }, entries: { main: { tools: { alsoAllow: ['conference_owner_probe', 'wechat_call'] } } } },
    plugins: { allow: ['agent-wechat'], load: { paths: [join(temporary, 'plugin')] }, entries: { 'agent-wechat': { enabled: true, config: { backend: 'native', native: { mode: 'external', accountId: 'a', endpoint: `http://127.0.0.1:${nativePort}`, token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(temporary, 'consumer'), readPolicy: 'never' } } } } },
    tts: { provider: 'fixture-speech', providers: { 'fixture-speech': {} } },
    channels: { 'agent-wechat': { enabled: true, dmPolicy: 'disabled', groupPolicy: 'disabled', calls: { voice: {
      enabled: true, mode: 'agent', agentId: 'main', transcription: { provider: 'fixture-stt' }, toolPolicy: 'owner', allowConsultSessionPersistence: true,
      conferences: { groups: { 'synthetic-group@chatroom': { toolPolicy: 'safe-read-only', ownerInviterToolPolicy: 'owner' } } },
    } } } }, logging: { file: join(temporary, 'gateway.log') } };
  const configPath = join(temporary, 'config.json'), initial = JSON.stringify(config); await writeFile(configPath, initial, { mode: 0o600 });
  const env = { AGENT_WECHAT_TOKEN: token, PATH: process.env.PATH, HOME: temporary, TMPDIR: temporary, OPENCLAW_STATE_DIR: join(temporary, 'state'), OPENCLAW_CONFIG_PATH: configPath, NATIVE_CONFERENCE_FIXTURE_DIR: temporary, NO_COLOR: '1' };
  child = spawn(process.execPath, [join(sdk, 'openclaw.mjs'), 'gateway', 'run'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk.toString()).slice(-131072); });
  let result;
  for (let i = 0; i < 600; i++) { if (child.exitCode !== null || child.signalCode) throw Error(`Isolated Gateway exited: ${log}`); try { result = JSON.parse(await readFile(join(temporary, 'result.json'), 'utf8')); break; } catch { await sleep(100); } }
  const toolEvents = await readFile(join(temporary, 'tool-events.jsonl'), 'utf8').catch(() => 'none');
  assert.ok(result, `Conference Gateway deadline: ${log}`); assert.equal(result.error, undefined, `${JSON.stringify(result)}; requests=${requests.length}; events=${toolEvents}; ${log}`);
  assert.equal(result.calls.length, 2, JSON.stringify({ result, modelTools: requests.map(body => (body.tools ?? []).map(tool => tool.function?.name)), toolReplies: requests.flatMap(body => body.messages.filter(message => message.role === 'tool')), log: log.slice(-2500) })); assert.equal(result.calls[0].senderId, result.groupId); assert.equal(result.calls[0].owner, true);
  assert.equal(result.calls[1].senderId, 'synthetic-human-owner'); assert.equal(result.calls[1].owner, true);
  assert.deepEqual(result.callControls, ['call.hangup'], 'the owner consultation did not reach the call-control port');
  assert.ok(result.calls[0].sessionKey.includes('wechat-conference:')); assert.equal(result.revoked, true); assert.equal(result.originalOwnersUnchanged, true); assert.equal(result.separateSessions, true);
  assert.equal(await readFile(configPath, 'utf8'), initial, 'the group grant changed the saved Gateway configuration');
  assert.ok(requests.length >= 6); assert.ok(requests.every(body => body.model === 'fixture-model'));
  assert.deepEqual(result.voiceContext, ['response', 'continuous'].map(mode => ({ mode, messages: 4, audioPreserved: true, interruptionPreserved: true })));
  assert.deepEqual(result.agentCalls, ['direct', 'conference-owner', 'conference-read-only'].map(kind => ({ kind, owner: kind !== 'conference-read-only', audio: true, transcript: true })));
  console.log('Pinned conference Gateway passed: per-agent optional calling-tool grant, owner consultation reaches local hangup control, read-only tool rejection, agent/TTS direct and owner/read-only conference pipelines, caller/roster context, audio and interruption, group principal, revocation, normal configured model routing and unchanged saved/global owner config. No external provider, login, WeChat send or deployment.');
} finally {
  if (child && child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await Promise.race([once(child, 'exit'), sleep(8000)]); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await once(child, 'exit'); } }
  for (const stream of streams) stream.end(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(temporary, { recursive: true, force: true });
}
