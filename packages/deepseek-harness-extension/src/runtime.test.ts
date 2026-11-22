import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  asAccountId,
  asChatJid,
  asRouteKey,
  asSenderId,
  asServerId,
  asSessionId,
  type FollowupMessage,
  type InboundMessage,
  type OutboundRecord,
  type RouteBinding,
  type RouteKey,
} from './contracts.ts';
import { createWechatRuntime, createJournalCursorStore, type RuntimeDeps } from './runtime.ts';
import type { ReplyState } from './reply-collector.ts';
import type { AdmissionConfig } from './admission.ts';
import { JournalRecordSchema, type JournalRecord, type TableView } from './storage.ts';

// ---------------------------------------------------------------------------
// In-memory table fake
// ---------------------------------------------------------------------------

function fakeTable<V>(): TableView<string, V> {
  const map = new Map<string, V>();
  return {
    get: (k) => map.get(k),
    entries: () => map.entries(),
    keys: () => map.keys(),
    get size() {
      return map.size;
    },
    async put(k, v) {
      map.set(k, v);
    },
    async delete(k) {
      return map.delete(k);
    },
    async deleteMany(keys) {
      let n = 0;
      for (const k of keys) if (map.delete(k)) n++;
      return n;
    },
    async update(k, fn) {
      const next = fn(map.get(k) as V);
      map.set(k, next);
      return next;
    },
  };
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const OPEN: AdmissionConfig = {
  dmPolicy: 'open',
  groupPolicy: 'disabled',
  dmAllowFrom: [],
  groupAllowFrom: [],
  groupSenderAllowFrom: [],
  requireMentionInGroups: true,
};

const DISABLED: AdmissionConfig = {
  dmPolicy: 'disabled',
  groupPolicy: 'disabled',
  dmAllowFrom: [],
  groupAllowFrom: [],
  groupSenderAllowFrom: [],
  requireMentionInGroups: true,
};

function dmMessage(id = 'm1'): InboundMessage {
  return {
    id,
    ref: {
      accountId: asAccountId('acct'),
      chatJid: asChatJid('wxid_peer'),
      serverId: asServerId('srv'),
      ...(true ? { senderId: asSenderId('wxid_peer') } : {}),
    },
    serverMessageId: `sm-${id}`,
    kind: 'text',
    text: 'hello',
    media: [],
    mentioned: false,
    createdAt: new Date().toISOString(),
  };
}

function binding(overrides: Partial<RouteBinding> = {}): RouteBinding {
  return {
    routeKey: asRouteKey('acct:wxid_peer'),
    sessionId: asSessionId('sess'),
    accountId: asAccountId('acct'),
    chatJid: asChatJid('wxid_peer'),
    policyFingerprint: 'fp',
    revision: 1,
    lastActivityAt: new Date().toISOString(),
    ...overrides,
  };
}

interface Harness {
  deps: RuntimeDeps;
  ensured: RouteKey[];
  followed: FollowupMessage[];
  enqueued: Array<Omit<OutboundRecord, 'status' | 'createdAt' | 'updatedAt'>>;
  delivered: string[];
  logs: string[];
}

function makeHarness(opts: {
  admission?: AdmissionConfig;
  replyState?: ReplyState | undefined;
  binding?: RouteBinding;
  deliveredStatus?: OutboundRecord['status'];
} = {}): Harness {
  const routeBinding = opts.binding ?? binding();
  const ensured: RouteKey[] = [];
  const followed: FollowupMessage[] = [];
  const enqueued: Harness['enqueued'] = [];
  const delivered: string[] = [];
  const logs: string[] = [];
  const outboxTable = fakeTable<unknown>();

  const router = {
    async binding() {
      return routeBinding;
    },
    async ensure(routeKey: RouteKey) {
      ensured.push(routeKey);
      return routeBinding;
    },
    async followup(_routeKey: RouteKey, message: FollowupMessage) {
      followed.push(message);
    },
    async bindingForAgent() {
      return routeBinding;
    },
    async evict() {},
    async refresh() {
      return routeBinding;
    },
    async evictIdle() {
      return [];
    },
  };

  const outbox = {
    async enqueue(record: Omit<OutboundRecord, 'status' | 'createdAt' | 'updatedAt'>) {
      enqueued.push(record);
      const at = new Date().toISOString();
      const row: OutboundRecord = { ...record, status: 'pending', createdAt: at, updatedAt: at };
      await outboxTable.put(record.key, row);
      return row;
    },
    async get() {
      return undefined;
    },
    async verify() {
      throw new Error('not used');
    },
    async markNeedsReview() {
      throw new Error('not used');
    },
    async pending() {
      return [];
    },
    async deliver(key: string) {
      delivered.push(key);
      return {
        key,
        accountId: routeBinding.accountId,
        chatJid: routeBinding.chatJid,
        kind: 'text' as const,
        payload: 'reply',
        status: (opts.deliveredStatus ?? 'sent') as OutboundRecord['status'],
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
      };
    },
    async recover() {
      return [];
    },
  };

  const native = {
    async status() {
      return {} as never;
    },
    async accounts() {
      return [];
    },
    async events() {
      return { items: [], nextCursor: '0' };
    },
    async *watch() {},
    async ack() {},
    async message() {
      return {};
    },
    async messages() {
      return { items: [] };
    },
  };

  const deps: RuntimeDeps = {
    native,
    router,
    outbox,
    outboxTable,
    journal: fakeTable(),
    admission: opts.admission ?? OPEN,
    replySource: {
      stateOf: () => opts.replyState,
    },
    sessionRoutes: () => routeBinding,
    serverId: 'srv',
    consumerId: 'consumer',
    toFollowup: (message) => ({
      id: message.id,
      text: message.text,
      source: 'wechat',
    }),
    log: (_level, msg) => logs.push(msg),
  };

  return { deps, ensured, followed, enqueued, delivered, logs };
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test('createJournalCursorStore returns 0 for unknown account and persists cursor', async () => {
  const table = fakeTable<JournalRecord>();
  const store = createJournalCursorStore(table);
  const account = asAccountId('acct');
  assert.equal(await store.get(account), '0');
  await store.set(account, '42');
  assert.equal(await store.get(account), '42');
});

test('createJournalCursorStore writes a record that satisfies JournalRecordSchema', async () => {
  // Regression: the storage domain re-validates every stored row when the
  // domain is (re)opened; a partial cursor record once parked the channel
  // permanently ("stored record ... does not match its schema").
  const table = fakeTable<JournalRecord>();
  const store = createJournalCursorStore(table);
  await store.set(asAccountId('acct'), '65535');
  const stored = table.entries().next().value?.[1];
  assert.ok(stored !== undefined);
  JournalRecordSchema.parse(stored); // throws on shape drift
  assert.equal(stored.accountId, 'acct');
  assert.equal(stored.cursor, '65535');
  assert.equal(stored.eventKind, 'cursor');
});

test('handleInbound rejects a disabled DM before any routing', async () => {
  const h = makeHarness({ admission: DISABLED });
  const rt = createWechatRuntime(h.deps);
  const result = await rt.handleInbound(dmMessage());
  assert.equal(result.admitted, false);
  assert.equal(result.reason, 'disabled');
  assert.equal(h.ensured.length, 0);
  assert.equal(h.followed.length, 0);
});

test('handleInbound admits a DM, ensures a route, and injects a followup', async () => {
  const h = makeHarness();
  const rt = createWechatRuntime(h.deps);
  const result = await rt.handleInbound(dmMessage('m2'));
  assert.equal(result.admitted, true);
  assert.equal(result.routeKey, 'acct:wxid_peer');
  assert.equal(h.ensured.length, 1);
  assert.equal(h.followed.length, 1);
  assert.equal(h.followed[0]!.id, 'm2');
  assert.equal(h.followed[0]!.text, 'hello');
});

test('drainReplies returns empty when the projection has no state', async () => {
  const h = makeHarness({ replyState: undefined });
  const rt = createWechatRuntime(h.deps);
  assert.deepEqual(await rt.drainReplies('sess'), []);
  assert.equal(h.enqueued.length, 0);
});

test('drainReplies enqueues and delivers a completed reply once', async () => {
  const state: ReplyState = {
    turns: {},
    ready: [{ seq: 5, turn: 1, text: 'the reply' }],
  };
  const h = makeHarness({ replyState: state });
  const rt = createWechatRuntime(h.deps);
  const results = await rt.drainReplies('sess');
  assert.equal(results.length, 1);
  assert.equal(h.enqueued.length, 1);
  assert.equal(h.enqueued[0]!.key, 'reply:sess:5:1');
  assert.equal(h.enqueued[0]!.payload, 'the reply');
  assert.deepEqual(h.delivered, ['reply:sess:5:1']);
});

test('drainReplies skips replies whose keys are already in the outbox', async () => {
  const state: ReplyState = {
    turns: {},
    ready: [{ seq: 5, turn: 1, text: 'already sent' }],
  };
  const h = makeHarness({ replyState: state });
  // Pre-populate the outbox table with the canonical reply key.
  await h.deps.outboxTable.put('reply:sess:5:1', { status: 'sent' });
  const rt = createWechatRuntime(h.deps);
  const results = await rt.drainReplies('sess');
  assert.equal(results.length, 0);
  assert.equal(h.enqueued.length, 0);
});

test('drainReplies warns and skips when no binding can be resolved', async () => {
  const state: ReplyState = {
    turns: {},
    ready: [{ seq: 5, turn: 1, text: 'orphan' }],
  };
  const h = makeHarness({ replyState: state });
  h.deps = { ...h.deps, sessionRoutes: () => undefined, knownRoutes: [] };
  const rt = createWechatRuntime(h.deps);
  assert.deepEqual(await rt.drainReplies('sess'), []);
  assert.ok(h.logs.some((line) => line.includes('no route binding')));
});

test('recoverOutbox delegates to the outbox without resending', async () => {
  const h = makeHarness();
  const rt = createWechatRuntime(h.deps);
  assert.deepEqual(await rt.recoverOutbox(), []);
});

test('start and stop are idempotent and flip the running flag', async () => {
  const h = makeHarness();
  const rt = createWechatRuntime(h.deps);
  assert.equal(rt.running, false);
  await rt.start([]);
  assert.equal(rt.running, true);
  await rt.start([]);
  assert.equal(rt.running, true);
  await rt.stop();
  assert.equal(rt.running, false);
  await rt.stop();
  assert.equal(rt.running, false);
});
