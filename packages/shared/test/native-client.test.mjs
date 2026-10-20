import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeApiError, NativeWeChatClient, validateNativeBaseUrl } from '../src/native-client.ts';
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
