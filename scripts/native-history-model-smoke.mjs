#!/usr/bin/env node
// Actual pinned Gateway, model tool calls, group catch-up and direct media.
// Native account/messages/media and provider responses are local fixtures.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { crc32 } from 'node:zlib';
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
const temporary = await mkdtemp(join(tmpdir(), 'native-history-model-'));
const requests = [], queries = [], reads = [], transfers = [], lookups = [], streams = new Set(); let child, fixtureError, log = '', modelStep = 0;
const token = 'synthetic-native-token-'.repeat(3), assets = new Map();
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
for (const [id, file, kind, mime] of [['image-a','native-model.png','image','image/png'], ['image-b','native-model-large.png','image','image/png'], ['dm-image','native-model.png','image','image/png'], ['image-file','native-model.png','file','image/png'], ['video','native-model.mp4','video','video/mp4'], ['voice',null,'voice','audio/wav'], ['file',null,'file','text/plain'], ['pending-image','native-model.png','image','image/png'], ['history-image','native-model.png','image','image/png'], ['history-file',null,'file','text/plain']]) {
  let bytes;
  if (file) bytes = await readFile(join(root, 'packages/openclaw-extension/test/fixtures', file));
  else if (kind === 'voice') { bytes = Buffer.alloc(44 + 3200); bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8); bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22); bytes.writeUInt32LE(16000, 24); bytes.writeUInt32LE(32000, 28); bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(3200, 40); }
  else bytes = Buffer.from('Synthetic stored attachment body');
  assets.set(id, { bytes, asset: { id: `asset-${id}`, accountId: 'a', messageId: id, kind, variant: kind === 'image' ? 'original' : 'normal', actualVariant: kind === 'image' ? 'original' : 'normal', state: ['pending-image','history-image'].includes(id) ? 'pending' : 'available', contentType: mime, byteLength: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), filename: `${id}.${mime.split('/')[1]}`, officialWriteback: 'complete', retryable: false } });
}
const quoteScenarios = [
  { id:'quote-text', type:49, appMessageType:57, mention:true, body:'Comment about quoted text', quote:{serverId:'18446744073709551615',type:1} },
  { id:'quote-image', type:49, appMessageType:57, mention:true, body:'Comment about quoted image', quote:{serverId:'9007199254740998',type:3} },
  { id:'quote-embedded-text', type:49, appMessageType:57, threadId:'u1', body:'Embedded text comment', quote:{serverId:'9007199254740997',type:1} },
  { id:'quote-embedded-image', type:49, appMessageType:57, mention:true, body:'Embedded image comment', quote:{serverId:'9007199254740996',type:3} },
  { id:'forwarded-bundle', type:49, appMessageType:19, mention:true, body:'[Chat History] Shared discussion\nAlice: Forwarded text\nBob: [media]\n  Carol: Nested forwarded text' },
];
for(const id of ['quoted-original-image','quote-embedded-image']) {
  const source=assets.get('dm-image');
  const text=Buffer.from(`Comment\0${id}`),chunk=Buffer.alloc(text.length+12);chunk.writeUInt32BE(text.length,0);chunk.write('tEXt',4);text.copy(chunk,8);chunk.writeUInt32BE(crc32(chunk.subarray(4,-4)),chunk.length-4);
  const bytes=Buffer.concat([source.bytes.subarray(0,-12),chunk,source.bytes.subarray(-12)]);
  assets.set(id,{bytes,asset:{...source.asset,id:`asset-${id}`,messageId:id,filename:`${id}.png`,byteLength:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex')}});
}
const quoteResolutions = {
  'quote-text':{state:'resolved',message:{id:'quoted-original-text',accountId:'a',threadId:'room@chatroom',senderId:'u1',serverId:'18446744073709551615',direction:'incoming',type:1,body:'Quoted original stored text',version:'v1'}},
  'quote-image':{state:'resolved',message:{id:'quoted-original-image',accountId:'a',threadId:'room@chatroom',senderId:'u1',serverId:'9007199254740998',direction:'incoming',type:3,version:'v1'}},
  'quote-embedded-text':{state:'embedded',message:null,senderId:'u1',embeddedText:'Quoted embedded text with no original row',embeddedImage:false},
  'quote-embedded-image':{state:'embedded',message:null,senderId:'u1',embeddedImage:true},
};
const toolSteps = [
  ['tool_search', { query:'wechat_search search stored WeChat conversation messages', limit:4 }],
  ['tool_call', { id:'wechat_search', args:{ query:'earlier-project-note', limit:2 } }],
  ['tool_call', { id:'wechat_search', args:{ query:'earlier-project-note', limit:2, cursor:'older-matches' } }],
  ['wechat_media', { messageId:'foreign-image' }],
  ['wechat_media', { messageId:'history-image' }],
  ['wechat_media', { messageId:'history-file', download:false }],
];
const upstream = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://fixture');
    if (url.pathname.startsWith('/v1/')) assert.equal(req.headers.authorization, `Bearer ${token}`);
    const json = value => { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(value)); };
    if (url.pathname === '/v1/status') return json({ serverId: 'fixture', dataDir: '/fixture', pid: process.pid, ownershipGeneration: 'fixture', state: 'running', version: { product: 'agent-wechat', backend: 'native', version: '1.0.0', apiVersion: 1, capabilities: ['serve','events','profile-read','text-send','native-login'] } });
    if (url.pathname === '/v1/accounts/a') return json({ id: 'a', authenticated: true, connection: 'online' });
    if (url.pathname === '/v1/accounts/a/threads') return json({ items: ['room@chatroom','u1'].map(id => ({ id, readStateVersion: '1' })), nextCursor: null, coverage: {} });
    if (url.pathname === '/v1/accounts/a/events/stream') { res.writeHead(200, { 'content-type': 'text/event-stream' }); res.write(': fixture\n\n'); streams.add(res); req.once('close', () => streams.delete(res)); return; }
    if (url.pathname === '/v1/accounts/a/threads/room%40chatroom/messages') {
      assert.equal(url.searchParams.get('query'), 'earlier-project-note'); queries.push(Object.fromEntries(url.searchParams));
      const id = url.searchParams.has('cursor') ? 'older-page-2' : 'stored-before-buffer';
      const attachmentId = id === 'stored-before-buffer' ? 'history-image' : 'history-file';
      return json({ items: [{ id, accountId: 'a', threadId: 'room@chatroom', senderId: 'u1', serverId: '9007199254740999', direction: 'incoming', type: 1, body: `earlier-project-note: ${id}`, version: 'v1', media: [] },
        { id:attachmentId, accountId:'a', threadId:'room@chatroom', senderId:'u1', direction:'incoming', type:attachmentId==='history-image'?3:49, body:'<private transport XML>', version:'v1', media:[] }], nextCursor: id === 'older-page-2' ? null : 'older-matches', sourceGeneration: 'fixture', coverage: { history: 'partial' } });
    }
    const lookup = /^\/v1\/accounts\/a\/messages\/(history-image|history-file|foreign-image)$/.exec(url.pathname);
    if (lookup) { lookups.push(lookup[1]); return json({id:lookup[1],accountId:'a',threadId:lookup[1]==='foreign-image'?'private-peer':'room@chatroom',senderId:'u1',direction:'incoming',type:lookup[1]==='history-file'?49:3,body:'<private transport XML>',version:'v1',media:[]}); }
    const quoteLookup = /^\/v1\/accounts\/a\/messages\/([^/]+)\/quote$/.exec(url.pathname);
    if(quoteLookup){const scenario=quoteScenarios.find(value=>value.id===quoteLookup[1]);assert.ok(scenario);return json({accountId:'a',messageId:scenario.id,threadId:scenario.threadId??'room@chatroom',...quoteResolutions[scenario.id]});}
    const media = /^\/v1\/accounts\/a\/messages\/([^/]+)\/media$/.exec(url.pathname);
    if (media) { assert.notEqual(media[1],'foreign-image','Foreign conversation must be rejected before attachment access'); return json({ items: [assets.get(media[1]).asset] }); }
    const content = /^\/v1\/accounts\/a\/media\/asset-([^/]+)\/content$/.exec(url.pathname);
    if (content) { const value = assets.get(content[1]); res.setHeader('content-type', value.asset.contentType); res.end(value.bytes); return; }
    let raw = ''; for await (const chunk of req) { raw += chunk; assert.ok(raw.length < 8 * 1024 * 1024); }
    if (url.pathname.endsWith('/media-transfers')) {
      if(url.pathname.includes('/history-image/')) { transfers.push(JSON.parse(raw)); assets.get('history-image').asset.state='available'; return json({id:'history-transfer',accountId:'a',kind:'download',state:'available',retryable:false}); }
      assert.ok(url.pathname.includes('/pending-image/'),'Only authorized missing media may request a transfer'); return json({ id: 'pending-job', state: 'queued' });
    }
    if (url.pathname.endsWith('/read-state')) { reads.push(url.pathname); return json({ phoneNotification: {state:'accepted'} }); }
    if (url.pathname === '/v1/accounts/a/sends') return json({ id: 'fixture-send', state: 'accepted' });
    if (url.pathname === '/model/v1/chat/completions') {
      const body = JSON.parse(raw); requests.push(body);
      const [requestedTool, args] = toolSteps[modelStep] ?? [];
      if (requestedTool) assert.ok(body.tools.some(tool => tool.function.name === requestedTool), `${requestedTool} missing from real Gateway catalog`);
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const chunk = (delta, finish_reason) => `data: ${JSON.stringify({ id: `fixture-${requests.length}`, object: 'chat.completion.chunk', created: 1, model: 'fixture-model', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
      if (requestedTool) {
        if (modelStep === 1) assert.ok(JSON.stringify(body.messages.filter(message=>message.role==='tool')).includes('wechat_search'), 'Stored-history tool missing from discovery results');
        if (modelStep === 2) assert.ok(JSON.stringify(body.messages).includes('stored-before-buffer'), 'Search result missing from the next provider request');
        if (modelStep === 3) assert.ok(JSON.stringify(body.messages).includes('older-page-2'), 'Continuation result missing from provider request');
        if (modelStep === 4) assert.ok(JSON.stringify(body.messages).includes('outside the permitted conversation'), 'Foreign message lookup was not rejected');
        if (modelStep === 5) assert.ok(JSON.stringify(body.messages).includes('history-image.png'), 'Downloaded image result missing from provider request');
        res.write(chunk({ role:'assistant', tool_calls:[{ index:0, id:`fixture-search-${modelStep++}`, type:'function', function:{ name:requestedTool, arguments:JSON.stringify(args) } }] }, null));
        res.write(chunk({}, 'tool_calls'));
      } else {
        if (modelStep === toolSteps.length) assert.ok(JSON.stringify(body.messages).includes('history-file.plain'), 'Cached file result missing from provider request');
        modelStep++; res.write(chunk({role:'assistant',content:'Located the stored project notes and attachment paths.'}, null)); res.write(chunk({}, 'stop'));
      }
      res.end('data: [DONE]\n\n'); return;
    }
    res.writeHead(404); res.end('fixture_not_found');
  } catch (error) { fixtureError = error; log += `\nFixture upstream: ${error}`; if (!res.headersSent) res.writeHead(500); res.end('fixture failure'); }
});
try {
  await mkdir(join(temporary,'plugin/dist'), {recursive:true}); await mkdir(join(temporary,'workspace'));
  for (const file of ['package.json','openclaw.plugin.json']) await copyFile(join(root,'packages/openclaw-extension',file),join(temporary,'plugin',file));
  await build({entryPoints:[join(root,'packages/openclaw-extension/test/fixtures/native-history-model-plugin.ts')],absWorkingDir:root,bundle:true,format:'esm',platform:'node',external:['openclaw'],outfile:join(temporary,'plugin/dist/index.js')});
  upstream.listen(0,'127.0.0.1'); await once(upstream,'listening'); const apiPort=upstream.address().port;
  const probe=createServer(); probe.listen(0,'127.0.0.1'); await once(probe,'listening'); const port=probe.address().port; await new Promise(resolve=>probe.close(resolve));
  const transcriber=join(temporary,'transcribe.mjs'); await writeFile(transcriber, "console.log('Synthetic historical voice transcript');\n");
  const native={mode:'external',accountId:'a',endpoint:`http://127.0.0.1:${apiPort}`,token:{source:'env',provider:'default',id:'AGENT_WECHAT_TOKEN'},consumerStateDir:join(temporary,'consumer'),readPolicy:'on_inbound'};
  const config={gateway:{mode:'local',bind:'loopback',port,auth:{mode:'token',token:'synthetic-gateway-token-'.repeat(3)},controlUi:{enabled:false}},browser:{enabled:false},cron:{enabled:false},discovery:{mdns:{mode:'off'}},update:{checkOnStart:false},session:{dmScope:'per-channel-peer'},
    models:{mode:'merge',providers:{fixture:{baseUrl:`http://127.0.0.1:${apiPort}/model/v1`,api:'openai-completions',apiKey:'synthetic-model-key',models:[{id:'fixture-model',name:'Fixture',reasoning:false,input:['text','image'],contextWindow:32768,maxTokens:256,cost:{input:0,output:0,cacheRead:0,cacheWrite:0}}]}}},
    tools:{media:{models:[{type:'cli',command:process.execPath,args:[transcriber,'{{MediaPath}}'],capabilities:['audio']}],audio:{enabled:true},image:{enabled:false},video:{enabled:false}}},agents:{defaults:{workspace:join(temporary,'workspace'),model:{primary:'fixture/fixture-model',fallbacks:[]}}},
    plugins:{allow:['agent-wechat'],load:{paths:[join(temporary,'plugin')]},entries:{'agent-wechat':{enabled:true,config:{native}}}},channels:{'agent-wechat':{enabled:true,dmPolicy:'allowlist',allowFrom:['u1'],groupPolicy:'allowlist',groupAllowFrom:['u1']}}};
  const scenarios=[{id:'earlier-text',type:1,body:'Earlier group discussion'},...['image-a','file','image-file','video','voice','image-b','pending-image'].map(id=>({id,type:id==='image-file'||id==='file'?49:id.includes('image')?3:id==='video'?43:34,body:'<private transport XML>'})),{id:'trigger',type:1,mention:true,body:'Search earlier-project-note in this conversation.'},{id:'dm-image',type:3,threadId:'u1',body:'<private transport XML>'},...quoteScenarios];
  await writeFile(join(temporary,'scenarios.json'),JSON.stringify(scenarios)); const configPath=join(temporary,'config.json'); await writeFile(configPath,JSON.stringify(config),{mode:0o600});
  child=spawn(process.execPath,[join(sdk,'openclaw.mjs'),'gateway','run'],{env:{PATH:process.env.PATH,HOME:temporary,TMPDIR:temporary,OPENCLAW_STATE_DIR:join(temporary,'state'),OPENCLAW_CONFIG_PATH:configPath,NATIVE_HISTORY_FIXTURE_DIR:temporary,AGENT_WECHAT_TOKEN:token,NO_COLOR:'1'},stdio:['ignore','pipe','pipe']});
  for (const stream of [child.stdout,child.stderr]) stream.on('data',chunk=>{log=(log+chunk.toString()).slice(-131072);});
  let result;
  for(let attempt=0;attempt<900;attempt++){if(fixtureError)break;if(child.exitCode!==null||child.signalCode)throw Error(`Gateway exited: ${log}`);try{result=JSON.parse(await readFile(join(temporary,'result.json'),'utf8'));break;}catch{await sleep(100);}}
  const evidence=process.env.NATIVE_HISTORY_PROOF_DIR;
  if(evidence){await mkdir(evidence,{recursive:true,mode:0o700});await writeFile(join(evidence,'observations.json'),JSON.stringify({result,requests,queries,reads,transfers,lookups,log}),{mode:0o600});}
  assert.ifError(fixtureError); assert.ok(result,`Gateway deadline: ${log}`); assert.equal(result.error,undefined,`${result.error}; ${log}`);
  for(const scenario of scenarios)assert.equal(result.outcomes[scenario.id],['trigger','dm-image',...quoteScenarios.map(value=>value.id)].includes(scenario.id)?'completed':'ignored');
  assert.equal(requests.length,13); assert.equal(queries.length,2); assert.equal(queries[0].cursor,undefined);assert.equal(queries[1].cursor,'older-matches');assert.equal(reads.length,7);
  assert.deepEqual(lookups,['foreign-image','history-image','history-file']);assert.deepEqual(transfers,[{messageId:'history-image',assetId:null,quality:'best'}]);
  const group=result.contexts.find(ctx=>ctx.MessageSid==='wechat-native:a:trigger'),direct=result.contexts.find(ctx=>ctx.MessageSid==='wechat-native:a:dm-image');
  assert.equal(group.Attachments.length,6);assert.equal(group.InboundHistory.length,8);assert.match(group.BodyForAgent,/pending/);assert.match(group.BodyForAgent,/historical voice transcript/);assert.equal(group.WasMentioned,true);
  for(const attachment of group.Attachments)assert.ok(JSON.stringify(requests[0].messages).includes(attachment.path),`Missing group ${attachment.kind} path`);
  const trigger=result.items.find(item=>item.message.id==='trigger'),selected=trigger.dispatchManifest.assets.find(asset=>asset.role==='model_image');assert.equal(selected.messageId,'image-b');assert.equal(selected.contextRole,'history');assert.notEqual(selected.sha256,selected.sourceSha256);assert.equal(group.MediaPath,selected.path);
  const images=request=>request.messages.flatMap(message=>Array.isArray(message.content)?message.content.filter(block=>block.type==='image_url'):[]);
  assert.equal(images(requests[0]).length,1);const image=/^data:([^;]+);base64,(.+)$/.exec(images(requests[0])[0].image_url.url);assert.equal(createHash('sha256').update(Buffer.from(image[2],'base64')).digest('hex'),selected.sha256);
  assert.equal(direct.Attachments.length,1);assert.ok(JSON.stringify(requests[7].messages).includes(direct.Attachments[0].path));assert.equal(images(requests[7]).length,1);
  assert.equal(images(requests[5]).length,2);const retrieved=/^data:([^;]+);base64,(.+)$/.exec(images(requests[5])[1].image_url.url);assert.ok(retrieved);assert.equal(createHash('sha256').update(Buffer.from(retrieved[2],'base64')).digest('hex'),assets.get('history-image').asset.sha256);
  for(let index=0;index<quoteScenarios.length;index++) {
    const scenario=quoteScenarios[index],request=requests[8+index];
    const providerText=request.messages.flatMap(message=>typeof message.content==='string'?[message.content]:Array.isArray(message.content)?message.content.filter(block=>block.type==='text').map(block=>block.text):[]).join('\n');
    assert.ok(providerText.includes(scenario.body),'Quote comment or forwarded bundle missing from provider');
    if(scenario.id==='quote-text')assert.ok(JSON.stringify(request.messages).includes('Quoted original stored text'));
    if(scenario.id==='quote-embedded-text')assert.ok(JSON.stringify(request.messages).includes('Quoted embedded text with no original row'));
    const isImage=['quote-image','quote-embedded-image'].includes(scenario.id);
    const context=result.contexts.find(ctx=>ctx.MessageSid===`wechat-native:a:${scenario.id}`);
    assert.equal(!!context.MediaPath,isImage);
    if(isImage) {
      const expected=assets.get(scenario.id==='quote-image'?'quoted-original-image':scenario.id).asset.sha256;
      const matches=images(request).filter(block=>{const encoded=/^data:([^;]+);base64,(.+)$/.exec(block.image_url.url);return encoded&&createHash('sha256').update(Buffer.from(encoded[2],'base64')).digest('hex')===expected;});
      assert.equal(matches.length,1,'Exact new quoted image must reach provider once, independently of prior history');
      const item=result.items.find(item=>item.message.id===scenario.id);
      assert.equal(item.dispatchManifest.assets[0].messageId,scenario.id);
      if(scenario.id==='quote-image')assert.equal(item.dispatchManifest.assets[0].sourceMessageId,'quoted-original-image');
    }
  }
  const mediaTool=requests[0].tools.find(tool=>tool.function.name==='wechat_media');assert.ok(mediaTool);assert.equal(mediaTool.function.parameters.properties.threadId,undefined);assert.ok(!JSON.stringify(requests.map(request=>request.tools)).includes('wechat_chats'));
  assert.ok(!JSON.stringify(requests).includes('private transport XML'));assert.equal(result.items.find(item=>item.message.id==='image-a').message.mentions.self,false);assert.equal(result.items.find(item=>item.message.id==='image-a').readPending,undefined);
  console.log(JSON.stringify({gatewayVersion:'2026.10.2',groupContextSurvivedRestart:true,groupOriginalAttachmentPaths:6,groupVisionImages:1,fileWithImageExtensionNotHydrated:true,latestPreparedImageHashVerified:true,directImagePathAndVision:true,realSearchToolCalls:queries.length,searchPaginationReachedProvider:true,currentThreadOnly:true,foreignAttachmentRejectedBeforeAccess:true,downloadedImageHashReachedProvider:true,cachedFilePathReachedProvider:true,attachmentTransferRequests:transfers.length,conversationAccess:false,quotedTextReachedProvider:true,quotedImageHashReachedProvider:true,embeddedQuoteTextAndImageWithoutOriginal:true,forwardedBundleReachedProvider:true,reads:reads.length}));
} finally {
  if(child&&child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await Promise.race([once(child,'exit'),sleep(5000)]);if(child.exitCode===null)child.kill('SIGKILL');}
  for(const stream of streams)stream.destroy();await new Promise(resolve=>upstream.close(resolve));upstream.closeAllConnections();await rm(temporary,{recursive:true,force:true});
}
