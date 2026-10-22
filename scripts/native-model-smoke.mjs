#!/usr/bin/env node
// Real pinned Gateway/model runtime; both native API and model are local fixtures.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
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
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function exercise(name, conversationAccess, readPolicy, dmScope = 'main') {
  const temporary = await mkdtemp(join(tmpdir(), `native-model-${name}-`));
  let child, log = ''; const requests = [], reads = [], streams = new Set();
  const token = 'synthetic-native-token-'.repeat(3);
  const upstream = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://fixture');
      if (url.pathname.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
      const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
      if (url.pathname === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running', version: { product: 'agent-wechat', backend: 'native', version: '0.16.0', apiVersion: 1, capabilities: ['serve', 'events', 'profile-read', 'text-send', 'native-login'] } });
      if (url.pathname === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
      if (url.pathname === '/v1/accounts/a/threads') return json({ items: [{ id: 'u1', readStateVersion: '1' }], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
      if (url.pathname === '/v1/accounts/a/events') return json({ items: [], nextCursor: '0' });
      if (url.pathname === '/v1/accounts/a/events/stream') { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res); req.once('close', () => streams.delete(res)); return; }
      const lookup = /^\/v1\/accounts\/a\/messages\/([^/]+)$/.exec(url.pathname);
      if (lookup) {
        const custody = JSON.parse(await readFile(join(temporary, 'probe-consumer', createHash('sha256').update('fixture\0a').digest('hex') + '.json'), 'utf8'));
        const message = custody.items[decodeURIComponent(lookup[1])]?.message;
        if (message) return json(message);
      }
      let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 4 * 1024 * 1024); }
      if (url.pathname === '/v1/accounts/a/threads/u1/read-state') { const body = JSON.parse(raw); assert.deepEqual(body, {expectedVersion:'1'}); const custody=JSON.parse(await readFile(join(temporary,'probe-consumer',createHash('sha256').update('fixture\0a').digest('hex')+'.json'),'utf8')); const triggers=Object.values(custody.items).filter(item=>item.readPending).map(item=>item.message.id); reads.push(triggers); return json({}); }
      if (url.pathname === '/v1/accounts/a/sends') return json({ id: 'synthetic-send', state: 'accepted' });
      if (url.pathname === '/model/v1/chat/completions') {
        assert.equal(req.headers.authorization, 'Bearer synthetic-model-key');
        const body = JSON.parse(raw); requests.push(body);
        const lastUser = [...body.messages].reverse().find(message => message.role === 'user');
        if (JSON.stringify(lastUser).includes('wechat-native:a:blocking')) await sleep(1500);
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        const chunk = (delta, finish_reason) => `data: ${JSON.stringify({ id: 'fixture', object: 'chat.completion.chunk', created: 1, model: 'fixture-model', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
        res.write(chunk({ role: 'assistant', content: 'Local fixture reply' }, null)); res.write(chunk({}, 'stop')); res.end('data: [DONE]\n\n'); return;
      }
      res.writeHead(404, { 'content-type': 'application/json' }); res.end('{"error":{"code":"fixture_not_found"}}');
    } catch { if (!res.headersSent) res.writeHead(500); res.end('fixture failure'); }
  });
  try {
    await mkdir(join(temporary, 'plugin/dist'), { recursive: true }); await mkdir(join(temporary, 'workspace'));
    for (const file of ['package.json', 'openclaw.plugin.json']) await copyFile(join(root, 'packages/openclaw-extension', file), join(temporary, 'plugin', file));
    await build({ entryPoints: [join(root, 'packages/openclaw-extension/test/fixtures/native-model-plugin.ts')], absWorkingDir: root, bundle: true, format: 'esm', platform: 'node', external: ['openclaw'], outfile: join(temporary, 'plugin/dist/index.js') });
    upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening'); const nativePort = upstream.address().port;
    const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening'); const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
    const config = { gateway: { mode: 'local', bind: 'loopback', port, auth: { mode: 'token', token: 'synthetic-gateway-token-'.repeat(3) }, controlUi: { enabled: false } }, browser: { enabled: false }, cron: { enabled: false }, discovery: { mdns: { mode: 'off' } }, update: { checkOnStart: false },
      session: { dmScope }, messages: { queue: { mode: 'collect' } },
      models: { mode: 'merge', providers: { fixture: { baseUrl: `http://127.0.0.1:${nativePort}/model/v1`, api: 'openai-completions', apiKey: 'synthetic-model-key', models: [{ id: 'fixture-model', name: 'Fixture', reasoning: false, input: ['text'], contextWindow: 32768, maxTokens: 256, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } },
      agents: { defaults: { workspace: join(temporary, 'workspace'), model: { primary: 'fixture/fixture-model', fallbacks: [] } } },
      plugins: { allow: ['agent-wechat'], load: { paths: [join(temporary, 'plugin')] }, entries: { 'agent-wechat': { enabled: true, hooks: { allowConversationAccess: conversationAccess }, config: { backend: 'native', native: { mode: 'external', accountId: 'a', endpoint: `http://127.0.0.1:${nativePort}`, token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(temporary, 'consumer'), readPolicy } } } } },
      logging: { file: join(temporary, 'gateway.log') }, channels: { 'agent-wechat': { enabled: true, dmPolicy: 'open', allowFrom: ['*'], groupPolicy: 'open', groupAllowFrom: ['*'] } } };
    const path = join(temporary, 'config.json'); await writeFile(path, JSON.stringify(config), { mode: 0o600 });
    // Channel startup must remain enabled: the pinned host intentionally removes
    // channel policy from runtime config when OPENCLAW_SKIP_CHANNELS is enabled.
    const env = { AGENT_WECHAT_TOKEN: token, PATH: process.env.PATH, HOME: temporary, TMPDIR: temporary, OPENCLAW_STATE_DIR: join(temporary, 'state'), OPENCLAW_CONFIG_PATH: path, NATIVE_MODEL_FIXTURE_DIR: temporary, NO_COLOR: '1' };
    child = spawn(process.execPath, [join(sdk, 'openclaw.mjs'), 'gateway', 'run'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
    for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { log = (log + chunk.toString()).slice(-131072); });
    let result;
    for (let attempt = 0; attempt < 900; attempt++) {
      if (child.exitCode !== null || child.signalCode) throw Error(`Pinned model Gateway exited: ${log}`);
      try { result = JSON.parse(await readFile(join(temporary, 'result.json'), 'utf8')); break; } catch { await sleep(100); }
    }
    assert.ok(result, `Model fixture deadline: ${log}`); assert.equal(result.error, undefined, `${result.error}; ${log}`);
    const items = new Map(result.items.map(item => [item.id, item]));
    const hooks = (await readFile(join(temporary, 'hooks.jsonl'), 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
    assert.ok(requests.some(request => JSON.stringify(request.messages).includes('wechat-native:a:single') && JSON.stringify(request.messages).includes('native-model-single-exact-input')));
    const inbound = readPolicy === undefined || readPolicy === 'on_inbound';
    assert.equal(items.get('gap').readPending, undefined);
    assert.ok(!reads.flat().includes('gap'));
    if (inbound) {
      for (const id of ['single', 'blocking', 'queued-a', 'queued-b', 'command']) {
        assert.equal(items.get(id).runId, undefined);
        assert.equal(items.get(id).handoffUnconfirmed, false);
        assert.equal(items.get(id).readPending, false);
        assert.ok(reads.flat().includes(id));
      }
      assert.ok(!requests.some(request => JSON.stringify(request.messages.at(-1)).includes('wechat-native:a:command')),
        'The built-in command must gain a read without invoking a model');
    } else if (conversationAccess) {
      const single = items.get('single'); assert.equal(single.runId, single.manifest.runId); assert.equal(single.handoffUnconfirmed, false);
      if (readPolicy === 'never') assert.equal(reads.length, 0); else assert.ok(reads.flat().includes('single'));
      if (dmScope === 'per-channel-peer') assert.equal(single.manifest.sessionKey, 'agent:main:agent-wechat:direct:u1');
      const input = hooks.find(hook => hook.kind === 'llm_input' && hook.event.runId === single.runId); assert.ok(input); assert.equal(input.ctx.channel, 'agent-wechat'); assert.equal(input.ctx.sessionKey, single.manifest.sessionKey);
      assert.ok(hooks.some(hook => hook.kind === 'model_call_started' && hook.event.runId === single.runId && hook.event.sessionId === input.event.sessionId));
    } else { assert.equal(reads.length, 0); assert.equal(items.get('single').handoffUnconfirmed, true); }
    if (!inbound) { assert.equal(items.get('command').readPending, undefined); assert.ok(!reads.flat().includes('command')); }
    // The released dispatch path can serialize concurrent inputs even in
    // collect mode. Check what actually reached it rather than assuming merge.
    const merged = hooks.some(hook => hook.kind === 'llm_input' && hook.event.prompt.includes('native-model-queued-alpha') && hook.event.prompt.includes('native-model-queued-beta'));
    for (const [id, body] of [['blocking', 'native-model-blocking-input'], ['queued-a', 'native-model-queued-alpha'], ['queued-b', 'native-model-queued-beta']]) {
      const item = items.get(id);
      const input = hooks.find(hook => hook.kind === 'llm_input' && hook.event.runId === item.manifest.runId);
      if (!inbound && conversationAccess && readPolicy !== 'never' && input?.event.prompt.includes(body) && !merged) {
        assert.equal(item.runId, item.manifest.runId); assert.equal(item.handoffUnconfirmed, false); assert.ok(reads.flat().includes(id));
      }
      if (!inbound && merged && id.startsWith('queued-')) assert.ok(!reads.flat().includes(id));
    }
    const inputBodies = new Map([['single', 'native-model-single-exact-input'], ['blocking', 'native-model-blocking-input'], ['queued-a', 'native-model-queued-alpha'], ['queued-b', 'native-model-queued-beta']]);
    for (const id of inbound ? [] : reads.flat()) {
      const item = items.get(id); const body = inputBodies.get(id); assert.ok(item && body);
      const input = hooks.find(hook => hook.kind === 'llm_input' && hook.event.runId === item.runId); assert.ok(input);
      assert.ok(hooks.some(hook => hook.kind === 'model_call_started' && hook.event.runId === item.runId && hook.event.sessionId === input.event.sessionId));
      assert.ok(requests.some(request => {
        const users = request.messages.filter(message => message.role === 'user');
        return JSON.stringify(users.at(-1)).includes(`wechat-native:a:${id}`) && JSON.stringify(users.at(-2)).includes(body);
      }), `Exact current input missing at local provider for ${id}`);
    }
    console.log(`Pinned inbound/model boundary ${name} passed: whole-thread reads, concurrent turns and command policy; merged=${merged}.`);
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await Promise.race([once(child, 'exit'), sleep(8000)]); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await once(child, 'exit'); } }
    for (const stream of streams) stream.end(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(temporary, { recursive: true, force: true });
  }
}
const phases = [['inbound', false, undefined], ['permitted', true, 'on_model_dispatch'], ['missing-grant', false, 'on_model_dispatch'], ['never-read', true, 'never'], ['per-peer', true, 'on_model_dispatch', 'per-channel-peer']];
assert.ok(!process.argv[2] || phases.some(phase => phase[0] === process.argv[2]), 'Unknown phase');
for (const [name, access, policy, dmScope] of phases) if (!process.argv[2] || process.argv[2] === name) await exercise(name, access, policy, dmScope);
console.log('No external provider, account credentials, native login or WeChat send was used.');
