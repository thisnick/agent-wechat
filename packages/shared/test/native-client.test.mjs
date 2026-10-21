import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeApiError, NativeWeChatClient, validateNativeBaseUrl, isNativeTombstone, NATIVE_MAX_SOURCE_BYTES } from '../src/native-client.ts';
const options = { baseUrl: 'http://127.0.0.1:6174', token: 'fixture-secret' };
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
test('structured failures expose safe error codes but never raw server messages', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ error: { code: 'read_state_conflict', message: 'private source content', retryable: false } }, { status: 409 }) });
  await assert.rejects(client.markRead('a', 't', '7'), error => error instanceof NativeApiError && error.status === 409 && error.code === 'read_state_conflict' && !error.message.includes('private'));
});
test('event pages enforce the server maximum of 200', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({}) });
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
test('media upload native limit is generated from the daemon contract and refuses before transport', async () => {
 let calls=0;const client=new NativeWeChatClient({...options,fetch:async()=>{calls++;throw Error('unexpected transport');}});assert.equal(NATIVE_MAX_SOURCE_BYTES,25*1024*1024-17);await assert.rejects(client.upload('a',new Uint8Array(NATIVE_MAX_SOURCE_BYTES+1),'fixture.bin'));assert.equal(calls,0);
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

test('quoted originals are parent-bound with embedded fallback and no caller-selected chat', async () => {
  const original={id:'original',accountId:'a',threadId:'t',serverId:'18446744073709551615',type:3};
  const response={accountId:'a',messageId:'parent',threadId:'t',state:'resolved',message:original};
  const requests=[];
  const client=new NativeWeChatClient({...options,fetch:async(url)=>{requests.push(String(url));return Response.json(response);}});
  assert.equal((await client.quotedMessage('a','parent')).message.serverId,'18446744073709551615');
  assert.equal(requests[0],'http://127.0.0.1:6174/v1/accounts/a/messages/parent/quote');
  for(const change of [{accountId:'other'},{threadId:'foreign'},{id:'parent'}]) {
    const foreign=new NativeWeChatClient({...options,fetch:async()=>Response.json({...response,message:{...original,...change}})});
    await assert.rejects(foreign.quotedMessage('a','parent'),/identity/);
  }
  const embedded=new NativeWeChatClient({...options,fetch:async()=>Response.json({...response,state:'embedded',message:null,embeddedText:'inside quote'})});
  assert.equal((await embedded.quotedMessage('a','parent')).embeddedText,'inside quote');
});
