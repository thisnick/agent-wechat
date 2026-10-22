#!/usr/bin/env node
// Real pinned Gateway/model runtime; both native API and model are local fixtures.
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
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

async function exercise(name, conversationAccess, readPolicy, dmScope = 'main') {
  const temporary = await mkdtemp(join(tmpdir(), `native-model-${name}-`));
  let child, log = ''; const requests = [], reads = [], streams = new Set();
  const token = 'synthetic-native-token-'.repeat(3);
  const threadId = name === 'group' ? 'room@chatroom' : 'u1';
  const assets = new Map();
  for (const [id, name, contentType, kind] of [['image', 'native-model.png', 'image/png', 'image'], ['image-large', 'native-model-large.png', 'image/png', 'image'], ['pending-image', 'native-model.png', 'image/png', 'image'], ['expired-image', 'native-model.png', 'image/png', 'image'], ['corrupt-image', 'native-model.png', 'image/png', 'image'], ['video', 'native-model.mp4', 'video/mp4', 'video'], ['file', 'fixture.txt', 'text/plain', 'file'], ['voice', 'fixture.wav', 'audio/wav', 'voice'], ['voice-failed', 'voice-failed.wav', 'audio/wav', 'voice']]) {
    let bytes;
    if (id === 'file') bytes = Buffer.from('Synthetic local file body.');
    else if (id.startsWith('voice')) { bytes = Buffer.alloc(44 + 3200); bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8); bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22); bytes.writeUInt32LE(16000, 24); bytes.writeUInt32LE(32000, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(3200, 40); }
    else bytes = await readFile(join(root, 'packages/openclaw-extension/test/fixtures', name));
    assets.set(id, { bytes, asset: { id: `asset-${id}`, accountId: 'a', messageId: id, kind, variant: kind === 'image' ? 'original' : 'normal', actualVariant: kind === 'image' ? 'original' : 'normal', state: id === 'pending-image' ? 'pending' : id === 'expired-image' ? 'expired' : 'available', contentType, byteLength: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), filename: name, officialWriteback: 'complete', retryable: false } });
  }
  const upstream = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://fixture');
      if (url.pathname.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
      const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
      if (url.pathname === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running', version: { product: 'agent-wechat', backend: 'native', version: '0.16.0', apiVersion: 1, capabilities: ['serve', 'events', 'profile-read', 'text-send', 'native-login'] } });
      if (url.pathname === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
      if (url.pathname === '/v1/accounts/a/threads') return json({ items: [{ id: threadId, readStateVersion: '1' }], nextCursor: null, sourceGeneration: 'fixture', coverage: {} });
      if (url.pathname === '/v1/accounts/a/events') return json({ items: [], nextCursor: '0' });
      if (url.pathname === '/v1/accounts/a/events/stream') { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res); req.once('close', () => streams.delete(res)); return; }
      const lookup = /^\/v1\/accounts\/a\/messages\/([^/]+)$/.exec(url.pathname);
      if (lookup) {
        const custody = JSON.parse(await readFile(join(temporary, 'probe-consumer', createHash('sha256').update('fixture\0a').digest('hex') + '.json'), 'utf8'));
        const message = custody.items[decodeURIComponent(lookup[1])]?.message;
        if (message) return json(message);
      }
      const media = /^\/v1\/accounts\/a\/messages\/([^/]+)\/media$/.exec(url.pathname);
      if (media) return json({ items: [assets.get(media[1]).asset] });
      const content = /^\/v1\/accounts\/a\/media\/asset-([^/]+)\/content$/.exec(url.pathname);
      if (content) { const value = assets.get(content[1]); res.setHeader('content-type', value.asset.contentType); res.end(content[1] === 'corrupt-image' ? Buffer.from('wrong') : value.bytes); return; }
      let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 4 * 1024 * 1024); }
      if (url.pathname.endsWith('/media-transfers')) return json({ id: 'pending-job', state: 'queued' });
      if (url.pathname === `/v1/accounts/a/threads/${encodeURIComponent(threadId)}/read-state`) { const body = JSON.parse(raw); assert.deepEqual(body, {expectedVersion:'1'}); const custody=JSON.parse(await readFile(join(temporary,'probe-consumer',createHash('sha256').update('fixture\0a').digest('hex')+'.json'),'utf8')); const triggers=Object.values(custody.items).filter(item=>item.readPending).map(item=>item.message.id); reads.push(triggers); if (triggers.includes('file') && reads.filter(ids => ids.includes('file')).length === 1) { res.statusCode = 503; return json({error:{code:'fixture_lost_response'}}); } return json({}); }
      if (url.pathname === '/v1/accounts/a/sends') return json({ id: 'synthetic-send', state: 'accepted' });
      if (url.pathname === '/model/v1/chat/completions') {
        assert.equal(req.headers.authorization, 'Bearer synthetic-model-key');
        const body = JSON.parse(raw); requests.push(body);
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
    await build({ entryPoints: [join(root, 'packages/openclaw-extension/test/fixtures/native-media-model-plugin.ts')], absWorkingDir: root, bundle: true, format: 'esm', platform: 'node', external: ['openclaw'], outfile: join(temporary, 'plugin/dist/index.js') });
    upstream.listen(0, '127.0.0.1'); await once(upstream, 'listening'); const nativePort = upstream.address().port;
    const probe = createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening'); const port = probe.address().port; await new Promise(resolve => probe.close(resolve));
    // The real SDK invokes this explicitly configured local transcription CLI.
    // It validates fixture bytes and returns deterministic text, not recognition.
    const transcriber = join(temporary, 'transcribe.mjs');
    await writeFile(transcriber, `import {appendFileSync,readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const path=process.argv[2], bytes=readFileSync(path);
if(bytes.toString('ascii',0,4)!=='RIFF')throw Error('Expected synthetic WAV');
appendFileSync(process.argv[3],JSON.stringify({path,sha256:createHash('sha256').update(bytes).digest('hex')})+'\\n');
if(path.includes('voice-failed'))process.exit(1);
console.log('Synthetic local transcript');
`, {mode:0o600});
    const config = { gateway: { mode: 'local', bind: 'loopback', port, auth: { mode: 'token', token: 'synthetic-gateway-token-'.repeat(3) }, controlUi: { enabled: false } }, browser: { enabled: false }, cron: { enabled: false }, discovery: { mdns: { mode: 'off' } }, update: { checkOnStart: false },
      session: { dmScope }, messages: { queue: { mode: 'collect' } },
      models: { mode: 'merge', providers: { fixture: { baseUrl: `http://127.0.0.1:${nativePort}/model/v1`, api: 'openai-completions', apiKey: 'synthetic-model-key', models: [{ id: 'fixture-model', name: 'Fixture', reasoning: false, input: name === 'text-only' ? ['text'] : ['text', 'image'], contextWindow: 32768, maxTokens: 256, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] } } },
      tools: { media: { models: [{type:'cli',command:process.execPath,args:[transcriber,'{{MediaPath}}',join(temporary,'transcriptions.jsonl')],capabilities:['audio']}], audio: { enabled: true }, image: { enabled: false }, video: { enabled: false } } }, agents: { defaults: { workspace: join(temporary, 'workspace'), model: { primary: 'fixture/fixture-model', fallbacks: [] } } },
      plugins: { allow: ['agent-wechat'], load: { paths: [join(temporary, 'plugin')] }, entries: { 'agent-wechat': { enabled: true, hooks: { allowConversationAccess: conversationAccess }, config: { backend: 'native', native: { mode: 'external', accountId: 'a', endpoint: `http://127.0.0.1:${nativePort}`, token: { source: 'env', provider: 'default', id: 'AGENT_WECHAT_TOKEN' }, consumerStateDir: join(temporary, 'consumer'), readPolicy } } } } },
      logging: { file: join(temporary, 'gateway.log') }, channels: { 'agent-wechat': { enabled: true, dmPolicy: 'open', allowFrom: ['*'], groupPolicy: 'open', groupAllowFrom: ['*'] } } };
    const scenarios = (name === 'text-only' ? [{id:'image',type:3},{id:'image-large',type:3}] : [...assets.keys()].map(id => ({id,type:id.includes('image')?3:id==='file'?49:id==='video'?43:34}))).map(item=>({...item,threadId}));
    await writeFile(join(temporary,'scenarios.json'),JSON.stringify(scenarios));
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
    const transcriptions = name === 'text-only' ? [] : (await readFile(join(temporary,'transcriptions.jsonl'),'utf8')).split('\n').filter(Boolean).map(line=>JSON.parse(line));
    if (name !== 'text-only') { assert.equal(transcriptions.length,2); assert.ok(transcriptions.every(item=>item.sha256===assets.get('voice').asset.sha256)); }
    const evidence = process.env.NATIVE_MODEL_MEDIA_PROOF_DIR;
    if (evidence) {
      const destination = join(evidence, name + '-' + Date.now()); await mkdir(destination, { recursive: true, mode: 0o700 });
      await writeFile(join(destination, 'observations.json'), JSON.stringify({ result, requests, hooks, reads, transcriptions }), { mode: 0o600 });
    }
    const inbound = readPolicy === 'on_inbound';
    const eligible = inbound || conversationAccess && readPolicy !== 'never' && name !== 'text-only';
    for (const { id } of scenarios) {
      const item = items.get(id); assert.ok(item);
      if (['pending-image','corrupt-image','expired-image'].includes(id)) {
        assert.equal(item.runId, undefined);
        assert.equal(reads.flat().includes(id), inbound && id === 'expired-image');
        continue;
      }
      const asset = item.manifest.assets[0]; assert.ok(asset); assert.equal(item.manifest.newInputs[0].version,'v1');
      assert.equal(item.manifest.newInputs[0].role,'trigger'); assert.equal(item.manifest.manifestSha256.length,64);
      assert.equal(createHash('sha256').update(await readFile(asset.path)).digest('hex'),asset.sha256);
      if (inbound) { assert.equal(item.runId,undefined); assert.equal(item.handoffUnconfirmed,false); }
      else if (conversationAccess && name !== 'text-only') { assert.equal(item.runId,item.manifest.runId); assert.equal(item.handoffUnconfirmed,false); }
      else { assert.equal(item.runId,undefined); assert.equal(item.handoffUnconfirmed,true); }
      assert.equal(reads.flat().includes(id),eligible);
      const request = requests.find(request => JSON.stringify(request.messages.at(-1)).includes(`wechat-native:a:${id}`)); assert.ok(request);
      const current = request.messages.filter(message=>message.role==='user').at(-2);
      if (asset.role === 'model_image') {
        const images = Array.isArray(current.content) ? current.content.filter(block=>block.type==='image_url') : [];
        if (name === 'text-only') assert.equal(images.length,0);
        else { assert.equal(images.length,1); const image=/^data:([^;]+);base64,(.+)$/.exec(images[0].image_url.url); assert.ok(image); assert.equal(image[1],asset.mime); assert.equal(createHash('sha256').update(Buffer.from(image[2],'base64')).digest('hex'),asset.sha256); }
        if(id==='image-large') { assert.notEqual(asset.sourceSha256,asset.sha256); assert.match(JSON.stringify(current),/prepared still preview/); }
      } else assert.ok(JSON.stringify(current).includes(asset.path));
      if(id==='voice') assert.match(JSON.stringify(current),/Synthetic local transcript/);
      if(id==='voice-failed') assert.match(JSON.stringify(current),/Voice transcription unavailable/);
    }
    if (items.has('file')) { assert.equal(result.outcomes['duplicate-file'],'needs_review'); assert.equal(requests.filter(request=>JSON.stringify(request.messages.at(-1)).includes('wechat-native:a:file')).length,1); }
    if (!eligible) assert.equal(reads.length,0);
    if (eligible) assert.equal(reads.filter(ids=>ids.includes('file')).length,2);
    console.log(`Pinned media model ${name} passed: exact image bytes/reference manifests, omitted/failed/pending guards, transcription status and durable thread receipts.`);
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) { child.kill('SIGTERM'); await Promise.race([once(child, 'exit'), sleep(8000)]); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await once(child, 'exit'); } }
    for (const stream of streams) stream.end(); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(temporary, { recursive: true, force: true });
  }
}
const phases = [['inbound',false,'on_inbound'],['media',true,'on_model_dispatch'],['text-only',true,'on_model_dispatch'],['missing-grant',false,'on_model_dispatch'],['never-read',true,'never'],['group',true,'on_model_dispatch']];
for (const [name, access, policy] of phases) if (!process.argv[2] || process.argv[2] === name) await exercise(name, access, policy);
assert.ok(!process.argv[2] || phases.some(phase=>phase[0]===process.argv[2]),'Unknown phase');
console.log('All models, media, tokens and messages were synthetic local fixtures; no WeChat or external provider was used.');
