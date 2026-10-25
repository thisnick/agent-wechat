import test from 'node:test';
import assert from 'node:assert/strict';
import { openAsBlob } from 'node:fs';
import { mkdtemp, writeFile, truncate, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { NativeApiError, NativeWeChatClient, validateNativeBaseUrl, isNativeTombstone, NATIVE_MAX_SOURCE_BYTES, NATIVE_MAX_ORDINARY_SOURCE_BYTES, NATIVE_MAX_VIDEO_SOURCE_BYTES } from '../src/native-client.ts';
const options = { baseUrl: 'http://127.0.0.1:6174', token: 'fixture-secret' };
test('group mutations preserve exact methods, stable keys and operation identity without transport retries', async () => {
  const calls=[];
  const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{
    const path=new URL(url).pathname; calls.push([path,init.method,init.headers.get('Idempotency-Key'),JSON.parse(init.body)]);
    const action=path.endsWith('/leave')?'leave':path.endsWith('/members')?(init.method==='DELETE'?'remove_members':'add_members'):path.endsWith('/my-name')?'my_name':path.endsWith('/announcement')?'clear_announcement':path.endsWith('/notifications')?'mute':init.method==='PATCH'?'rename':'create';
    return Response.json({id:'operation',accountId:'a',kind:'group_mutation',state:'queued',result:{action,targetId:action==='create'?null:'room@chatroom'}});
  }});
  await client.createGroup('a',{members:['peer'],name:'Synthetic'},'key');
  await client.renameGroup('a','room@chatroom','New','key2');
  await client.addGroupMembers('a','room@chatroom',['peer'],'key3');
  await client.removeGroupMembers('a','room@chatroom',['peer'],'key4');
  await client.setMyGroupName('a','room@chatroom','Me','key5');
  await client.clearGroupAnnouncement('a','room@chatroom','key6');
  await client.setThreadMuted('a','room@chatroom',false,'key7');
  await client.leaveGroup('a','room@chatroom','key8');
  assert.deepEqual(calls.map(v=>v[1]),['POST','PATCH','POST','DELETE','PUT','DELETE','PUT','POST']);
  assert.ok(calls[1][0].includes('room%40chatroom'));assert.deepEqual(calls[6][3],{muted:false});
  for(const key of ['', 'x'.repeat(201),'bad\nkey'])await assert.rejects(client.createGroup('a',{members:['peer']},key));
  assert.equal(calls.length,8);
  let tries=0; const lost=new NativeWeChatClient({...options,fetch:async()=>{tries++;throw Error('lost');}});
  await assert.rejects(lost.leaveGroup('a','room@chatroom','same'),/lost/);assert.equal(tries,1);
  const foreign=new NativeWeChatClient({...options,fetch:async()=>Response.json({id:'o',accountId:'other',kind:'group_mutation',result:{action:'create',targetId:null}})});
  await assert.rejects(foreign.createGroup('a',{members:['peer']},'k'),/identity/);
});
test('single group member reads retain unconfirmed absence and encode both selectors',async()=>{
  const client=new NativeWeChatClient({...options,fetch:async(url)=>{assert.match(String(url),/groups\/room%40chatroom\/members\/peer%2Fexact$/);return Response.json({state:'unconfirmed',member:null,coverage:'cached'});}});
  const result=await client.groupMember('a','room@chatroom','peer/exact');assert.equal(result.state,'unconfirmed');assert.equal(result.member,null);
});
test('video send quality and its separate 50 MiB source bound are exposed without changing other source limits', async () => {
  assert.equal(NATIVE_MAX_VIDEO_SOURCE_BYTES, 50 * 1024 * 1024); assert.equal(NATIVE_MAX_SOURCE_BYTES, 512 * 1024 * 1024);
  assert.equal(NATIVE_MAX_ORDINARY_SOURCE_BYTES, 25 * 1024 * 1024 - 17);
  const seen = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    assert.equal(new URL(url).pathname, '/v1/accounts/a/sends'); assert.equal(init.method, 'POST');
    const body = JSON.parse(init.body); assert.equal(init.headers.get('Idempotency-Key'), `video-key-${body.videoQuality}`); seen.push(body);
    return Response.json({ id: 'video-job', state: 'queued' }, { status: 202 });
  } });
  for (const videoQuality of ['normal', 'original']) await client.sendMedia('a', { threadId: 'peer', kind: 'video', sourceId: 'owned', videoQuality }, `video-key-${videoQuality}`);
  assert.deepEqual(seen.map(value => value.videoQuality), ['normal', 'original']);
});
test('native transport rejects plaintext remote URLs and embedded credentials', () => {
  for (const url of ['http://example.net', 'https://user:secret@example.net', 'https://example.net?token=x', 'file:///tmp/store']) assert.throws(() => validateNativeBaseUrl(url));
  assert.equal(validateNativeBaseUrl('https://example.net/prefix').pathname, '/prefix/');
  assert.throws(() => new NativeWeChatClient({ ...options, token: '' }), /token/);
});
test('Bearer auth and encoded string IDs are preserved without query credentials or redirects', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    assert.equal(String(url), 'http://127.0.0.1:6174/v1/accounts/a/threads/room%40chatroom/messages');
    assert.equal(init.headers.get('Authorization'), 'Bearer fixture-secret');
    assert.equal(init.redirect, 'error');
    return Response.json({ items: [{ id: 'm_1', accountId: 'a', threadId: 'room@chatroom', serverId: '18446744073709551614' }], nextCursor: null });
  } });
  assert.equal((await client.messages('a', 'room@chatroom')).items[0].serverId, '18446744073709551614');
});
test('unsafe numeric server IDs and cross-account message resources are rejected', async () => {
  for (const message of [{ id: 'm', accountId: 'a', threadId: 't', serverId: 9007199254740992 }, { id: 'm', accountId: 'other', threadId: 't', serverId: '1' }]) {
    const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ items: [message] }) });
    await assert.rejects(client.messages('a', 't'), /identity/);
  }
});
test('account-wide search preserves cross-thread results, query and cursor with bearer authentication', async () => {
  const requests = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    requests.push(new URL(url)); assert.equal(init.headers.get('Authorization'), 'Bearer fixture-secret');
    return Response.json({ items: ['peer', 'group@chatroom'].map((threadId, index) => ({ id: `m${index}`, accountId: 'a', threadId, serverId: '9007199254740999' })), nextCursor: 'opaque' });
  } });
  const page = await client.searchMessages('a', { query: 'project note', limit: 2, cursor: 'earlier' });
  assert.deepEqual(page.items.map(item => item.threadId), ['peer', 'group@chatroom']);
  assert.equal(requests[0].pathname, '/v1/accounts/a/messages');
  assert.deepEqual(Object.fromEntries(requests[0].searchParams), { query: 'project note', limit: '2', cursor: 'earlier' });
  for (const message of [{ id: 'm', accountId: 'other', threadId: 't' }, { id: 'm', accountId: 'a', threadId: '' }, { id: 'm', accountId: 'a', threadId: 't', serverId: 9007199254740992 }]) {
    const foreign = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ items: [message] }) });
    await assert.rejects(foreign.searchMessages('a', { query: 'note' }), /archive message identity/);
  }
});
test('send uses caller idempotency key and read marks the whole thread', async () => {
  const requests = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => { requests.push({ url: String(url), init }); return Response.json({ state: 'accepted' }); } });
  await client.sendText('a', 't', 'fixture', 'send-key');
  await client.markRead('a', 't', '7');
  assert.equal(requests[0].init.headers.get('Idempotency-Key'), 'send-key');
  assert.deepEqual(JSON.parse(requests[0].init.body), { threadId: 't', text: 'fixture' });
  assert.deepEqual(JSON.parse(requests[1].init.body), { expectedVersion: '7' });
  assert.throws(() => client.markRead('a', 't', []));
});
test('quoted text preserves exact source IDs and rejects invalid references before transport', async () => {
  const requests=[];
  const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{requests.push({url:String(url),init});return Response.json({id:'quoted-send',state:'queued'});}});
  const quote={messageId:'opaque/source?=1',serverId:'18446744073709551615'};
  await client.sendText('a','room@chatroom','Reply','quote-key',quote);
  assert.equal(requests.length,1);
  assert.equal(requests[0].url,'http://127.0.0.1:6174/v1/accounts/a/sends');
  assert.equal(requests[0].init.headers.get('Idempotency-Key'),'quote-key');
  assert.deepEqual(JSON.parse(requests[0].init.body),{kind:'text',threadId:'room@chatroom',text:'Reply',quote});
  for(const invalid of [null,[],{}, {messageId:'',serverId:'42'}, {messageId:'source',serverId:9007199254740992},
    ...['0','01','-1','1.0','18446744073709551616'].map(serverId=>({messageId:'source',serverId})),
    {...quote,content:'caller-selected source'}, {...quote,threadId:'another-chat'}]) {
    assert.throws(()=>client.sendText('a','room@chatroom','Reply','quote-key',invalid),/quote message identity/);
  }
  assert.equal(requests.length,1);
  let attempts=0;
  const lost=new NativeWeChatClient({...options,fetch:async()=>{attempts++;throw Error('lost response');}});
  await assert.rejects(lost.sendText('a','room@chatroom','Reply','same-key',quote),/lost response/);
  assert.equal(attempts,1);
});

test('recall sends an exact decimal identity and caller key once without cancelling its operation', async () => {
  const requests=[];
  const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{requests.push({url:String(url),init});return Response.json({id:'recall_operation',accountId:'a',kind:'message_recall',state:'queued',result:{}});}});
  await client.recallMessage('a','msg/source?=','18446744073709551614','stable-recall-key');
  assert.equal(requests.length,1);assert.match(requests[0].url,/messages\/msg%2Fsource%3F%3D\/recall$/);
  assert.equal(requests[0].init.headers.get('Idempotency-Key'),'stable-recall-key');
  assert.deepEqual(JSON.parse(requests[0].init.body),{serverId:'18446744073709551614'});
  for(const serverId of ['0','01','9007199254740993.0',9007199254740992,'18446744073709551616'])await assert.rejects(client.recallMessage('a','m',serverId,'key'));
  for(const key of ['', 'x'.repeat(201), 'bad\r\nkey'])await assert.rejects(client.recallMessage('a','m','3',key));
  assert.equal(requests.length,1);
});
test('recall transport rejects foreign operation identities and does not retry a lost response', async () => {
  for(const operation of [{id:'o',accountId:'other',kind:'message_recall',state:'queued'},{id:'o',accountId:'a',kind:'login',state:'queued'}]){
    const client=new NativeWeChatClient({...options,fetch:async()=>Response.json(operation)});
    await assert.rejects(client.recallMessage('a','m','3','key'),/operation identity/);
  }
  let attempts=0;const client=new NativeWeChatClient({...options,fetch:async()=>{attempts++;throw Error('lost response');}});
  await assert.rejects(client.recallMessage('a','m','3','key'),/lost response/);assert.equal(attempts,1);
});
test('derived recall notices preserve optional unknowns and reject malformed text', async () => {
  const observedAt='2026-10-23T00:00:00Z';
  const recall={observedAt,replacementText:'Localized notice <text> & detail'};
  const message={id:'m',accountId:'a',threadId:'t',serverId:'42',senderId:'original-author',direction:'outgoing',type:10000,recall};
  const valid=new NativeWeChatClient({...options,fetch:async()=>Response.json(message)});
  assert.deepEqual((await valid.message('a','m')).recall,recall);
  for(const unknown of [{observedAt},{observedAt,replacementText:null}]) {
    const client=new NativeWeChatClient({...options,fetch:async()=>Response.json({...message,recall:unknown})});
    assert.deepEqual((await client.message('a','m')).recall,unknown);
  }
  for(const change of [{replacementText:42},{replacementText:''},{replacementText:'bad\u0001notice'},{replacementText:'🧪'.repeat(4097)}]) {
    const invalid={...message,recall:{...recall,...change}};
    const client=new NativeWeChatClient({...options,fetch:async(url)=>{
      const path=new URL(url).pathname;
      return Response.json(path.endsWith('/m') ? invalid : {items:[invalid],nextCursor:null});
    }});
    await assert.rejects(client.message('a','m'),/recall metadata/);
    await assert.rejects(client.messages('a','t'),/recall metadata/);
    await assert.rejects(client.searchMessages('a',{query:'notice'}),/recall metadata/);
    const event={cursor:'1',kind:'message.recalled',createdAt:observedAt,data:{threadId:'t',serverId:'42',...invalid.recall}};
    const events=new NativeWeChatClient({...options,fetch:async()=>Response.json({items:[event],nextCursor:'1'})});
    await assert.rejects(events.events('a'),/recall metadata/);
    const stream=new NativeWeChatClient({...options,fetch:async()=>new Response(`data: ${JSON.stringify(event)}\n\n`,{headers:{'Content-Type':'text/event-stream'}})});
    await assert.rejects(async()=>{for await(const _event of stream.watch('a')) assert.fail('invalid recall must not be yielded');},/recall metadata/);
  }
});
test('structured failures expose safe error codes but never raw server messages', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ error: { code: 'read_state_conflict', message: 'private source content', retryable: false } }, { status: 409 }) });
  await assert.rejects(client.markRead('a', 't', '7'), error => error instanceof NativeApiError && error.status === 409 && error.code === 'read_state_conflict' && !error.message.includes('private'));
});
test('URL shares use the typed send route, caller key and cancellation signal exactly once', async () => {
  const calls = [], controller = new AbortController();
  const request = { kind: 'share', threadId: 'room@chatroom', title: 'Shared title', url: 'https://example.invalid/article', thumbnailUrl: 'https://example.invalid/p.png' };
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => { calls.push({ url: String(url), init }); return Response.json({ id: 'send-share', state: 'queued' }); } });
  const receipt = await client.sendShare('a', request, 'stable-key', controller.signal);
  assert.equal(receipt.id, 'send-share'); assert.equal(calls.length, 1); assert.match(calls[0].url, /\/accounts\/a\/sends$/);
  assert.equal(calls[0].init.headers.get('Idempotency-Key'), 'stable-key'); assert.deepEqual(JSON.parse(calls[0].init.body), request);
  for (const key of ['', 'x'.repeat(201), 'bad\nkey']) assert.throws(() => client.sendShare('a', request, key));
});
test('event pages enforce the server maximum of 200', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ items: [], nextCursor: '0' }) });
  await client.events('a', '0', 200);
  assert.throws(() => client.events('a', '0', 201));
});
test('SSE accepts UTF-8 chunk boundaries, heartbeat comments and decimal cursors', async () => {
  const wire = new TextEncoder().encode(': ping\r\n\r\nid: 9007199254740999\r\nevent: message\r\ndata: {"cursor":"9007199254740999","kind":"message","data":{"text":"测试"},"createdAt":1}\r\n\r\n');
  const body = new ReadableStream({ start(controller) { for (let index = 0; index < wire.length; index += 3) controller.enqueue(wire.slice(index, index + 3)); controller.close(); } });
  const client = new NativeWeChatClient({ ...options, fetch: async (_url, init) => {
    assert.equal(init.headers.get('Authorization'), 'Bearer fixture-secret');
    return new Response(body, { headers: { 'Content-Type': 'text/event-stream' } });
  } });
  const values = []; for await (const value of client.watch('a')) values.push(value);
  assert.equal(values.length, 1); assert.equal(values[0].cursor, '9007199254740999'); assert.equal(values[0].data.text, '测试');
});
test('stalled SSE is aborted and a later watch replays from the last durable cursor', async () => {
  const encode = cursor => new TextEncoder().encode(`id: ${cursor}\ndata: ${JSON.stringify({cursor,kind:'message.committed',data:{messageId:'fixture-'+cursor},createdAt:1})}\n\n`);
  const requests=[];let cancelled=false;
  const client=new NativeWeChatClient({...options,streamIdleTimeoutMs:20,fetch:async(url,init)=>{
    requests.push({url:new URL(url),signal:init.signal});
    if(requests.length===1)return new Response(new ReadableStream({start(c){c.enqueue(encode('1'));},cancel(){cancelled=true;}}),{headers:{'content-type':'text/event-stream'}});
    return new Response(encode('2'),{headers:{'content-type':'text/event-stream'}});
  }});
  const first=client.watch('a','0');assert.equal((await first.next()).value.cursor,'1');
  await assert.rejects(first.next(),/idle timeout/);assert.equal(cancelled,true);assert.equal(requests[0].signal.aborted,true);
  const replay=[];for await(const event of client.watch('a','1'))replay.push(event.cursor);
  assert.deepEqual(replay,['2']);assert.equal(requests[1].url.searchParams.get('after'),'1');
});
test('SSE keepalive comments reset idle waits and slow consumers do not expire the connection', async () => {
  let timer;let cancelled=false;
  const encode=cursor=>new TextEncoder().encode(`data: ${JSON.stringify({cursor,kind:'fixture',data:{},createdAt:1})}\n\n`);
  const body=new ReadableStream({start(c){let ticks=0;timer=setInterval(()=>{ticks++;c.enqueue(new TextEncoder().encode(': keepalive\n\n'));if(ticks===7)c.enqueue(encode('1'));if(ticks===15)c.enqueue(encode('2'));},10);},cancel(){clearInterval(timer);cancelled=true;}});
  const client=new NativeWeChatClient({...options,streamIdleTimeoutMs:40,fetch:async()=>new Response(body,{headers:{'content-type':'text/event-stream'}})});
  const iterator=client.watch('a');
  try{assert.equal((await iterator.next()).value.cursor,'1');await new Promise(resolve=>setTimeout(resolve,85));assert.equal((await iterator.next()).value.cursor,'2');}
  finally{await iterator.return();clearInterval(timer);}
  assert.equal(cancelled,true);
});
test('SSE connection waits and caller cancellation are bounded before headers arrive', async () => {
  let requestSignal;
  const client=new NativeWeChatClient({...options,timeoutMs:20,fetch:async(_url,init)=>{requestSignal=init.signal;return new Promise(()=>{});}});
  await assert.rejects(client.watch('a').next(),/connection timeout/);assert.equal(requestSignal.aborted,true);
  const caller=new AbortController();const waiting=client.watch('a','0',caller.signal).next();caller.abort();await assert.rejects(waiting,error=>error.name==='AbortError');
  for(const streamIdleTimeoutMs of [0,-1,NaN,Infinity,300001])assert.throws(()=>new NativeWeChatClient({...options,streamIdleTimeoutMs}),/idle timeout/);
});
test('invalid event cursors and oversized unterminated SSE frames fail closed', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => new Response('x'.repeat(1024 * 1024 + 1), { headers: { 'Content-Type': 'text/event-stream' } }) });
  await assert.rejects(async () => { for await (const _ of client.watch('a')) {} }, /bound/);
  await assert.rejects(async () => { for await (const _ of client.watch('a', 'latest')) {} }, /decimal/);
});
test('PNG login QR is bounded and fetched with header authentication', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async (_url, init) => { assert.equal(init.headers.get('Authorization'), 'Bearer fixture-secret'); return new Response(new Uint8Array([137, 80, 78, 71]), { headers: { 'content-type': 'image/png' } }); } });
  assert.equal((await client.qr('a', 'o')).byteLength, 4);
});
test('expired SSE cursor is an explicit terminal recovery condition instead of jumping to now', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => new Response('event: error\ndata: {"error":{"code":"event_cursor_expired"}}\n\n', { headers: { 'content-type': 'text/event-stream' } }) });
  await assert.rejects(async () => { for await (const _ of client.watch('a', '7')) {} }, error => error instanceof NativeApiError && error.status === 410 && error.code === 'event_cursor_expired' && !error.retryable);
});

test('native HTTPS refuses Node global certificate-verification bypass', () => {
  const previous = process.env.NODE_TLS_REJECT_UNAUTHORIZED;
  process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
  try { assert.throws(() => new NativeWeChatClient({ ...options, baseUrl: 'https://daemon.example.net' }), /verification disabled/); }
  finally { if (previous === undefined) delete process.env.NODE_TLS_REJECT_UNAUTHORIZED; else process.env.NODE_TLS_REJECT_UNAUTHORIZED = previous; }
});
test('logout and explicit local forgetting preserve distinct durable request identities', async () => {
  const requests=[];const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{requests.push({url:String(url),init});return Response.json({id:'operation_fixture',state:'queued'});}});
  await client.logout('a','logout-key');await client.forgetCredentials('a','forget-key',true);
  assert.match(requests[0].url,/\/logout$/);assert.equal(requests[0].init.headers.get('Idempotency-Key'),'logout-key');
  assert.match(requests[1].url,/\/forget-credentials$/);assert.deepEqual(JSON.parse(requests[1].init.body),{confirm:true});
  assert.throws(()=>client.forgetCredentials('a','forget-key',false));
});
test('media source upload uses multipart bytes without a caller path or JSON content type', async () => {
  const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{assert.match(String(url),/\/uploads$/);assert.equal(init.headers.get('content-type'),null);assert.ok(init.body instanceof FormData);assert.equal(init.body.get('file').name,'fixture.bin');assert.deepEqual(new Uint8Array(await init.body.get('file').arrayBuffer()),new Uint8Array([1,2,3]));return Response.json({id:'upload_fixture'});}});
  assert.equal((await client.upload('a',new Uint8Array([1,2,3]),'fixture.bin')).id,'upload_fixture');
  await assert.rejects(client.upload('a',new Uint8Array([1]),'../fixture.bin'));
});
test('cached media range reads preserve bytes/status and reject oversized bodies', async () => {
  const client=new NativeWeChatClient({...options,fetch:async(_url,init)=>{assert.equal(init.headers.get('Range'),'bytes=1-2');return new Response(new Uint8Array([2,3]),{status:206,headers:{'content-type':'application/octet-stream','content-length':'2','content-range':'bytes 1-2/3','etag':'fixture'}});}});
  const content=await client.mediaContent('a','asset_fixture',{range:'bytes=1-2',maxBytes:2});assert.equal(content.status,206);assert.equal(content.contentRange,'bytes 1-2/3');assert.deepEqual(content.bytes,new Uint8Array([2,3]));
  await assert.rejects(client.mediaContent('a','asset_fixture',{range:'bytes=1-2',maxBytes:1}),/bound/);
});
test('derived media reads use an explicit representation and retain the source hash', async () => {
  const sourceHash='a'.repeat(64);
  const client=new NativeWeChatClient({...options,fetch:async url=>{assert.equal(new URL(url).searchParams.get('representation'),'preview');return new Response(new Uint8Array([1,2]),{headers:{'content-type':'image/jpeg','content-length':'2','x-source-sha256':sourceHash}});}});
  const content=await client.mediaContent('a','asset_fixture',{representation:'preview',maxBytes:2});
  assert.equal(content.sourceSha256,sourceHash);assert.equal(content.contentType,'image/jpeg');assert.deepEqual(content.bytes,new Uint8Array([1,2]));
  await assert.rejects(client.mediaContent('a','asset_fixture',{representation:'arbitrary'}),/bounded content/);
});

test('native resource pagination retains tombstones and binds continuation query without losing IDs', async () => {
  const tombstone={id:'deleted-message',deleted:true,version:'2'};let requested;
  const client=new NativeWeChatClient({...options,fetch:async url=>{requested=new URL(url);return Response.json({items:[tombstone],nextCursor:null,coverage:{},sourceGeneration:'g'});}});
  const page=await client.messages('a','t',{cursor:'cursor+/=',limit:17});assert.ok(isNativeTombstone(page.items[0]));assert.equal(requested.searchParams.get('cursor'),'cursor+/=');assert.equal(requested.searchParams.get('limit'),'17');assert.equal(page.items[0].id,'deleted-message');
  await assert.rejects(async()=>client.contacts('a',{limit:201}));assert.equal(isNativeTombstone({id:'x',deleted:false,version:'1'}),false);
});
test('whole-thread reads require a bounded optimistic version', async () => {
 const client=new NativeWeChatClient({...options,fetch:async(_url,init)=>{assert.deepEqual(JSON.parse(init.body),{expectedVersion:'7'});return Response.json({version:'8'});}});await client.markRead('a','t','7');
 for(const invalid of ['', 'x'.repeat(129), ['old-message-selection']]) assert.throws(()=>client.markRead('a','t',invalid));
});
test('source staging accepts the exact file limit as a disk-backed Blob and refuses one extra byte before transport', async t => {
  const directory=await mkdtemp(join(tmpdir(),'native-sdk-source-'));t.after(()=>rm(directory,{recursive:true,force:true}));
  const path=join(directory,'source');await writeFile(path,new Uint8Array(1));await truncate(path,NATIVE_MAX_SOURCE_BYTES);
  assert.equal(NATIVE_MAX_SOURCE_BYTES,512*1024*1024);assert.equal(NATIVE_MAX_ORDINARY_SOURCE_BYTES,25*1024*1024-17);
  let uploads=0;
  const client=new NativeWeChatClient({...options,fetch:async(url,init)=>{
    uploads++;assert.equal(new URL(url).pathname,'/v1/accounts/a/uploads');assert.equal(init.method,'POST');
    const file=init.body.get('file');assert.equal(file.size,NATIVE_MAX_SOURCE_BYTES);assert.equal(file.name,'synthetic.bin');
    return Response.json({id:'staged-source',byteLength:file.size});
  }});
  const source=await client.upload('a',await openAsBlob(path),'synthetic.bin');
  assert.equal(source.id,'staged-source');assert.equal(uploads,1);assert.ok(NATIVE_MAX_ORDINARY_SOURCE_BYTES<NATIVE_MAX_SOURCE_BYTES);
  await truncate(path,NATIVE_MAX_SOURCE_BYTES+1);
  await assert.rejects(client.upload('a',await openAsBlob(path),'synthetic.bin'),/bounded media upload/);assert.equal(uploads,1);
});

test('streamed media keeps range headers and cancels an over-limit or incomplete response without buffering it', async () => {
  for(const mode of ['range','overflow','short']){
    let cancelled=0;
    const client=new NativeWeChatClient({...options,fetch:async(_url,init)=>{
      if(mode==='range')assert.equal(new Headers(init.headers).get('Range'),'bytes=4-6');
      const body=new ReadableStream({start(controller){controller.enqueue(new Uint8Array([1,2,3]));if(mode==='overflow')controller.enqueue(new Uint8Array([4]));else controller.close();},cancel(){cancelled++;}});
      return new Response(body,{status:mode==='range'?206:200,headers:{'content-type':'application/pdf',...(mode==='range'?{'content-range':'bytes 4-6/536870912','content-length':'3'}:mode==='short'?{'content-length':'4'}:{})}});
    }});
    const response=await client.mediaContentStream('a','file',{maxBytes:mode==='short'?4:3,...(mode==='range'?{range:'bytes=4-6'}:{})});
    let size=0;const consume=async()=>{for await(const chunk of response.stream)size+=chunk.byteLength;};
    if(mode==='range'){await consume();assert.equal(size,3);assert.equal(response.contentRange,'bytes 4-6/536870912');assert.equal(response.status,206);}
    else await assert.rejects(consume(),mode==='overflow'?/exceeds its bound/:/length mismatch/);
    if(mode==='overflow')assert.equal(cancelled,1);
  }
});

test('media transfers have a separate deadline and leave normal JSON requests unchanged', async () => {
  const client=new NativeWeChatClient({...options,timeoutMs:1,mediaTimeoutMs:100,fetch:async(_url,init)=>{
    await new Promise(resolve=>setTimeout(resolve,10));init.signal.throwIfAborted();
    return new URL(_url).pathname.endsWith('/uploads')?Response.json({id:'source'}):new Response(new Uint8Array([1]),{headers:{'content-length':'1'}});
  }});
  assert.equal((await client.upload('a',new Blob([new Uint8Array([1])]),'small.bin')).id,'source');
  assert.equal((await client.mediaContent('a','asset',{maxBytes:1})).bytes.length,1);
  await assert.rejects(client.status(),error=>error.name==='TimeoutError');
  for(const mediaTimeoutMs of [0,-1,3600001,NaN])assert.throws(()=>new NativeWeChatClient({...options,mediaTimeoutMs}),/media timeout/);
});
test('disk-backed or browser Blob uploads honor the requested MIME without converting to bytes', async () => {
  const blob=new Blob([new Uint8Array([1])],{type:'text/plain'});
  blob.arrayBuffer=async()=>assert.fail('Blob uploads must stay lazy');
  const client=new NativeWeChatClient({...options,fetch:async(_url,init)=>{assert.equal(init.body.get('file').type,'application/pdf');return Response.json({id:'source'});}});
  assert.equal((await client.upload('a',blob,'fixture.pdf','application/pdf')).id,'source');
});
test('cancelling a media stream releases a blocked reader without another request', async () => {
  let requests=0,cancelled=0;
  const client=new NativeWeChatClient({...options,fetch:async()=>{requests++;return new Response(new ReadableStream({start(controller){controller.enqueue(new Uint8Array([1]));},cancel(){cancelled++;}}));}});
  const content=await client.mediaContentStream('a','asset',{maxBytes:4}),iterator=content.stream[Symbol.asyncIterator]();
  assert.equal((await iterator.next()).value[0],1);
  const blocked=iterator.next();const rejection=assert.rejects(blocked,error=>error.name==='AbortError');await content.cancel();await rejection;
  assert.equal(requests,1);assert.equal(cancelled,1);
});
test('missing positive-length bodies fail and media streams cannot be replayed or leak readers after early exit', async () => {
  const missing=new NativeWeChatClient({...options,fetch:async()=>new Response(null,{headers:{'content-length':'1'}})});
  await assert.rejects(missing.mediaContentStream('a','asset'),/length mismatch/);
  for(const beforeRead of [true,false]){
    let requests=0,cancelled=0,response;
    const client=new NativeWeChatClient({...options,fetch:async()=>{requests++;response=new Response(new ReadableStream({start(controller){controller.enqueue(new Uint8Array([1]));},cancel(){cancelled++;}}));return response;}});
    const content=await client.mediaContentStream('a','asset',{maxBytes:1});
    if(beforeRead){await content.cancel();await content.cancel();await assert.rejects(async()=>{for await(const _chunk of content.stream){}},error=>error.name==='AbortError');}
    else {for await(const _chunk of content.stream)break;assert.throws(()=>content.stream[Symbol.asyncIterator](),/already consumed/);}
    assert.equal(response.body.locked,false);assert.equal(cancelled,1);assert.equal(requests,1);
  }
});

test('local media writeback repair is bounded and does not submit a send or transfer', async () => {
  let calls = 0;
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    calls++;
    assert.equal(String(url), 'http://127.0.0.1:6174/v1/accounts/a/media-writeback/repair');
    assert.equal(init.method, 'POST'); assert.deepEqual(JSON.parse(init.body), { limit: 25 });
    return Response.json({ id: 'repair', accountId: 'a', kind: 'media-writeback', state: 'queued', result: {} });
  } });
  for (const limit of [0,101,1.5,NaN]) assert.throws(() => client.repairMediaWriteback('a', limit), /limit/);
  assert.equal(calls,0); assert.equal((await client.repairMediaWriteback('a')).id,'repair'); assert.equal(calls,1);
});


test('official profile preparation uses daemon-host paths and a global admin resource', async () => {
  const client = new NativeWeChatClient({ baseUrl: 'http://127.0.0.1:6174', token: 'synthetic-token', fetch: async (url, init) => {
    assert.equal(String(url), 'http://127.0.0.1:6174/v1/profiles/official');
    assert.equal(init.method, 'POST');
    assert.deepEqual(JSON.parse(init.body), { path: '/server/official-home' });
    return new Response(JSON.stringify({ accountId: 'official-fixture', profilePath: '/server/official-home', storageMode: 'official', savedDeviceLoginAvailable: true }));
  }});
  assert.equal((await client.prepareOfficialProfile({ path: '/server/official-home' })).accountId, 'official-fixture');
});

test('message and list reads preserve own and quoted server IDs without quote requests', async () => {
  const quote={serverId:'18446744073709551615',type:49,state:'embedded',senderId:'u',direction:'incoming',appMessageType:5,
    embeddedText:'[Link] Article\nhttps://example.com/article',embeddedImage:false};
  const message={id:'parent',accountId:'a',threadId:'t',serverId:'9007199254740993',type:49,appMessageType:57,body:'Comment',quote};
  const requests=[];
  const client=new NativeWeChatClient({...options,fetch:async(url)=>{
    requests.push(String(url));
    return Response.json(new URL(url).pathname.endsWith('/parent') ? message : {items:[message],nextCursor:null});
  }});
  const results=[await client.message('a','parent'),(await client.messages('a','t')).items[0],(await client.searchMessages('a',{query:'Article'})).items[0]];
  for(const item of results) {
    assert.equal(item.serverId,message.serverId);
    assert.deepEqual(item.quote,quote);
  }
  assert.deepEqual(requests,[
    'http://127.0.0.1:6174/v1/accounts/a/messages/parent',
    'http://127.0.0.1:6174/v1/accounts/a/threads/t/messages',
    'http://127.0.0.1:6174/v1/accounts/a/messages?query=Article',
  ]);
});

test('completed upload reuse is an explicit option on the original job retry', async () => {
  const bodies = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    const body = JSON.parse(init.body);
    assert.equal(String(url), 'http://127.0.0.1:6174/v1/accounts/a/jobs/original-job/' + (body.reuseCompletedUpload ? 'retry-completed-upload' : 'retry'));
    assert.equal(init.method, 'POST'); bodies.push(body);
    return Response.json({ id: 'original-job', state: 'queued', attempts: 1 });
  } });
  await client.retryJob('a', 'original-job');
  await client.retryJob('a', 'original-job', { reuseCompletedUpload: true });
  assert.deepEqual(bodies, [{}, { reuseCompletedUpload: true }]);
});

test('an older daemon cannot turn completed-upload reuse into a normal retry', async () => {
  let calls = 0;
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    calls++; assert.ok(String(url).endsWith('/jobs/original-job/retry-completed-upload'));
    return Response.json({ error: { code: 'not_found', retryable: false } }, { status: 404 });
  } });
  await assert.rejects(client.retryJob('a', 'original-job', { reuseCompletedUpload: true }), error => error instanceof NativeApiError && error.status === 404);
  assert.equal(calls, 1);
});

test('invalid retry options never fall back to an ordinary job retry', () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => assert.fail('An invalid operator option must not start work') });
  for (const invalid of [null, [], { reuseCompletedUpload: 1 }, { reuseCompletedUpload: 'true' }, { reuseCompletedUpload: null }, { skipReadback: true }]) {
    assert.throws(() => client.retryJob('a', 'original-job', invalid), /Invalid media retry options/);
  }
});
test('default browser fetch keeps the global receiver', async () => {
  const original = globalThis.fetch;
  globalThis.fetch = function () { assert.equal(this, globalThis); return Promise.resolve(Response.json({ items: [] })); };
  try { await new NativeWeChatClient(options).accounts(); } finally { globalThis.fetch = original; }
});
test('SSE activity starts on actual frames, including keepalive comments', async () => {
  const encoder = new TextEncoder(); let activity = 0;
  const client = new NativeWeChatClient({ ...options, fetch: async () => new Response(new ReadableStream({ start(controller) {
    controller.enqueue(encoder.encode(': keepalive\n\n'));
    controller.enqueue(encoder.encode('data: {"cursor":"1","kind":"message.committed","data":{"messageId":"m"},"createdAt":"1"}\n\n')); controller.close();
  } }), { headers: { 'Content-Type': 'text/event-stream' } }) });
  assert.equal(activity,0); const events=[];
  for await (const event of client.watch('a','0',undefined,{ onActivity: () => activity++ })) events.push(event);
  assert.equal(activity,2); assert.equal(events.length,1);
});


test('profile removal sends a confirmed DELETE and refuses a mismatched identity before transport', async () => {
  const requests = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    requests.push({ url: String(url), init });
    return Response.json({ accountId: 'profile-1', removed: true, filesRetained: true });
  }});
  assert.throws(() => client.removeAccount('profile-1', 'profile-2'), /confirmation/);
  assert.equal(requests.length, 0);
  assert.equal((await client.removeAccount('profile-1', 'profile-1')).filesRetained, true);
  assert.match(requests[0].url, /\/v1\/accounts\/profile-1$/);
  assert.equal(requests[0].init.method, 'DELETE');
  assert.deepEqual(JSON.parse(requests[0].init.body), { confirmAccountId: 'profile-1' });
});

test('account settings read and update the account-scoped API with byte limits', async () => {
  const calls = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => {
    calls.push([new URL(url).pathname, init.method ?? 'GET', init.body ? JSON.parse(init.body) : null]);
    return Response.json({ autoDownloadMaxBytes: init.body ? JSON.parse(init.body).autoDownloadMaxBytes : 52428800 });
  }});
  assert.equal((await client.accountSettings('personal')).autoDownloadMaxBytes, 52428800);
  assert.equal((await client.updateAccountSettings('personal', { autoDownloadMaxBytes: 0 })).autoDownloadMaxBytes, 0);
  assert.deepEqual(calls, [ ['/v1/accounts/personal/settings', 'GET', null], ['/v1/accounts/personal/settings', 'PATCH', { autoDownloadMaxBytes: 0 }] ]);
  for (const autoDownloadMaxBytes of [-1, 1.5, NaN, 536870913]) assert.throws(() => client.updateAccountSettings('personal', { autoDownloadMaxBytes }), /limit/);
  assert.equal(calls.length, 2);
});
