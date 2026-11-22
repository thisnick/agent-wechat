/**
 * Pure-part tests for the per-thread SessionRouter (DESIGN.md §3.2).
 *
 * Everything is faked in-memory: a hand-written TableView, a recording Agent
 * factory, and an injected clock/message mapper. No live bot, no host import
 * at runtime, deterministic timestamps.
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  asAccountId,
  asChatJid,
  makeRouteKey,
  stableHash32,
  type FollowupMessage,
  type RouteKey,
  type SessionId,
} from './contracts.ts';
import type { TableView } from './storage.ts';
import type { ThreadRecord } from './storage.ts';
import {
  createSessionRouter,
  type AgentCreateOptions,
  type AgentResumeOptions,
  type LiveAgent,
  type SessionRouter,
  type SetupFn,
} from './session-router.ts';

const T0 = 1_800_000_000_000; // 2027-01-15T08:00:00.000Z
const mkTime = (start: number) => {
  let t = start;
  return () => t;
};

class MemoryTable implements TableView<string, ThreadRecord> {
  readonly map = new Map<string, ThreadRecord>();
  get(key: string): ThreadRecord | undefined {
    return this.map.get(key);
  }
  entries(): IterableIterator<[string, ThreadRecord]> {
    return this.map.entries();
  }
  keys(): IterableIterator<string> {
    return this.map.keys();
  }
  get size(): number {
    return this.map.size;
  }
  async put(key: string, value: ThreadRecord): Promise<void> {
    this.map.set(key, value);
  }
  async delete(key: string): Promise<boolean> {
    return this.map.delete(key);
  }
  async deleteMany(keys: readonly string[]): Promise<number> {
    let n = 0;
    for (const k of keys) if (this.map.delete(k)) n++;
    return n;
  }
  async update(key: string, fn: (current: ThreadRecord) => ThreadRecord): Promise<ThreadRecord> {
    const current = this.map.get(key);
    if (current === undefined) throw new Error(`No record for ${key}`);
    const next = fn(current);
    this.map.set(key, next);
    return next;
  }
}

class FakeAgent implements LiveAgent {
  readonly id: string;
  readonly followups: unknown[] = [];
  disposed = false;
  constructor(id: string) {
    this.id = id;
  }
  followup(message: unknown): void {
    this.followups.push(message);
  }
  async dispose(): Promise<void> {
    this.disposed = true;
  }
}

interface CreateCall extends AgentCreateOptions {}
interface ResumeCall extends AgentResumeOptions {}

function makeHarness(opts: {
  now?: () => number;
  failCreate?: boolean;
  failResume?: boolean;
  sessionIdFor?: (routeKey: RouteKey) => SessionId;
} = {}) {
  const threads = new MemoryTable();
  const created: CreateCall[] = [];
  const resumed: ResumeCall[] = [];
  const agents: FakeAgent[] = [];
  let nextId = 1;
  const factory = {
    async create(options: AgentCreateOptions): Promise<LiveAgent> {
      created.push(options);
      if (opts.failCreate) throw new Error('create exploded');
      const agent = new FakeAgent(`agent-${nextId++}`);
      agents.push(agent);
      return agent;
    },
    async resume(options: AgentResumeOptions): Promise<LiveAgent> {
      resumed.push(options);
      if (opts.failResume) throw new Error('stored session is gone');
      const agent = new FakeAgent(`agent-${nextId++}`);
      agents.push(agent);
      return agent;
    },
  };
  const router = createSessionRouter({
    tables: { threads },
    agents: factory,
    sessionIdFor: opts.sessionIdFor ?? sessionIdOf,
    toUserMessage: (message: FollowupMessage, binding) => ({
      role: 'user',
      text: `mapped:${message.text}`,
      messageId: message.id,
      routeKey: binding.routeKey,
    }),
    ...(opts.now === undefined ? {} : { now: opts.now }),
  });
  return { threads, created, resumed, agents, router, factory };
}

function sessionIdOf(routeKey: RouteKey): SessionId {
  return `wht-${stableHash32(routeKey)}` as SessionId;
}

const ROUTE = makeRouteKey(asAccountId('acct'), asChatJid('chat@jid'));
const MSG: FollowupMessage = { id: 'm-1', text: 'hello' };

// ---------------------------------------------------------------------------
// 1. New route creates + persists a binding
// ---------------------------------------------------------------------------

test('new route creates a durable binding with revision 1 and derived sessionId', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const binding = await h.router.ensure(ROUTE, { policyFingerprint: 'fp-7' });

  assert.equal(h.created.length, 1);
  assert.equal(h.created[0]?.sessionId, sessionIdOf(ROUTE));
  assert.equal(h.resumed.length, 0);
  assert.deepEqual(binding, {
    routeKey: ROUTE,
    sessionId: sessionIdOf(ROUTE),
    accountId: asAccountId('acct'),
    chatJid: asChatJid('chat@jid'),
    policyFingerprint: 'fp-7',
    revision: 1,
    lastActivityAt: new Date(T0).toISOString(),
  });
  const stored = h.threads.get(ROUTE);
  assert.ok(stored);
  assert.equal(stored.sessionId, sessionIdOf(ROUTE));
  assert.equal(stored.revision, 1);
  assert.equal(stored.policyFingerprint, 'fp-7');
});

test('policyFingerprint defaults to empty string when not supplied', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const binding = await h.router.ensure(ROUTE, {});
  assert.equal(binding.policyFingerprint, '');
});

// ---------------------------------------------------------------------------
// 2. Second ensure reuses the live agent
// ---------------------------------------------------------------------------

test('second ensure reuses the live agent without another create/resume', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const first = await h.router.ensure(ROUTE, { policyFingerprint: 'fp' });
  const second = await h.router.ensure(ROUTE, { policyFingerprint: 'different' });

  assert.equal(h.created.length, 1);
  assert.equal(h.resumed.length, 0);
  assert.deepEqual(second, first);
  assert.equal(h.agents.length, 1);
});

// ---------------------------------------------------------------------------
// 3. Resume after evict uses durable sessionId and bumps revision
// ---------------------------------------------------------------------------

test('resume after evict reuses durable sessionId and bumps revision', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const first = await h.router.ensure(ROUTE, { policyFingerprint: 'fp' });
  await h.router.evict(ROUTE);
  const resumed = await h.router.ensure(ROUTE, { policyFingerprint: 'ignored-on-resume' });

  assert.equal(h.created.length, 1);
  assert.equal(h.resumed.length, 1);
  assert.equal(h.resumed[0]?.resumeSessionId, first.sessionId);
  assert.equal(resumed.sessionId, first.sessionId);
  assert.equal(resumed.revision, 2);
  assert.equal(resumed.policyFingerprint, 'fp');
  assert.equal(h.threads.get(ROUTE)?.revision, 2);
});

test('resume failure drops the stale binding and recreates a fresh session', async () => {
  const h = makeHarness({ now: mkTime(T0), failResume: true });
  const first = await h.router.ensure(ROUTE, { policyFingerprint: 'fp' });
  await h.router.evict(ROUTE);
  const second = await h.router.ensure(ROUTE, { policyFingerprint: 'fp' });

  assert.equal(h.resumed.length, 1);
  assert.equal(h.created.length, 2);
  // The stale record was deleted and replaced by a fresh revision-1 binding.
  assert.equal(h.threads.get(ROUTE)?.sessionId, second.sessionId);
  assert.equal(second.revision, 1);
  assert.equal(second.sessionId, first.sessionId);
  // And the route still delivers followups through the new Agent.
  await h.router.followup(ROUTE, MSG);
  assert.equal(h.agents[1]?.followups.length, 1);
});

// ---------------------------------------------------------------------------
// 4. Followup maps through the injected toUserMessage
// ---------------------------------------------------------------------------

test('followup ensures the route and injects the mapped message', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  await h.router.followup(ROUTE, MSG);

  assert.equal(h.created.length, 1);
  assert.equal(h.agents[0]?.followups.length, 1);
  assert.deepEqual(h.agents[0]?.followups[0], {
    role: 'user',
    text: 'mapped:hello',
    messageId: 'm-1',
    routeKey: ROUTE,
  });
});

// ---------------------------------------------------------------------------
// 5. bindingForAgent reverse map
// ---------------------------------------------------------------------------

test('bindingForAgent resolves the live agent back to its binding', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const binding = await h.router.ensure(ROUTE, {});
  const agent = h.agents[0];
  assert.ok(agent);
  assert.deepEqual(await h.router.bindingForAgent(agent.id), binding);
  assert.equal(await h.router.bindingForAgent('unknown-agent'), undefined);
});

// ---------------------------------------------------------------------------
// 6. evict keeps the durable record and clears the reverse map
// ---------------------------------------------------------------------------

test('evict disposes the agent, keeps durable record, clears reverse map', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const binding = await h.router.ensure(ROUTE, { policyFingerprint: 'fp' });
  const agent = h.agents[0];
  assert.ok(agent);

  await h.router.evict(ROUTE);

  assert.equal(agent.disposed, true);
  assert.equal(await h.router.bindingForAgent(agent.id), undefined);
  assert.deepEqual(await h.router.binding(ROUTE), binding);
  assert.ok(h.threads.get(ROUTE));
  // Evicting twice is harmless.
  await h.router.evict(ROUTE);
});

// ---------------------------------------------------------------------------
// 7. refresh bumps revision and lastActivityAt
// ---------------------------------------------------------------------------

test('refresh bumps revision and lastActivityAt on both record and live binding', async () => {
  let t = T0;
  const h = makeHarness({ now: () => t });
  const first = await h.router.ensure(ROUTE, {});
  t = T0 + 60_000;
  const refreshed = await h.router.refresh(ROUTE);

  assert.equal(refreshed.revision, first.revision + 1);
  assert.equal(refreshed.lastActivityAt, new Date(T0 + 60_000).toISOString());
  assert.equal(h.threads.get(ROUTE)?.revision, 2);

  const agent = h.agents[0];
  assert.ok(agent);
  assert.deepEqual(await h.router.bindingForAgent(agent.id), refreshed);
});

// ---------------------------------------------------------------------------
// 8. Concurrent ensure coalesces to a single create
// ---------------------------------------------------------------------------

test('concurrent ensure calls coalesce into one create', async () => {
  const h = makeHarness({ now: mkTime(T0) });
  const [a, b, c] = await Promise.all([
    h.router.ensure(ROUTE, {}),
    h.router.ensure(ROUTE, {}),
    h.router.ensure(ROUTE, {}),
  ]);

  assert.equal(h.created.length, 1);
  assert.deepEqual(a, b);
  assert.deepEqual(b, c);
  assert.equal(h.agents.length, 1);
});

// ---------------------------------------------------------------------------
// 9. Failed create leaves no durable binding and can be retried
// ---------------------------------------------------------------------------

test('failed create leaves no durable binding and a retry succeeds', async () => {
  const threads = new MemoryTable();
  const created: AgentCreateOptions[] = [];
  let fail = true;
  const router = createSessionRouter({
    tables: { threads },
    agents: {
      async create(options: AgentCreateOptions): Promise<LiveAgent> {
        created.push(options);
        if (fail) throw new Error('boom');
        return new FakeAgent('agent-retry');
      },
      async resume(): Promise<LiveAgent> {
        return new FakeAgent('agent-resume-unexpected');
      },
    },
    sessionIdFor: sessionIdOf,
    toUserMessage: () => ({}),
    now: mkTime(T0),
  });

  await assert.rejects(() => router.ensure(ROUTE, {}), /boom/);
  assert.equal(threads.size, 0);
  assert.equal(await router.binding(ROUTE), undefined);

  fail = false;
  const binding = await router.ensure(ROUTE, {});
  assert.equal(binding.revision, 1);
  assert.equal(threads.size, 1);
  assert.equal(created.length, 2);
});

// ---------------------------------------------------------------------------
// 10. sessionId derivation hook is used
// ---------------------------------------------------------------------------

test('injected sessionIdFor hook drives the derived session id', async () => {
  const seen: string[] = [];
  const custom = (key: RouteKey): SessionId => {
    seen.push(key);
    return `custom-${stableHash32(key)}` as SessionId;
  };
  const h = makeHarness({ now: mkTime(T0), sessionIdFor: custom });

  const binding = await h.router.ensure(ROUTE, {});
  assert.deepEqual(seen, [ROUTE]);
  assert.equal(binding.sessionId, `custom-${stableHash32(ROUTE)}`);
  assert.equal(h.created[0]?.sessionId, binding.sessionId);
  assert.equal(h.threads.get(ROUTE)?.sessionId, binding.sessionId);
});

// ---------------------------------------------------------------------------
// 11. evictIdle disposes only routes idle beyond the threshold
// ---------------------------------------------------------------------------

test('evictIdle disposes aged live routes and keeps their durable records', async () => {
  let t = T0;
  const h = makeHarness({ now: () => t });
  const routeA = ROUTE;
  const routeB = makeRouteKey(asAccountId('acct'), asChatJid('other@jid'));
  await h.router.ensure(routeA, {});
  t = T0 + 10_000;
  await h.router.ensure(routeB, {});
  t = T0 + 20_000;

  const evicted = await h.router.evictIdle(15_000);

  assert.deepEqual(evicted.sort(), [routeA].sort());
  assert.equal(h.agents[0]?.disposed, true); // route A
  assert.equal(h.agents[1]?.disposed, false); // route B, still young
  assert.ok(h.threads.get(routeA));
  assert.ok(h.threads.get(routeB));
  assert.equal(await h.router.bindingForAgent(h.agents[0]!.id), undefined);
});

// ---------------------------------------------------------------------------
// 12. The factory result implements the full SessionRouterPort surface
// ---------------------------------------------------------------------------

test('router exposes every SessionRouterPort method plus evictIdle', () => {
  const h = makeHarness();
  const r: SessionRouter = h.router;
  for (const method of [
    'binding',
    'ensure',
    'followup',
    'bindingForAgent',
    'evict',
    'refresh',
    'evictIdle',
  ] as const) {
    assert.equal(typeof r[method], 'function', `missing ${method}`);
  }
});
