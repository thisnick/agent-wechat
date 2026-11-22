import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  acceptEvent,
  Backoff,
  CATCH_UP_WINDOW_MS,
  isGroupChat,
  NativeConsumer,
  toInboundMessage,
  type BridgeDeps,
  type CursorStore,
} from './native-bridge.ts';
import { NativeApiError } from '@agent-wechat/shared';
import { asAccountId, asChatJid, asServerId } from './contracts.ts';
import type { NativeClientPort, NativeEventView } from './ports.ts';

const ACCOUNT = asAccountId('acct-1');
const SERVER = 'srv-123';
const NOW = Date.parse('2026-11-23T02:00:00Z');

const event = (cursor: string, kind: string, data: Record<string, unknown>, createdAt = '2026-11-23T01:59:00Z'): NativeEventView => ({
  cursor, kind, data, createdAt,
});

const storedMessage = (overrides: Record<string, unknown> = {}): Record<string, unknown> => ({
  id: 'msg-1',
  accountId: ACCOUNT,
  threadId: 'user-a',
  serverId: '5001',
  senderId: 'user-a',
  direction: 'incoming',
  type: 1,
  body: 'hello',
  createdAt: '2026-11-23T01:59:00Z',
  mentions: { state: 'known', self: false, all: false, userIds: [] },
  ...overrides,
});

function makeDeps(stored: Record<string, unknown>, answerable = true): BridgeDeps {
  const client = {
    message: async () => stored,
  } as unknown as NativeClientPort;
  const cursors: CursorStore = {
    get: async () => '0',
    set: async () => {},
  };
  return {
    client,
    cursors,
    serverId: SERVER,
    consumerId: 'dsh',
    answerable: () => answerable,
    now: () => NOW,
  };
}

test('toInboundMessage produces a stable dispatch id', () => {
  const a = toInboundMessage(SERVER, storedMessage());
  const b = toInboundMessage(SERVER, storedMessage());
  assert.equal(a.id, b.id);
  assert.match(a.id, /^wechat-native:acct-1:srv-123:/);
  assert.equal(a.kind, 'text');
  assert.equal(a.text, 'hello');
});

test('dispatch ids differ per message within one chat (inbox rejects duplicate pending ids)', () => {
  const a = toInboundMessage(SERVER, storedMessage());
  const b = toInboundMessage(SERVER, storedMessage({ serverId: '5002', id: 'msg-2', body: 'again' }));
  assert.notEqual(a.id, b.id);
  assert.match(b.id, /^wechat-native:acct-1:srv-123:[0-9a-f]+:[0-9a-f]+$/);
  // The same stored message keeps its id across catch-up re-reads.
  assert.equal(a.id, toInboundMessage(SERVER, storedMessage()).id);
});

test('acceptEvent ignores events at or below the cursor', async () => {
  const deps = makeDeps(storedMessage());
  const res = await acceptEvent(deps, ACCOUNT, '10', event('10', 'message.committed', { messageId: 'msg-1', origin: 'live', direction: 'incoming' }));
  assert.equal(res.message, undefined);
  assert.equal(res.advancedCursor, false);
});

test('acceptEvent advances non-committed events without dispatch', async () => {
  const deps = makeDeps(storedMessage());
  const res = await acceptEvent(deps, ACCOUNT, '0', event('1', 'thread.updated', {}));
  assert.equal(res.message, undefined);
  assert.equal(res.advancedCursor, true);
});

test('acceptEvent skips non-live / outgoing origins', async () => {
  const deps = makeDeps(storedMessage());
  for (const data of [
    { messageId: 'msg-1', origin: 'catchup', direction: 'incoming' },
    { messageId: 'msg-1', origin: 'live', direction: 'outgoing' },
  ]) {
    const res = await acceptEvent(deps, ACCOUNT, '0', event('1', 'message.committed', data));
    assert.equal(res.message, undefined, JSON.stringify(data));
    assert.equal(res.advancedCursor, true);
  }
});

test('acceptEvent skips messages older than 24h', async () => {
  const old = new Date(NOW - CATCH_UP_WINDOW_MS - 1000).toISOString();
  const deps = makeDeps(storedMessage({ createdAt: old }));
  const res = await acceptEvent(deps, ACCOUNT, '0', event('1', 'message.committed', { messageId: 'msg-1', origin: 'live', direction: 'incoming' }, old));
  assert.equal(res.message, undefined);
  assert.equal(res.advancedCursor, true);
});

test('acceptEvent dispatches an answerable live incoming committed message', async () => {
  const deps = makeDeps(storedMessage());
  let review: string | undefined;
  const withReview: BridgeDeps = { ...deps, onNeedsReview: (r) => { review = r; } };
  const res = await acceptEvent(withReview, ACCOUNT, '0', event('1', 'message.committed', {
    messageId: 'msg-1', threadId: 'user-a', origin: 'live', direction: 'incoming',
  }));
  assert.ok(res.message);
  assert.equal(res.advancedCursor, true);
  assert.equal(res.message.kind, 'text');
  assert.equal(review, undefined);
});

test('acceptEvent holds identity mismatch for review without advancing', async () => {
  const deps = makeDeps(storedMessage({ accountId: 'other-acct' }));
  let review: string | undefined;
  const withReview: BridgeDeps = { ...deps, onNeedsReview: (r) => { review = r; } };
  const res = await acceptEvent(withReview, ACCOUNT, '0', event('1', 'message.committed', {
    messageId: 'msg-1', threadId: 'user-a', origin: 'live', direction: 'incoming',
  }));
  assert.equal(res.message, undefined);
  assert.equal(res.advancedCursor, false);
  assert.equal(review, 'committed-identity-mismatch');
});

test('acceptEvent holds missing message id for review', async () => {
  const deps = makeDeps(storedMessage());
  let review: string | undefined;
  const withReview: BridgeDeps = { ...deps, onNeedsReview: (r) => { review = r; } };
  const res = await acceptEvent(withReview, ACCOUNT, '0', event('1', 'message.committed', { origin: 'live' }));
  assert.equal(res.advancedCursor, false);
  assert.equal(review, 'committed-message-id-missing');
});

test('acceptEvent honours the answerable gate (admission before dispatch)', async () => {
  const deps = makeDeps(storedMessage(), false);
  const res = await acceptEvent(deps, ACCOUNT, '0', event('1', 'message.committed', {
    messageId: 'msg-1', origin: 'live', direction: 'incoming',
  }));
  assert.equal(res.message, undefined);
  assert.equal(res.advancedCursor, true);
});

test('isGroupChat detects @chatroom suffix', () => {
  assert.equal(isGroupChat(asChatJid('room@chatroom')), true);
  assert.equal(isGroupChat(asChatJid('user-a')), false);
});

test('Backoff doubles 250ms→30s and resets', () => {
  const b = new Backoff();
  assert.equal(b.next(), 250);
  assert.equal(b.next(), 500);
  assert.equal(b.next(), 1000);
  let last = 0;
  for (let i = 0; i < 20; i++) last = b.next();
  assert.equal(last, 30000);
  b.reset();
  assert.equal(b.next(), 250);
});

test('toInboundMessage maps mention flags from native message', () => {
  const mentioned = toInboundMessage(SERVER, storedMessage({ mentions: { state: 'known', self: true, all: false, userIds: [] } }));
  assert.equal(mentioned.mentioned, true);
  const all = toInboundMessage(SERVER, storedMessage({ mentions: { state: 'known', self: false, all: true, userIds: [] } }));
  assert.equal(all.mentioned, true);
});

test('catchUp rewinds a stored cursor that sits ahead of the journal tail', async () => {
  // The daemon can rebuild its journal with lower sequence numbers; a cursor
  // past the tail answers empty pages forever instead of 410.
  const journal = new Map<string, Record<string, unknown>>();
  for (let c = 5; c <= 10; c++) {
    journal.set(String(c), { cursor: String(c), kind: 'thread.updated', data: {}, createdAt: '2026-11-23T01:00:00Z' });
  }
  const events = async (_a: unknown, after: string, _limit: number) => {
    if (BigInt(after) < 5n) throw new NativeApiError(410, 'event_cursor_expired');
    const items = [...journal.values()].filter((e) => BigInt((e as { cursor: string }).cursor) > BigInt(after));
    const last = items[items.length - 1];
    return { items, ...(last !== undefined ? { nextCursor: last.cursor } : {}) };
  };
  const stored: string[] = [];
  const acked: string[] = [];
  const cursors: CursorStore = {
    get: async () => '65535',
    set: async (_a, cursor) => { stored.push(cursor); },
  };
  const deps = {
    ...makeDeps(storedMessage()),
    client: { events, ack: async (_a: unknown, _c: unknown, cursor: string) => { acked.push(cursor); } } as unknown as NativeClientPort,
    cursors,
  };
  await new NativeConsumer(deps).catchUp(ACCOUNT, { onMessage: async () => {} });
  assert.ok(stored.includes('10'), `cursor should rewind to tail 10, set=${JSON.stringify(stored)}`);
  assert.deepEqual(acked, [], 'rewind past the tail replays nothing, so no ack during catch-up');
});

test('catchUp keeps a stored cursor that matches the journal tail', async () => {
  const journal = new Map<string, Record<string, unknown>>();
  for (let c = 5; c <= 10; c++) {
    journal.set(String(c), { cursor: String(c), kind: 'thread.updated', data: {}, createdAt: '2026-11-23T01:00:00Z' });
  }
  const events = async (_a: unknown, after: string, _limit: number) => {
    if (BigInt(after) < 5n) throw new NativeApiError(410, 'event_cursor_expired');
    const items = [...journal.values()].filter((e) => BigInt((e as { cursor: string }).cursor) > BigInt(after));
    const last = items[items.length - 1];
    return { items, ...(last !== undefined ? { nextCursor: last.cursor } : {}) };
  };
  const acked: string[] = [];
  const deps = {
    ...makeDeps(storedMessage()),
    client: { events, ack: async (_a: unknown, _c: unknown, cursor: string) => { acked.push(cursor); } } as unknown as NativeClientPort,
    cursors: { get: async () => '10', set: async () => {} },
  };
  await new NativeConsumer(deps).catchUp(ACCOUNT, { onMessage: async () => {} });
  assert.deepEqual(acked, [], 'a caught-up cursor must not re-ack');
});

test('journalTail rewind survives a retention floor inside the search range', async () => {
  // Floor at 40000: below it probes 410; events 40001..50005; empty above 50005.
  const journal = new Map<string, Record<string, unknown>>();
  for (let c = 40001; c <= 50005; c++) {
    journal.set(String(c), { cursor: String(c), kind: 'thread.updated', data: {}, createdAt: '2026-11-23T01:00:00Z' });
  }
  const events = async (_a: unknown, after: string, _limit: number) => {
    if (BigInt(after) < 40000n) throw new NativeApiError(410, 'event_cursor_expired');
    const items = [...journal.values()].filter((e) => BigInt((e as { cursor: string }).cursor) > BigInt(after));
    const last = items[items.length - 1];
    return { items, ...(last !== undefined ? { nextCursor: last.cursor } : {}) };
  };
  const client = { events, ack: async () => {} } as unknown as NativeClientPort;
  const consumer = new NativeConsumer({ ...makeDeps(storedMessage()), client, cursors: { get: async () => '65535', set: async () => {} } });
  // @ts-expect-error private method access for the regression test
  const tail = await consumer.journalTail(ACCOUNT, '65535');
  assert.equal(tail, '50005');
});
