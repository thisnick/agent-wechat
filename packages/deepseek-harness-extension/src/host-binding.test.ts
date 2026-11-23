/**
 * Host-binding tests — the assembly seam between pure modules and host
 * services. Every host facility is an in-memory fake with the same structural
 * surface as the real Cordis Context, and the native client is injected through
 * the `createNativeClient` test seam (no network, per AGENTS.md:24).
 *
 * Coverage:
 *  - happy-path assembly wires domain/projection/router/outbox/runtime
 *  - invalid tokenCredentialRef fails before any host branding
 *  - failure inside step 3 disposes the projection and closes the domain
 *  - `bindWechatHost` effect owns cleanup (stop runtime, dispose, close)
 *  - turn/end completed drains replies; other reasons/types never drain
 *  - native client is rebuilt when the resolved token rotates
 *
 * Every test that starts consumers disposes them, so the process exits.
 */

import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  assembleWechatBinding,
  bindWechatHost,
  bindSearchRoute,
  createDirectorySearch,
  type BindingNativeClient,
  type HostContextLike,
  type NativeClientFactory,
  type WechatConfigView,
} from './host-binding.ts';
import { type ReplyState } from './reply-collector.ts';
import { asAccountId, asChatJid } from './contracts.ts';

// ---------------------------------------------------------------------------
// Fake host context
// ---------------------------------------------------------------------------

const INITIAL_REPLY_STATE: ReplyState = { turns: {}, ready: [] };

interface FakeHost {
  readonly ctx: HostContextLike;
  readonly domain: { closed: boolean; tables: Map<string, Map<string, unknown>> };
  readonly registered: Array<Record<string, unknown>>;
  /** Resolves to the effect's disposer once its async body settles. */
  readonly effectDisposers: Array<Promise<() => Promise<unknown>>>;
  readonly agents: { created: string[]; resumed: string[]; followups: unknown[] };
  readonly listeners: Array<(session: unknown, event: unknown) => void>;
  readonly clientCreations: string[];
  readonly sends: Array<{ accountId: string; chatJid: string; text: string; key: string }>;
  /** Tool definitions registered on the fake ctx.tools registry. */
  readonly toolRegistrations: Array<Record<string, unknown>>;
  /** Captured `when` clauses keyed by tool name. */
  readonly registerWhens: Map<string, (context: unknown) => boolean | Promise<boolean>>;
  /** Live (not yet disposed) tool registrations. */
  readonly liveToolCount: () => number;
  /** Native read calls recorded by the fake client (method + first args). */
  readonly nativeCalls: Array<{ method: string; args: unknown[] }>;
  /** Local directory rows served by the fake client's contacts/threads. */
  readonly directoryContacts: Array<Record<string, unknown>>;
  readonly directoryThreads: Array<Record<string, unknown>>;
}

function makeFakeHost(): FakeHost {
  const tables = new Map<string, Map<string, unknown>>();
  for (const name of [
    'accounts',
    'threads',
    'journal',
    'custody',
    'outbox',
    'recalls',
    'config_generation',
  ]) {
    tables.set(name, new Map());
  }
  const domain = { closed: false, tables };

  const tableView = (store: Map<string, unknown>) => ({
    get: (k: string) => store.get(k),
    entries: () => store.entries(),
    keys: () => store.keys(),
    get size() {
      return store.size;
    },
    put: async (k: string, value: unknown) => {
      store.set(k, value);
    },
    delete: async (k: string) => store.delete(k),
    deleteMany: async (keys: readonly string[]) => {
      let removed = 0;
      for (const key of keys) if (store.delete(key)) removed += 1;
      return removed;
    },
    update: async (k: string, fn: (current: unknown) => unknown) => {
      const next = fn(store.get(k));
      store.set(k, next);
      return next;
    },
  });

  const registered: Array<Record<string, unknown>> = [];
  const agents = { created: [] as string[], resumed: [] as string[], followups: [] as unknown[] };
  const effectDisposers: Array<Promise<() => Promise<unknown>>> = [];
  const listeners: Array<(session: unknown, event: unknown) => void> = [];
  const clientCreations: string[] = [];
  const sends: FakeHost['sends'] = [];
  const toolRegistrations: Array<Record<string, unknown>> = [];
  const registerWhens = new Map<string, (context: unknown) => boolean | Promise<boolean>>();
  const nativeCalls: FakeHost['nativeCalls'] = [];
  const directoryContacts: Array<Record<string, unknown>> = [];
  const directoryThreads: Array<Record<string, unknown>> = [];
  let liveTools = 0;
  let imageSeq = 0;
  let fileSeq = 0;

  const ctx: HostContextLike = {
    storageDomain: {
      open: async () => ({
        table: (name: string) => tableView(tables.get(name) ?? new Map()) as never,
        global: {
          get: () => ({ generation: 0 }),
          set: async () => {},
        },
        close: async () => {
          domain.closed = true;
        },
      }),
    },
    sessionProjections: {
      register: async (definition: unknown) => {
        registered.push(definition as Record<string, unknown>);
        return {
          stateOf: () => INITIAL_REPLY_STATE,
          dispose: async () => {},
        };
      },
    },
    credentials: {
      resolve: async () => ({ value: 'a'.repeat(40), source: 'fake' }),
    },
    agents: {
      create: async (createOptions: { sessionId: string }) => {
        agents.created.push(createOptions.sessionId);
        return {
          agent: {
            id: `agent-${createOptions.sessionId}`,
            followup: (m: unknown) => agents.followups.push(m),
          },
          dispose: async () => {},
        };
      },
      resume: async (resumeOptions: { resumeSessionId: string }) => {
        agents.resumed.push(resumeOptions.resumeSessionId);
        return {
          agent: { id: `agent-${resumeOptions.resumeSessionId}`, followup: () => {} },
          dispose: async () => {},
        };
      },
    },
    sessions: {
      get: (id: string) => (id === 'wht-test' ? { id: 'wht-test' } : undefined),
      list: () => [],
    },
    tools: {
      register: (
        definition: unknown,
        options?: { readonly when?: (context: unknown) => boolean | Promise<boolean> },
      ) => {
        const def = definition as Record<string, unknown>;
        toolRegistrations.push(def);
        if (options !== undefined && typeof options.when === 'function') {
          registerWhens.set(String(def['name']), options.when);
        }
        liveTools += 1;
        return () => {
          liveTools -= 1;
        };
      },
    },
    attachments: {
      saveImage: async (input: { data: Uint8Array; mediaType: string; name?: string }) => ({
        attachmentId: `att-${(imageSeq += 1)}`,
        mediaType: input.mediaType,
        bytes: input.data.length,
        width: 1,
        height: 1,
        name: input.name,
      }),
      saveFile: async (input: { data: Uint8Array; name?: string }) => ({
        attachmentId: `file-${(fileSeq += 1)}`,
        name: input.name,
        bytes: input.data.length,
      }),
    },
    effect(execute: () => unknown) {
      // Mirrors Cordis: run the async effect and expose a promise for its disposer.
      effectDisposers.push(
        Promise.resolve(execute())
          .then((value) => (typeof value === 'function' ? (value as () => Promise<unknown>) : async () => {}))
          .catch(() => async () => {}),
      );
      return () => {};
    },
    on(name: string, listener: (...args: any[]) => void) {
      void name;
      listeners.push(listener as (session: unknown, event: unknown) => void);
      return () => {};
    },
  };

  return {
    ctx,
    domain,
    registered,
    effectDisposers,
    agents,
    listeners,
    clientCreations,
    sends,
    toolRegistrations,
    registerWhens,
    liveToolCount: () => liveTools,
    nativeCalls,
    directoryContacts,
    directoryThreads,
  };
}

const TOKEN = 'a'.repeat(40);

function fakeClientFactory(
  host: FakeHost,
  behavior: { failStatus?: boolean } = {},
): NativeClientFactory {
  return ({ token }: { baseUrl: string; token: string }): BindingNativeClient => {
    host.clientCreations.push(token);
    return {
      status: async () => {
        if (behavior.failStatus) throw new Error('native status unavailable');
        return {
          serverId: 'native-server-1',
          pid: 1,
          ownershipGeneration: 'gen',
          version: {
            product: 'agent-wechat',
            backend: 'native',
            version: '3.0.0',
            apiVersion: 1,
            capabilities: ['events'],
          },
        };
      },
      accounts: async () => ({ items: [] }),
      events: async () => ({ items: [], nextCursor: '0' }),
      watch: async function* () {},
      ack: async () => {},
      message: async () => ({}),
      messages: async (...args: unknown[]) => {
        host.nativeCalls.push({ method: 'messages', args });
        return { items: [] };
      },
      sendText: async (accountId: string, chatJid: string, text: string, key: string) => {
        host.sends.push({ accountId, chatJid, text, key });
        return { serverId: 'srv-msg-1', localMessageId: 'local-1' };
      },
      sendByKey: async () => undefined,
      contacts: async () => ({ items: host.directoryContacts, nextCursor: undefined }),
      threads: async () => ({ items: host.directoryThreads, nextCursor: undefined }),
    };
  };
}

const CONFIG: WechatConfigView = {
  native: {
    accountId: 'default',
    endpoint: 'http://127.0.0.1:6790',
    tokenCredentialRef: 'WECHAT_DAEMON_TOKEN',
  },
  dmPolicy: 'open',
  groupPolicy: 'disabled',
  dmAllowFrom: [],
  groupAllowFrom: [],
  groupSenderAllowFrom: [],
  requireMentionInGroups: true,
};

/** Dispose whatever the host binding registered (keeps the test process alive otherwise). */
async function disposeAll(host: FakeHost): Promise<void> {
  const pending = host.effectDisposers.splice(0);
  for (const resolve of pending) {
    const dispose = await resolve;
    await dispose();
  }
}

// ---------------------------------------------------------------------------
// assembleWechatBinding
// ---------------------------------------------------------------------------

test('assembly wires domain, projection, runtime and native identity', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });

  assert.equal(host.registered.length, 1, 'one projection registered');
  assert.equal((host.registered[0] as { key?: string }).key, 'agent_wechat_replies');
  assert.equal(host.domain.closed, false, 'domain stays open for the owner effect');
  assert.equal(binding.serverId, 'native-server-1');
  assert.equal(binding.consumerId, 'agent-wechat-host');
  assert.equal(binding.runtime.running, false, 'not started until the owner starts it');
  assert.equal(host.clientCreations.length, 1, 'one read client created');

  await binding.runtime.stop();
  await binding.replyHandle.dispose();
  await binding.domain.close();
  assert.equal(host.domain.closed, true);
});

test('invalid tokenCredentialRef fails before branding and closes the domain', async () => {
  const host = makeFakeHost();
  const bad: WechatConfigView = {
    ...CONFIG,
    native: { ...CONFIG.native, tokenCredentialRef: 'wechat.daemonToken' },
  };
  await assert.rejects(
    assembleWechatBinding({
      ctx: host.ctx,
      config: bad,
      log: () => {},
      createNativeClient: fakeClientFactory(host),
    }),
    /not a valid credential ref/,
  );
  assert.equal(host.domain.closed, true, 'domain closed on failure');
  assert.equal(host.clientCreations.length, 0, 'no client built with an invalid ref');
});

test('failure after registration disposes projection and closes domain', async () => {
  const host = makeFakeHost();
  await assert.rejects(
    assembleWechatBinding({
      ctx: host.ctx,
      config: CONFIG,
      log: () => {},
      createNativeClient: fakeClientFactory(host, { failStatus: true }),
    }),
    /native status unavailable/,
  );
  assert.equal(host.domain.closed, true);
});

test('missing credential fails closed without leaking a value', async () => {
  const host = makeFakeHost();
  (host.ctx.credentials as { resolve: () => Promise<undefined> }).resolve = async () => undefined;
  await assert.rejects(
    assembleWechatBinding({
      ctx: host.ctx,
      config: CONFIG,
      log: () => {},
      createNativeClient: fakeClientFactory(host),
    }),
    /credential not found/,
  );
  assert.equal(host.domain.closed, true);
});

// ---------------------------------------------------------------------------
// bindWechatHost lifecycle + session listener
// ---------------------------------------------------------------------------

test('bindWechatHost starts consumers and cleanup stops them', async () => {
  const host = makeFakeHost();
  try {
    const result = bindWechatHost(host.ctx, CONFIG, {
      createNativeClient: fakeClientFactory(host),
    });
    await result.ready;

    assert.equal(result.getState().running, true);
    assert.equal(result.getState().serverId, 'native-server-1');
    assert.equal(host.listeners.length, 1, 'session/event listener installed');

    await disposeAll(host);
    assert.equal(result.getState().running, false);
    assert.equal(host.domain.closed, true);
  } finally {
    await disposeAll(host);
  }
});

test('unreachable daemon parks the channel without rejecting ready (host stays up)', async () => {
  const host = makeFakeHost();
  const result = bindWechatHost(host.ctx, CONFIG, {
    createNativeClient: fakeClientFactory(host, { failStatus: true }),
    retryInitialMs: 5,
  });
  // The P0 contract: ready resolves, never rejects, even with no daemon.
  await result.ready;
  const state = result.getState();
  assert.equal(state.running, false);
  assert.match(String(state.error), /native status unavailable/);
  assert.ok(state.attempts >= 1, 'activation was attempted');
  // Parked channel retries in the background; disposal stops the loop.
  await disposeAll(host);
});

test('apply() never throws when the binding fails synchronously', async () => {
  const { apply, getServiceState } = await import('../index.ts');
  const throwingCtx = {
    get storageDomain(): never {
      throw new Error('host facility unavailable');
    },
  };
  // Must not throw.
  apply(throwingCtx as never, {});
  assert.equal(getServiceState(), undefined);
});

test('completed turn/end drains replies; other reasons and types do not', async () => {
  const host = makeFakeHost();
  try {
    const result = bindWechatHost(host.ctx, CONFIG, {
      createNativeClient: fakeClientFactory(host),
    });
    await result.ready;

    const listener = host.listeners[0];
    assert.ok(listener);
    // Non-completed turn reasons are never delivered.
    listener(
      { id: 'wht-test' },
      { type: 'turn/end', data: { turn: 1, reason: { kind: 'interrupted' } } },
    );
    listener({ id: 'wht-test' }, { type: 'turn/end', data: { turn: 1, reason: { kind: 'aborted' } } });
    // Non turn/end events are ignored.
    listener({ id: 'wht-test' }, { type: 'assistant/message', data: { turn: 1, step: 1 } });
    // Unknown session ids are ignored without throwing.
    listener(
      { id: 'unknown-session' },
      { type: 'turn/end', data: { turn: 2, reason: { kind: 'completed' } } },
    );
    // A completed turn for a known session reaches the drain path.
    listener(
      { id: 'wht-test' },
      { type: 'turn/end', data: { turn: 3, reason: { kind: 'completed' } } },
    );
    assert.equal(host.sends.length, 0, 'no reply state means nothing to send');
  } finally {
    await disposeAll(host);
  }
});

test('native client is rebuilt when the resolved token rotates', async () => {
  const host = makeFakeHost();
  let resolveCount = 0;
  (host.ctx.credentials as { resolve: () => Promise<{ value: string; source: string }> }).resolve =
    async () => {
      resolveCount += 1;
      // Rotate the token after the initial read client was built.
      const value = resolveCount === 1 ? TOKEN : 'b'.repeat(40);
      return { value, source: 'fake' };
    };

  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    assert.equal(host.clientCreations.length, 1);
    assert.equal(host.clientCreations[0], TOKEN);

    // A send re-resolves the credential and must build a new client for the new token.
    const queued = await binding.outbound.enqueue({
      key: 'reply:test:1:1',
      accountId: asAccountId('default'),
      chatJid: asChatJid('user-a'),
      kind: 'text',
      payload: 'hello',
    });
    await binding.outbound.deliver(queued.key);

    assert.equal(host.clientCreations.length, 2, 'rotated token rebuilds the client');
    assert.equal(host.clientCreations[1], 'b'.repeat(40));
    assert.equal(host.sends.length, 1, 'send went through the rotated client');
    assert.equal(host.sends[0]?.text, 'hello');
  } finally {
    await binding.runtime.stop();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

// ---------------------------------------------------------------------------
// Tool wiring (M3): 16 tools registered behind the trusted context
// ---------------------------------------------------------------------------

const EXPECTED_TOOL_NAMES = [
  // current-chat tools
  'wechat_history',
  'wechat_search',
  'wechat_message',
  'wechat_media',
  'wechat_recall',
  'wechat_send_share',
  // account tools (owner)
  'wechat_stickers',
  'wechat_login',
  // owner tools
  'wechat_contacts',
  'wechat_contact_request',
  'wechat_group_info',
  'wechat_archive_threads',
  'wechat_archive_search',
  'wechat_archive_history',
  'wechat_archive_message',
  'wechat_archive_media',
];

function inboundDm(id: string): Parameters<typeof Object>[0] extends never ? never : any {
  return {
    id,
    ref: {
      accountId: asAccountId('default'),
      chatJid: asChatJid('wxid_peer'),
      senderId: 'wxid_peer',
    },
    serverMessageId: `srv-${id}`,
    kind: 'text',
    text: 'hello',
    media: [],
    mentioned: false,
    createdAt: new Date().toISOString(),
  };
}

test('assembly registers all sixteen WeChat tools and disposeTools unregisters them', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const names = host.toolRegistrations.map((d) => d['name']);
    assert.deepEqual(names, EXPECTED_TOOL_NAMES);
    assert.equal(host.liveToolCount(), 16);
    // Every registered definition carries the registry-required fields.
    for (const def of host.toolRegistrations) {
      assert.equal(typeof def['description'], 'string');
      assert.equal(typeof (def['parameters'] as Record<string, unknown>)['type'], 'string');
      assert.equal(typeof (def['output'] as Record<string, unknown>)['render'], 'function');
      assert.equal(typeof def['execute'], 'function');
    }
    binding.disposeTools();
    assert.equal(host.liveToolCount(), 0);
  } finally {
    await binding.runtime.stop();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

test('a routed Agent runs wechat_history end-to-end; an unknown Agent is denied', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const result = await binding.runtime.handleInbound(inboundDm('m1'));
    assert.equal(result.admitted, true);
    assert.equal(host.agents.created.length, 1);
    const agentId = `agent-${host.agents.created[0]}`;

    const history = host.toolRegistrations.find((d) => d['name'] === 'wechat_history') as {
      execute: (args: unknown, exec: unknown) => Promise<unknown>;
    };
    const value = (await history.execute(
      { limit: 5 },
      { agent: { id: agentId }, signal: new AbortController().signal },
    )) as { items: unknown[] };
    assert.deepEqual(value, { items: [] });
    // The call reached the native client scoped to the DM chat.
    assert.equal(host.nativeCalls.length, 1);
    assert.equal(host.nativeCalls[0]!.method, 'messages');
    assert.equal(host.nativeCalls[0]!.args[0], 'default');
    assert.equal(host.nativeCalls[0]!.args[1], 'wxid_peer');

    // An Agent without a route binding is rejected before any native call.
    await assert.rejects(
      history.execute({}, { agent: { id: 'agent-stranger' }, signal: new AbortController().signal }),
      /not bound to a WeChat chat/i,
    );
    assert.equal(host.nativeCalls.length, 1, 'denied calls never reach the client');
  } finally {
    await binding.runtime.stop();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

test('assembly without ctx.tools skips tool registration', async () => {
  const host = makeFakeHost();
  const ctx = { ...host.ctx, tools: undefined } as unknown as typeof host.ctx;
  const binding = await assembleWechatBinding({
    ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    assert.deepEqual(host.toolRegistrations, []);
    binding.disposeTools(); // no-op must not throw
  } finally {
    await binding.runtime.stop();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

test('bindWechatHost cleanup unregisters the tools it registered', async () => {
  const host = makeFakeHost();
  const binding = bindWechatHost(host.ctx, CONFIG, {
    createNativeClient: fakeClientFactory(host),
    retryInitialMs: 10,
  });
  await binding.ready;
  assert.equal(host.liveToolCount(), 16);
  await disposeAll(host);
  assert.equal(host.liveToolCount(), 0, 'plugin unload removes every tool');
});

// ---------------------------------------------------------------------------
// M4: model-side visibility via ToolRegistrationOptions.when
// ---------------------------------------------------------------------------

interface RecordedRegistration {
  name: string;
  when?: ((context: unknown) => boolean | Promise<boolean>) | undefined;
}

function recordedToolRegistrations(host: FakeHost): Array<RecordedRegistration> {
  return host.toolRegistrations.map((def) => ({
    name: String(def['name']),
    when: host.registerWhens.get(String(def['name'])),
  }));
}

test('owner tools are model-invisible outside the owner DM; archive stays hidden', async () => {
  const host = makeFakeHost();
  const config: WechatConfigView = { ...CONFIG, tools: { ownerAllowFrom: ['wxid_peer'] } };
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    // Owner DM agent.
    const dm = await binding.runtime.handleInbound(inboundDm('m1'));
    assert.equal(dm.admitted, true);
    const ownerAgentId = `agent-${host.agents.created[0]}`;
    // Group agent (group allowlist admits the chat) — a separate fake host so
    // the two assemblies do not double-register into one registry.
    const groupHost = makeFakeHost();
    const groupConfig: WechatConfigView = {
      ...CONFIG,
      groupPolicy: 'allowlist',
      groupAllowFrom: ['room@chatroom'],
    };
    const groupBinding = await assembleWechatBinding({
      ctx: groupHost.ctx,
      config: groupConfig,
      log: () => {},
      createNativeClient: fakeClientFactory(groupHost),
    });
    const groupMsg = {
      ...inboundDm('g1'),
      ref: {
        accountId: asAccountId('default'),
        chatJid: asChatJid('room@chatroom'),
        senderId: 'wxid_member',
      },
      mentioned: true,
    };
    const groupResult = await groupBinding.runtime.handleInbound(groupMsg);
    assert.equal(groupResult.admitted, true);
    const groupAgentId = `agent-${groupHost.agents.created[0]}`;

    const registrations = recordedToolRegistrations(host);
    assert.equal(registrations.length, 16);
    const groupRegistrations = groupHost.toolRegistrations.map((def) => ({
      name: String(def['name']),
      when: groupHost.registerWhens.get(String(def['name'])),
    }));
    const groupWhenOf = (name: string) => {
      const found = groupRegistrations.find((r) => r.name === name);
      assert.ok(found?.when, `${name} registered with a when clause`);
      return found!.when!;
    };

    // Owner DM sees owner tools; a group agent does not.
    const whenOf = (name: string) => {
      const found = registrations.find((r) => r.name === name);
      assert.ok(found?.when, `${name} registered with a when clause`);
      return found!.when!;
    };
    assert.equal(await whenOf('wechat_contacts')({ agent: { id: ownerAgentId } }), true);
    assert.equal(await whenOf('wechat_stickers')({ agent: { id: ownerAgentId } }), true);
    assert.equal(await groupWhenOf('wechat_contacts')({ agent: { id: groupAgentId } }), false);
    assert.equal(await groupWhenOf('wechat_login')({ agent: { id: groupAgentId } }), false);
    // Current-chat tools are visible to both.
    assert.equal(await groupWhenOf('wechat_history')({ agent: { id: groupAgentId } }), true);
    assert.equal(await whenOf('wechat_history')({ agent: { id: ownerAgentId } }), true);
    // Archive tools are model-invisible for everyone (default deny).
    for (const name of [
      'wechat_archive_threads',
      'wechat_archive_search',
      'wechat_archive_history',
      'wechat_archive_message',
      'wechat_archive_media',
    ]) {
      assert.equal(await whenOf(name)({ agent: { id: ownerAgentId } }), false, name);
    }
    // Non-WeChat agents see nothing.
    assert.equal(await whenOf('wechat_history')({ agent: { id: 'agent-stranger' } }), false);
    assert.equal(await whenOf('wechat_history')({}), false);
    // A throwing checker degrades to invisible, never fails the step.
    assert.equal(await whenOf('wechat_history')({ agent: undefined }), false);

    await groupBinding.runtime.stop();
    await groupBinding.replyHandle.dispose();
    await groupBinding.domain.close();
  } finally {
    await binding.runtime.stop();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

// ---------------------------------------------------------------------------
// Calls (M5): the seventeenth tool and the coordinator
// ---------------------------------------------------------------------------

const CALLS_CONFIG = {
  voice: {
    enabled: true,
    mode: 'agent',
    agentId: 'agent-calls',
    inboundPolicy: 'allowlist',
    inboundAllowFrom: ['wxid_alice'],
    outboundPolicy: 'allowlist',
    outboundAllowTo: ['wxid_bob'],
    outboundToolAllowFrom: ['wxid_alice'],
    toolPolicy: 'safe-read-only',
    allowConsultSessionPersistence: true,
    consultation: { timeoutMs: 5_000, summary: true, recentMessages: 5 },
    sessionScope: 'per-contact',
    maxDurationSeconds: 600,
    realtime: { provider: 'openai', apiKeyCredentialRef: 'OPENAI_API_KEY' },
    transcription: { provider: 'openai' },
  },
} as const;

test('a calls config arms the seventeenth tool and the coordinator', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: { ...CONFIG, calls: CALLS_CONFIG } as WechatConfigView,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const names = host.toolRegistrations.map((d) => d['name']);
    assert.deepEqual(names, [...EXPECTED_TOOL_NAMES, 'wechat_call']);
    assert.equal(host.liveToolCount(), 17);
    assert.ok(binding.coordinator, 'coordinator assembled');
    assert.equal(binding.coordinator!.status().available, true);
    assert.equal(binding.coordinator!.status().state, 'disabled', 'not started until the owner starts it');
  } finally {
    binding.disposeTools();
    await binding.disposeCalls?.();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

test('without a calls config only the sixteen base tools register', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const names = host.toolRegistrations.map((d) => d['name']);
    assert.deepEqual(names, EXPECTED_TOOL_NAMES);
    assert.equal(host.liveToolCount(), 16);
    assert.equal(binding.coordinator, undefined);
    assert.equal(binding.disposeCalls, undefined);
  } finally {
    binding.disposeTools();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

// ---------------------------------------------------------------------------
// Message push (wechat_push): whitelist-delivery tool for any DSH session
// ---------------------------------------------------------------------------

test('a push config arms wechat_push for every session and delivers through the outbox', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: {
      ...CONFIG,
      push: { enabled: true, targets: ['wxid_boss', '555@chatroom'] },
    } as WechatConfigView,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const names = host.toolRegistrations.map((d) => d['name']);
    assert.deepEqual(names, [...EXPECTED_TOOL_NAMES, 'wechat_push']);
    assert.equal(host.liveToolCount(), 17);

    // Model-side visibility: armed for an agent with NO WeChat route binding.
    const when = host.registerWhens.get('wechat_push');
    assert.equal(typeof when, 'function');
    assert.equal(await when!({ agent: { id: 'agent-cli-no-route' } }), true);

    // End-to-end dispatch from a non-WeChat agent through the durable outbox.
    const def = host.toolRegistrations.find((d) => d['name'] === 'wechat_push') as {
      execute: (args: unknown, exec: unknown) => Promise<unknown>;
    };
    const result = (await def.execute(
      { target: 'wxid_boss', text: '  nightly build is green ✅  ' },
      { agent: { id: 'agent-cli-no-route' }, signal: new AbortController().signal },
    )) as { status: string; target: string };
    assert.equal(result.status, 'sent');
    assert.equal(result.target, 'wxid_boss');
    const send = host.sends.find((s) => s.text === 'nightly build is green ✅');
    assert.ok(send, 'push reached the native transport');
    assert.equal(send?.chatJid, 'wxid_boss');

    // Whitelist enforcement at dispatch time.
    await assert.rejects(
      def.execute(
        { target: 'wxid_stranger', text: 'hi' },
        { agent: { id: 'agent-cli-no-route' }, signal: new AbortController().signal },
      ),
      /not in the configured push targets/,
    );
  } finally {
    binding.disposeTools();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

test('without a push config the wechat_push tool never registers', async () => {
  const host = makeFakeHost();
  const binding = await assembleWechatBinding({
    ctx: host.ctx,
    config: CONFIG,
    log: () => {},
    createNativeClient: fakeClientFactory(host),
  });
  try {
    const names = host.toolRegistrations.map((d) => d['name']);
    assert.equal(names.includes('wechat_push'), false);
    assert.equal(host.liveToolCount(), 16);
  } finally {
    binding.disposeTools();
    await binding.replyHandle.dispose();
    await binding.domain.close();
  }
});

// ---------------------------------------------------------------------------
// Settings directory search (wxid / group pickers)
// ---------------------------------------------------------------------------

interface FakeRouteRequest {
  method?: string;
  url?: string;
}

interface FakeRouteResponse {
  status?: number;
  headers?: Record<string, string>;
  body?: string | undefined;
}

function fakeWebServer(): {
  routes: Map<string, (req: FakeRouteRequest, res: unknown) => void>;
  webServer: HostContextLike['webServer'];
} {
  const routes = new Map<string, (req: FakeRouteRequest, res: unknown) => void>();
  const webServer = {
    register: (route: { kind: string; path: string; handler: (req: FakeRouteRequest, res: unknown) => void }) => {
      routes.set(route.path, route.handler);
      return () => {
        routes.delete(route.path);
      };
    },
  };
  return { routes, webServer: webServer as never };
}

function fakeResponse() {
  const chunks: FakeRouteResponse = {};
  const res = {
    writeHead: (status: number, headers: Record<string, string>) => {
      chunks.status = status;
      chunks.headers = headers;
    },
    end: (body?: string) => {
      chunks.body = body;
    },
  };
  return { res, chunks };
}

test('createDirectorySearch filters contacts and groups from the local profile', async () => {
  const client = {
    contacts: async () => ({
      items: [
        { username: 'wxid_alice', displayName: 'Alice', alias: 'ali', remark: null, nickname: 'AliceNick' },
        { username: 'wxid_bob', displayName: 'Bob', alias: null, remark: null, nickname: 'bobby' },
      ],
      nextCursor: undefined,
    }),
    threads: async () => ({
      items: [
        { id: '123@chatroom', kind: 'group', displayName: 'Family' },
        { id: 'wxid_alice', kind: 'direct', displayName: 'Alice' },
        { id: '456@chatroom', kind: 'group', displayName: 'Work crew' },
      ],
      nextCursor: undefined,
    }),
  };
  const search = createDirectorySearch(client as never, 'default');

  const both = await search('ali', 'both');
  assert.deepEqual(both.contacts.map((c) => c.id), ['wxid_alice'], 'alias/username hit');
  assert.deepEqual(both.groups, [], 'contacts kind never returns groups');

  const groups = await search('fam', 'groups');
  assert.deepEqual(groups.groups.map((g) => g.id), ['123@chatroom']);
  assert.deepEqual(groups.contacts, []);

  const empty = await search('   ', 'both');
  assert.deepEqual(empty, { contacts: [], groups: [] });

  const miss = await search('zzz-nothing', 'both');
  assert.deepEqual(miss, { contacts: [], groups: [] });
});

test('the search route answers 503 while parked and 400/405 for bad requests', async () => {
  const host = makeFakeHost();
  const { routes, webServer } = fakeWebServer();
  (host.ctx as { webServer?: unknown }).webServer = webServer;

  const binding = bindWechatHost(host.ctx, CONFIG, {
    createNativeClient: fakeClientFactory(host, { failStatus: true }),
    retryInitialMs: 10,
  });
  try {
    await binding.ready; // parked (daemon unreachable in this fake)
    const handler = routes.get('/plugins/wechat-channel/search');
    assert.equal(typeof handler, 'function');

    const parked = fakeResponse();
    handler!({ method: 'GET', url: '/plugins/wechat-channel/search?q=ali' }, parked.res);
    await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(parked.chunks.status, 503);

    const badQuery = fakeResponse();
    handler!({ method: 'GET', url: '/plugins/wechat-channel/search' }, badQuery.res);
    assert.equal(badQuery.chunks.status, 400);

    const method = fakeResponse();
    handler!({ method: 'POST', url: '/plugins/wechat-channel/search?q=ali' }, method.res);
    assert.equal(method.chunks.status, 405);
  } finally {
    await disposeAll(host);
  }
});

test('after activation the search route serves whitelist-ready directory rows', async () => {
  const host = makeFakeHost();
  const { routes, webServer } = fakeWebServer();
  (host.ctx as { webServer?: unknown }).webServer = webServer;
  host.directoryContacts.push(
    { username: 'wxid_alice', displayName: 'Alice', alias: 'ali', remark: null, nickname: null },
    { username: 'wxid_bob', displayName: 'Bob', alias: null, remark: null, nickname: null },
  );
  host.directoryThreads.push(
    { id: '123@chatroom', kind: 'group', displayName: 'Family' },
    { id: 'wxid_alice', kind: 'direct', displayName: 'Alice' },
  );

  const binding = bindWechatHost(host.ctx, CONFIG, {
    createNativeClient: fakeClientFactory(host),
    retryInitialMs: 10,
  });
  try {
    await binding.ready;
    const handler = routes.get('/plugins/wechat-channel/search');
    assert.equal(typeof handler, 'function');

    const hit = fakeResponse();
    handler!({ method: 'GET', url: '/plugins/wechat-channel/search?q=ali' }, hit.res);
    await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(hit.chunks.status, 200);
    const payload = JSON.parse(hit.chunks.body ?? '{}') as {
      contacts: Array<{ id: string; name: string; alias?: string }>;
      groups: Array<{ id: string; name: string }>;
    };
    assert.deepEqual(payload.contacts, [{ id: 'wxid_alice', name: 'Alice', alias: 'ali' }]);
    assert.deepEqual(payload.groups, []);

    const groupHit = fakeResponse();
    handler!({ method: 'GET', url: '/plugins/wechat-channel/search?q=fam&kind=groups' }, groupHit.res);
    await new Promise((resolve) => setTimeout(resolve, 5));
    assert.equal(groupHit.chunks.status, 200);
    const groupPayload = JSON.parse(groupHit.chunks.body ?? '{}') as {
      groups: Array<{ id: string; name: string }>;
    };
    assert.deepEqual(groupPayload.groups, [{ id: '123@chatroom', name: 'Family' }]);
  } finally {
    await disposeAll(host);
  }
});
