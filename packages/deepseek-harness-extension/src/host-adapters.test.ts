/**
 * Tests for the host adapters. Every dependency is a plain fake — no host
 * SDK or native daemon is loaded. Run with:
 *   node --test --experimental-strip-types src/host-adapters.test.ts
 */
import test from 'node:test';
import assert from 'node:assert/strict';

import type { FollowupMessage } from './contracts.ts';
import { asAccountId, asChatJid } from './contracts.ts';
import type { SharedNativeClientLike } from './host-adapters.ts';
import {
  NATIVE_TOKEN_MAX_CHARS,
  NATIVE_TOKEN_MIN_CHARS,
  createAgentFactoryAdapter,
  createNativeClientAdapter,
  createOutboxTransport,
  createSecretResolverAdapter,
  toDshUserMessage,
  validateNativeToken,
} from './host-adapters.ts';

const makeToken = (length: number, char = 'a'): string => char.repeat(length);
const VALID_TOKEN = makeToken(32, 'x');

// ---------------------------------------------------------------------------
// validateNativeToken
// ---------------------------------------------------------------------------

test('validateNativeToken: valid 32-char ASCII passes unchanged', () => {
  assert.equal(validateNativeToken(VALID_TOKEN), VALID_TOKEN);
  assert.equal(NATIVE_TOKEN_MIN_CHARS, 32);
  assert.equal(NATIVE_TOKEN_MAX_CHARS, 1024);
});

test('validateNativeToken: too short and too long reject', () => {
  assert.throws(() => validateNativeToken(makeToken(31)), Error);
  assert.throws(() => validateNativeToken(makeToken(1025)), Error);
  assert.equal(validateNativeToken(makeToken(1024)).length, 1024);
});

test('validateNativeToken: newline and CR reject', () => {
  assert.throws(() => validateNativeToken('a'.repeat(31) + '\n'), Error);
  assert.throws(() => validateNativeToken('a'.repeat(31) + '\r'), Error);
});

test('validateNativeToken: non-ASCII rejects', () => {
  assert.throws(() => validateNativeToken('a'.repeat(31) + '你'), Error);
  assert.throws(() => validateNativeToken('a'.repeat(31) + '\u0000'), Error);
});

test('validateNativeToken: error message never leaks the token', () => {
  const secret = 'SUPERSECRETVALUE12345678901234567890';
  try {
    validateNativeToken(secret + '\n');
    assert.fail('should have thrown');
  } catch (error) {
    const message = (error as Error).message;
    assert.ok(!message.includes(secret));
    assert.ok(!message.includes('SUPERSECRET'));
  }
});

// ---------------------------------------------------------------------------
// createSecretResolverAdapter
// ---------------------------------------------------------------------------

test('createSecretResolverAdapter: resolve passes through value/source', async () => {
  const adapter = createSecretResolverAdapter({
    resolve: async () => ({ value: 'v', source: 'env' }),
  });
  assert.deepEqual(await adapter.resolve('ref'), { value: 'v', source: 'env' });
});

test('createSecretResolverAdapter: resolve returns undefined when missing', async () => {
  const adapter = createSecretResolverAdapter({ resolve: async () => undefined });
  assert.equal(await adapter.resolve('ref'), undefined);
});

test('createSecretResolverAdapter: resolveNativeToken resolves and validates', async () => {
  const adapter = createSecretResolverAdapter({
    resolve: async () => ({ value: VALID_TOKEN, source: 'env' }),
  });
  assert.equal(await adapter.resolveNativeToken('token.ref'), VALID_TOKEN);
});

test('createSecretResolverAdapter: no cache — two calls resolve twice', async () => {
  let calls = 0;
  const adapter = createSecretResolverAdapter({
    resolve: async () => {
      calls++;
      return { value: VALID_TOKEN, source: 'env' };
    },
  });
  await adapter.resolveNativeToken('token.ref');
  await adapter.resolveNativeToken('token.ref');
  assert.equal(calls, 2);
});

test('createSecretResolverAdapter: missing credential throws without leaking value', async () => {
  const adapter = createSecretResolverAdapter({ resolve: async () => undefined });
  await assert.rejects(
    () => adapter.resolveNativeToken('wechat.daemonToken'),
    (error: Error) => {
      assert.ok(error.message.includes('wechat.daemonToken'));
      assert.ok(!error.message.includes(VALID_TOKEN));
      return true;
    },
  );
});

test('createSecretResolverAdapter: invalid token rejects', async () => {
  const adapter = createSecretResolverAdapter({
    resolve: async () => ({ value: 'short', source: 'env' }),
  });
  await assert.rejects(() => adapter.resolveNativeToken('token.ref'), Error);
});

// ---------------------------------------------------------------------------
// createNativeClientAdapter
// ---------------------------------------------------------------------------

function fakeClient(overrides: Partial<SharedNativeClientLike> = {}): SharedNativeClientLike {
  return {
    status: async () => ({}),
    accounts: async () => ({ items: [] }),
    events: async () => ({ items: [], nextCursor: '' }),
    watch: async function* () {},
    ack: async () => {},
    message: async () => ({}),
    messages: async () => ({ items: [], nextCursor: '' }),
    sendText: async () => ({}),
    sendByKey: async () => undefined,
    ...overrides,
  };
}

test('createNativeClientAdapter: status maps and coerces fields', async () => {
  const adapter = createNativeClientAdapter(
    fakeClient({
      status: async () => ({
        serverId: 42,
        pid: '777',
        ownershipGeneration: 9,
        version: {
          product: 1,
          backend: 2,
          version: 3,
          apiVersion: '4',
          capabilities: ['text-send', 5],
        },
      }),
    }),
  );
  const status = await adapter.status();
  assert.equal(status.serverId, '42');
  assert.equal(status.pid, 777);
  assert.equal(status.ownershipGeneration, '9');
  assert.deepEqual(status.version, {
    product: '1',
    backend: '2',
    version: '3',
    apiVersion: 4,
    capabilities: ['text-send', '5'],
  });
});

test('createNativeClientAdapter: status defaults when version missing', async () => {
  const adapter = createNativeClientAdapter(
    fakeClient({ status: async () => ({}) }),
  );
  const status = await adapter.status();
  assert.deepEqual(status.version.capabilities, []);
  assert.equal(status.version.apiVersion, 0);
  assert.equal(status.serverId, '');
});

test('createNativeClientAdapter: accounts brands raw ids', async () => {
  const adapter = createNativeClientAdapter(
    fakeClient({ accounts: async () => ({ items: ['acct-a', 'acct-b'] }) }),
  );
  assert.deepEqual(await adapter.accounts(), ['acct-a', 'acct-b']);
});

test('createNativeClientAdapter: events maps view fields', async () => {
  const adapter = createNativeClientAdapter(
    fakeClient({
      events: async () => ({
        items: [
          { cursor: 7, kind: 'message.committed', data: { messageId: 'm1' }, createdAt: 123 },
        ],
        nextCursor: 8,
      }),
    }),
  );
  const page = await adapter.events(asAccountId('acct'), '0', 10);
  assert.deepEqual(page.items, [
    {
      cursor: '7',
      kind: 'message.committed',
      data: { messageId: 'm1' },
      createdAt: '123',
    },
  ]);
  assert.equal(page.nextCursor, '8');
});

test('createNativeClientAdapter: watch maps events and forwards signal + onActivity', async () => {
  let seenArgs: unknown[] = [];
  const adapter = createNativeClientAdapter(
    fakeClient({
      watch: async function* (accountId, after, signal, opts) {
        seenArgs = [accountId, after, signal, opts];
        opts?.onActivity?.();
        yield { cursor: '1', kind: 'message.committed', data: { x: 1 }, createdAt: 't' };
        yield { cursor: '2', kind: 'thread.updated', data: {}, createdAt: 't2' };
      },
    }),
  );
  const controller = new AbortController();
  let activities = 0;
  const events = [];
  for await (const event of adapter.watch(
    asAccountId('acct'),
    '0',
    controller.signal,
    () => { activities++; },
  )) {
    events.push(event);
  }
  assert.equal(seenArgs[0], 'acct');
  assert.equal(seenArgs[1], '0');
  assert.equal(seenArgs[2], controller.signal);
  assert.equal((seenArgs[3] as { onActivity: () => void }).onActivity instanceof Function, true);
  assert.equal(activities, 1);
  assert.equal(events.length, 2);
  assert.deepEqual(events[0]?.data, { x: 1 });
  assert.equal(events[1]?.kind, 'thread.updated');
});

test('createNativeClientAdapter: watch forwards undefined onActivity as absent opts', async () => {
  let optsSeen: unknown = 'unset';
  const adapter = createNativeClientAdapter(
    fakeClient({
      watch: async function* (_a, _b, _c, opts) {
        optsSeen = opts;
      },
    }),
  );
  for await (const _ of adapter.watch(asAccountId('acct'), '0', new AbortController().signal)) {
    // drain
  }
  assert.equal(optsSeen, undefined);
});

test('createNativeClientAdapter: ack / message / messages delegate', async () => {
  const calls: unknown[] = [];
  const adapter = createNativeClientAdapter(
    fakeClient({
      ack: async (...args) => { calls.push(['ack', ...args]); },
      message: async (...args) => {
        calls.push(['message', ...args]);
        return { id: 'm1', body: 'hi' };
      },
      messages: async (...args) => {
        calls.push(['messages', ...args]);
        return { items: [{ id: 'm1' }], nextCursor: '5' };
      },
    }),
  );
  await adapter.ack(asAccountId('acct'), 'consumer', '3');
  assert.deepEqual(await adapter.message(asAccountId('acct'), 'm1'), { id: 'm1', body: 'hi' });
  const signal = new AbortController().signal;
  const page = await adapter.messages(
    asAccountId('acct'),
    asChatJid('chat@jid'),
    { limit: 5 },
    signal,
  );
  assert.deepEqual(page.items, [{ id: 'm1' }]);
  assert.equal(page.nextCursor, '5');
  assert.deepEqual(calls[0], ['ack', 'acct', 'consumer', '3']);
  assert.deepEqual(calls[1], ['message', 'acct', 'm1']);
  assert.deepEqual(calls[2], ['messages', 'acct', 'chat@jid', { limit: 5 }, signal]);
});

// ---------------------------------------------------------------------------
// createAgentFactoryAdapter
// ---------------------------------------------------------------------------

test('createAgentFactoryAdapter: create wraps handle.agent.id, followup and dispose delegate', async () => {  const calls: unknown[] = [];
  const adapter = createAgentFactoryAdapter({
    create: async (opts) => {
      calls.push(['create', opts]);
      return {
        id: 'outer',
        agent: {
          id: 'agent-1',
          followup: (m: unknown) => { calls.push(['followup', m]); },
        },
        dispose: async () => { calls.push(['dispose']); },
      };
    },
    resume: async () => { throw new Error('unexpected'); },
  });
  const opts = { sessionId: 's1' };
  const agent = await adapter.create(opts);
  assert.equal(agent.id, 'agent-1');
  agent.followup({ id: 'u1' });
  await agent.dispose();
  assert.deepEqual(calls[0], ['create', opts]);
  assert.deepEqual(calls[1], ['followup', { id: 'u1' }]);
  assert.deepEqual(calls[2], ['dispose']);
});

test('createAgentFactoryAdapter: resume falls back to handle when .agent missing', async () => {
  let passed: unknown;
  const adapter = createAgentFactoryAdapter({
    create: async () => { throw new Error('unexpected'); },
    resume: async (opts) => {
      passed = opts;
      return {
        id: 'handle-2',
        followup: () => {},
        dispose: async () => {},
      };
    },
  });
  const agent = await adapter.resume({ resumeSessionId: 's2' });
  assert.equal(agent.id, 'handle-2');
  assert.deepEqual(passed, { resumeSessionId: 's2' });
});

test('createAgentFactoryAdapter: default model rides along as live agentOptions', async () => {
  const calls: Array<['create' | 'resume', { agentOptions?: Record<string, unknown> }]> = [];
  const makeHandle = (id: string) => ({
    id,
    followup: () => {},
    dispose: async () => {},
  });
  const adapter = createAgentFactoryAdapter(
    {
      create: async (opts: { agentOptions?: Record<string, unknown> }) => {
        calls.push(['create', opts]);
        return makeHandle('a1');
      },
      resume: async (opts: { agentOptions?: Record<string, unknown> }) => {
        calls.push(['resume', opts]);
        return makeHandle('a2');
      },
    } as never,
    {
      currentSelection: () => ({
        provider: current.provider,
        model: current.model,
        reasoningEffort: current.reasoningEffort,
      }),
    },
  );
  const current = {
    provider: 'p1',
    model: 'm1',
    reasoningEffort: 'high' as string | undefined,
  };

  await adapter.create({ sessionId: 's1' });
  assert.equal(calls[0]?.[1].agentOptions?.provider, 'p1');
  assert.equal(calls[0]?.[1].agentOptions?.model, 'm1');
  assert.equal(calls[0]?.[1].agentOptions?.reasoningEffort, 'high');

  // The selection is read live per call, not captured at adapter build.
  current.provider = 'p2';
  current.model = 'm2';
  current.reasoningEffort = undefined;
  await adapter.resume({ resumeSessionId: 's2' });
  assert.equal(calls[1]?.[1].agentOptions?.provider, 'p2');
  assert.equal(calls[1]?.[1].agentOptions?.reasoningEffort, undefined);
});

test('createAgentFactoryAdapter: without a default model options pass through untouched', async () => {
  let createOptions: unknown;
  const adapter = createAgentFactoryAdapter({
    create: async (opts: { sessionId: string; agentOptions?: Record<string, unknown> }) => {
      createOptions = opts;
      return { id: 'a', followup: () => {}, dispose: async () => {} };
    },
    resume: async () => {
      throw new Error('unexpected');
    },
  } as never);
  await adapter.create({ sessionId: 's1', agentOptions: { temperature: 1 } });
  assert.deepEqual(createOptions, { sessionId: 's1', agentOptions: { temperature: 1 } });
});

// ---------------------------------------------------------------------------
// toDshUserMessage
// ---------------------------------------------------------------------------

test('toDshUserMessage: frozen shape, stable id, text content, kind user', () => {
  const message: FollowupMessage = { id: 'wechat-native:acct:s1:abc', text: 'hello', source: 'wechat' };
  const userMessage = toDshUserMessage(message);
  assert.equal(Object.isFrozen(userMessage), true);
  assert.equal(userMessage.id, 'wechat-native:acct:s1:abc');
  assert.equal(userMessage.role, 'user');
  assert.equal(Object.isFrozen(userMessage.content), true);
  assert.deepEqual(userMessage.content, [{ type: 'text', text: 'hello' }]);
  assert.equal(userMessage.source.kind, 'user');
  assert.equal(Object.isFrozen(userMessage.source), true);
  assert.equal((userMessage.source as { source?: string }).source, 'wechat');
});

test('toDshUserMessage: source omitted when message.source absent', () => {
  const userMessage = toDshUserMessage({ id: 'u1', text: 'x' });
  assert.equal((userMessage.source as { source?: string }).source, undefined);
});

// ---------------------------------------------------------------------------
// createOutboxTransport
// ---------------------------------------------------------------------------

test('createOutboxTransport: sendText maps intent ids', async () => {
  let passed: unknown[] = [];
  const transport = createOutboxTransport(
    fakeClient({
      sendText: async (...args) => {
        passed = args;
        return {
          serverId: 'srv-1',
          localMessageId: 'local-1',
          clientId: 'cli-1',
          state: 'accepted',
          errorCode: null,
        };
      },
    }),
  );
  const result = await transport.sendText(asAccountId('acct'), asChatJid('chat@jid'), 'hi', 'key1');
  assert.deepEqual(result, { serverId: 'srv-1', serverMessageId: 'local-1' });
  assert.deepEqual(passed, ['acct', 'chat@jid', 'hi', 'key1']);
});

test('createOutboxTransport: sendText falls back to clientId then serverId', async () => {
  const t1 = createOutboxTransport(
    fakeClient({ sendText: async () => ({ serverId: 's', clientId: 'c' }) }),
  );
  assert.deepEqual(await t1.sendText(asAccountId('a'), asChatJid('c'), 't', 'k'), {
    serverId: 's',
    serverMessageId: 'c',
  });
  const t2 = createOutboxTransport(
    fakeClient({ sendText: async () => ({ serverId: 's' }) }),
  );
  assert.deepEqual(await t2.sendText(asAccountId('a'), asChatJid('c'), 't', 'k'), {
    serverId: 's',
    serverMessageId: 's',
  });
});

test('createOutboxTransport: sendText throws when serverId is null or empty', async () => {
  for (const serverId of [null, '', undefined]) {
    const transport = createOutboxTransport(
      fakeClient({ sendText: async () => ({ serverId, clientId: 'cli' }) }),
    );
    await assert.rejects(
      () => transport.sendText(asAccountId('a'), asChatJid('c'), 'secret-text', 'key'),
      (error: Error) => {
        assert.ok(!error.message.includes('secret-text'));
        return true;
      },
    );
  }
});

test('createOutboxTransport: sendByKey returns undefined on missing', async () => {
  const transport = createOutboxTransport(
    fakeClient({ sendByKey: async () => undefined }),
  );
  assert.equal(await transport.sendByKey(asAccountId('acct'), 'key'), undefined);
  const transportNull = createOutboxTransport(
    fakeClient({ sendByKey: async () => null }),
  );
  assert.equal(await transportNull.sendByKey(asAccountId('acct'), 'key'), undefined);
});

test('createOutboxTransport: sendByKey maps valid intent with text', async () => {
  const transport = createOutboxTransport(
    fakeClient({
      sendByKey: async () => ({
        serverId: 'srv-2',
        localMessageId: 'local-2',
        text: 'original text',
        state: 'accepted',
        errorCode: null,
      }),
    }),
  );
  assert.deepEqual(await transport.sendByKey(asAccountId('acct'), 'key'), {
    serverId: 'srv-2',
    serverMessageId: 'local-2',
    text: 'original text',
  });
});

test('createOutboxTransport: sendByKey omits text when not confidently available', async () => {
  const transport = createOutboxTransport(
    fakeClient({
      sendByKey: async () => ({ serverId: 'srv-3', clientId: 'cli-3' }),
    }),
  );
  assert.equal(await transport.sendByKey(asAccountId('acct'), 'key'), undefined);
});
