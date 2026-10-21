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
test('send uses caller idempotency key and sparse read updates never widen a boundary', async () => {
  const requests = [];
  const client = new NativeWeChatClient({ ...options, fetch: async (url, init) => { requests.push({ url: String(url), init }); return Response.json({ state: 'accepted' }); } });
  await client.sendText('a', 't', 'fixture', 'send-key');
  await client.markRead('a', 't', ['m1', 'm3', 'm1'], '7');
  assert.equal(requests[0].init.headers.get('Idempotency-Key'), 'send-key');
  assert.deepEqual(JSON.parse(requests[0].init.body), { threadId: 't', text: 'fixture' });
  assert.deepEqual(JSON.parse(requests[1].init.body), { messageIds: ['m1', 'm3'], expectedVersion: '7' });
  assert.throws(() => client.markRead('a', 't', [], '7'));
});
test('structured failures expose safe error codes but never raw server messages', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({ error: { code: 'read_state_conflict', message: 'private source content', retryable: false } }, { status: 409 }) });
  await assert.rejects(client.markRead('a', 't', ['m'], '7'), error => error instanceof NativeApiError && error.status === 409 && error.code === 'read_state_conflict' && !error.message.includes('private'));
});
test('read projections and event pages enforce the server maximum of 200', async () => {
  const client = new NativeWeChatClient({ ...options, fetch: async () => Response.json({}) });
  await client.markRead('a', 't', Array.from({ length: 200 }, (_, index) => `m${index}`), '7');
  await client.events('a', '0', 200);
  assert.throws(() => client.markRead('a', 't', Array.from({ length: 201 }, (_, index) => `m${index}`), '7'));
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
test('explicit human read-through stays distinct from exact model input IDs', async () => {
 const client=new NativeWeChatClient({...options,fetch:async(_url,init)=>{assert.deepEqual(JSON.parse(init.body),{throughMessageId:'m_boundary',expectedVersion:'7'});return Response.json({version:'8'});}});await client.markReadThrough('a','t','m_boundary','7');
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
