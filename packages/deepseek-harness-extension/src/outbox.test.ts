import test from 'node:test';
import assert from 'node:assert/strict';
import {
  asAccountId,
  asChatJid,
  asServerId,
  type OutboundRecord,
  type ServerId,
} from './contracts.ts';
import {
  createOutbox,
  DEFAULT_READBACK_TIMEOUT_MS,
  REASON_READBACK_MISMATCH,
  REASON_SEND_UNCONFIRMED,
  type OutboxDeps,
  type ReadbackResult,
  type SendResult,
} from './outbox.ts';
import type { OutboxRecord, TableView } from './storage.ts';

const ACCOUNT = asAccountId('account-1');
const CHAT = asChatJid('room@chatroom');
const KEY = 'idem-key-1';
const PAYLOAD = 'hello from outbox';
const SERVER_ID = asServerId('server-1');
const SERVER_MESSAGE_ID = 'msg-42';

/** Hand-written in-memory TableView fake — mirrors the host KvTable semantics. */
class MemoryTable<V> implements TableView<string, V> {
  readonly rows = new Map<string, V>();

  get(key: string): V | undefined {
    return this.rows.get(key);
  }

  entries(): IterableIterator<[string, V]> {
    return this.rows.entries();
  }

  keys(): IterableIterator<string> {
    return this.rows.keys();
  }

  get size(): number {
    return this.rows.size;
  }

  async put(key: string, value: V): Promise<void> {
    this.rows.set(key, value);
  }

  async delete(key: string): Promise<boolean> {
    return this.rows.delete(key);
  }

  async deleteMany(keys: readonly string[]): Promise<number> {
    let n = 0;
    for (const k of keys) if (this.rows.delete(k)) n++;
    return n;
  }

  async update(key: string, fn: (current: V) => V): Promise<V> {
    const current = this.rows.get(key);
    if (current === undefined) throw new Error(`no row for ${key}`);
    const next = fn(current);
    this.rows.set(key, next);
    return next;
  }
}

interface Harness {
  deps: OutboxDeps;
  table: MemoryTable<OutboxRecord>;
  outbox: ReturnType<typeof createOutbox>;
  sent: Array<{ accountId: string; chatJid: string; text: string; key: string }>;
  sendInvocations: number;
  lookups: string[];
  sleeps: number[];
  setSend: (fn: OutboxDeps['sendText']) => void;
  setLookup: (fn: NonNullable<OutboxDeps['sendByKey']>) => void;
  setClock: (fn: () => number) => void;
}

function harness(overrides: Partial<OutboxDeps> = {}): Harness {
  const table = new MemoryTable<OutboxRecord>();
  const sent: Harness['sent'] = [];
  let sendInvocations = 0;
  const lookups: string[] = [];
  const sleeps: number[] = [];
  let sendFn: OutboxDeps['sendText'] = async (accountId, chatJid, text, key) => {
    sent.push({ accountId, chatJid, text, key });
    return { serverId: SERVER_ID, serverMessageId: SERVER_MESSAGE_ID };
  };
  let lookupFn: NonNullable<OutboxDeps['sendByKey']> = async () => undefined;
  let nowMs = 1_700_000_000_000;
  let clock = () => nowMs;

  const deps: OutboxDeps = {
    tables: { outbox: table },
    sendText: (a, c, t, k) => {
      sendInvocations++;
      return sendFn(a, c, t, k);
    },
    sendByKey: (a, k) => {
      lookups.push(k);
      return lookupFn(a, k);
    },
    now: () => clock(),
    readbackTimeoutMs: 1_000,
    sleep: async (ms) => {
      sleeps.push(ms);
      // Advance the fake clock so the polling deadline is reachable, exactly
      // as it is with a real timer.
      nowMs += ms;
    },
    ...overrides,
  };
  return {
    deps,
    table,
    sent,
    get sendInvocations() {
      return sendInvocations;
    },
    lookups,
    sleeps,
    outbox: createOutbox(deps),
    setSend: (fn) => {
      sendFn = fn;
    },
    setLookup: (fn) => {
      lookupFn = fn;
    },
    setClock: (fn) => {
      clock = fn;
    },
  };
}

const enqueueDefault = (h: Harness): Promise<OutboundRecord> =>
  h.outbox.enqueue({
    key: KEY,
    accountId: ACCOUNT,
    chatJid: CHAT,
    kind: 'text',
    payload: PAYLOAD,
  });

const goodReadback = (): ReadbackResult => ({
  serverId: SERVER_ID,
  serverMessageId: SERVER_MESSAGE_ID,
  text: PAYLOAD,
});

/** Port `verify` takes a branded ServerId; helper for direct-verify tests. */
const brandedReadback = (text: string = PAYLOAD): {
  serverId: ServerId;
  serverMessageId: string;
  text: string;
} => ({ serverId: SERVER_ID, serverMessageId: SERVER_MESSAGE_ID, text });

// ---------------------------------------------------------------------------

test('enqueue creates a pending record with timestamps', async () => {
  const h = harness();
  const rec = await enqueueDefault(h);
  assert.equal(rec.key, KEY);
  assert.equal(rec.status, 'pending');
  assert.equal(rec.payload, PAYLOAD);
  assert.equal(rec.createdAt, rec.updatedAt);
  assert.equal(Number.isNaN(Date.parse(rec.createdAt)), false);
  assert.equal(h.table.size, 1);
});

test('replaying the same key returns the existing record without a second row', async () => {
  const h = harness();
  const first = await enqueueDefault(h);
  const replay = await h.outbox.enqueue({
    key: KEY,
    accountId: asAccountId('different-account'),
    chatJid: asChatJid('different@chatroom'),
    kind: 'text',
    payload: 'different payload',
  });
  assert.deepEqual(replay, first);
  assert.equal(h.table.size, 1);
  assert.equal(h.table.get(KEY)?.payload, PAYLOAD);
  assert.equal(h.table.get(KEY)?.accountId, ACCOUNT);
});

test('successful send becomes sent after canonical readback match', async () => {
  const h = harness();
  await enqueueDefault(h);
  const rec = await h.outbox.deliver(KEY);
  assert.equal(rec?.status, 'sent');
  assert.equal(rec?.serverId, SERVER_ID);
  assert.equal(rec?.serverMessageId, SERVER_MESSAGE_ID);
  assert.equal(rec?.reason, undefined);
  assert.equal(h.sent.length, 1);
  assert.deepEqual(h.sent[0], {
    accountId: ACCOUNT,
    chatJid: CHAT,
    text: PAYLOAD,
    key: KEY,
  });
  assert.equal(h.lookups.length, 0);
});

test('readback mismatch marks needs_review with canonical-readback-mismatch and no retry', async () => {
  const h = harness();
  await enqueueDefault(h);
  // The POST returns ids, but a direct verify with mismatching text/ids must
  // be held for review.
  const review = await h.outbox.verify(KEY, brandedReadback('not what we queued'));
  assert.equal(review.status, 'needs_review');
  assert.equal(review.reason, REASON_READBACK_MISMATCH);

  // A later canonical readback must not resurrect a terminal review record.
  const late = await h.outbox.verify(KEY, brandedReadback());
  assert.equal(late.status, 'needs_review');
  const delivered = await h.outbox.deliver(KEY);
  assert.equal(delivered?.status, 'needs_review');
  assert.equal(h.sent.length, 0);
  assert.equal(h.lookups.length, 0);
});

test('send throws, sendByKey finds matching message, deliver becomes sent without resending', async () => {
  const h = harness();
  await enqueueDefault(h);
  h.setSend(async () => {
    throw new Error('network lost after POST');
  });
  h.setLookup(async () => goodReadback());

  const rec = await h.outbox.deliver(KEY);
  assert.equal(rec?.status, 'sent');
  assert.equal(rec?.serverId, SERVER_ID);
  assert.equal(rec?.serverMessageId, SERVER_MESSAGE_ID);
  assert.equal(h.sendInvocations, 1);
  assert.deepEqual(h.lookups, [KEY]);
});

test('send throws and sendByKey times out: needs_review send-unconfirmed, still one send', async () => {
  const h = harness();
  await enqueueDefault(h);
  h.setSend(async () => {
    throw new Error('timeout');
  });
  h.setLookup(async () => undefined); // never finds the message

  const rec = await h.outbox.deliver(KEY);
  assert.equal(rec?.status, 'needs_review');
  assert.equal(rec?.reason, REASON_SEND_UNCONFIRMED);
  assert.equal(h.sendInvocations, 1);
  assert.ok(h.lookups.length >= 1);
  // Polls are bounded by the configured 1s deadline, not infinite.
  assert.ok(h.sleeps.reduce((sum, ms) => sum + ms, 0) <= 1_000);
  assert.equal(DEFAULT_READBACK_TIMEOUT_MS, 65_000);
});

test('deliver on an already-sent record makes no send call', async () => {
  const h = harness();
  await enqueueDefault(h);
  const first = await h.outbox.deliver(KEY);
  assert.equal(first?.status, 'sent');
  const second = await h.outbox.deliver(KEY);
  assert.deepEqual(second, first);
  assert.equal(h.sent.length, 1);
  assert.equal(h.lookups.length, 0);
});

test('recover of a pending record uses sendByKey only, never sendText', async () => {
  const h = harness();
  await enqueueDefault(h);
  h.setLookup(async () => goodReadback());

  const recovered = await h.outbox.recover();
  assert.equal(recovered.length, 1);
  assert.equal(recovered[0]?.status, 'sent');
  assert.equal(recovered[0]?.serverId, SERVER_ID);
  assert.equal(h.sent.length, 0);
  assert.deepEqual(h.lookups, [KEY]);

  // A second sweep now skips the row (no longer pending).
  assert.deepEqual(await h.outbox.recover(), []);
  assert.equal(h.lookups.length, 1);
});

test('recover with no matching message holds send-unconfirmed without sending', async () => {
  const h = harness();
  await enqueueDefault(h);
  const recovered = await h.outbox.recover();
  assert.equal(recovered[0]?.status, 'needs_review');
  assert.equal(recovered[0]?.reason, REASON_SEND_UNCONFIRMED);
  assert.equal(h.sent.length, 0);
});

test('markNeedsReview persists the reason and pending() excludes non-pending rows', async () => {
  const h = harness();
  await enqueueDefault(h);
  await h.outbox.enqueue({
    key: 'other-key',
    accountId: ACCOUNT,
    chatJid: CHAT,
    kind: 'text',
    payload: 'still pending',
  });
  const rec = await h.outbox.markNeedsReview(KEY, 'manual-override');
  assert.equal(rec.status, 'needs_review');
  assert.equal(rec.reason, 'manual-override');
  assert.equal(h.table.get(KEY)?.reason, 'manual-override');

  const pending = await h.outbox.pending();
  assert.equal(pending.length, 1);
  assert.equal(pending[0]?.key, 'other-key');
  assert.equal((await h.outbox.get(KEY))?.status, 'needs_review');
});
