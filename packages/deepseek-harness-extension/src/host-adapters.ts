/**
 * Host adapters — thin, pure wrappers that bind structural "host-like"
 * objects (the shared native client, DSH credentials, the agent registry,
 * and the native send transport) to this package's port types.
 *
 * Hard rules for this module:
 *  - runtime imports from `@deepseek-ai/*` are forbidden; everything here is
 *    testable with plain fakes;
 *  - relative imports use explicit `.ts` specifiers because the test runner
 *    is `node --test --experimental-strip-types src/*.test.ts` (no JS emit);
 *  - no TypeScript parameter properties (strip-types cannot lower them).
 */

import type {
  AccountId,
  ChatJid,
  FollowupMessage,
} from './contracts.ts';
import { asAccountId, asChatJid } from './contracts.ts';
import type {
  EventPage,
  NativeClientPort,
  NativeEventView,
  NativeIdentityView,
  NativeStatusView,
  ResolvedSecret,
  SecretResolverPort,
} from './ports.ts';
import type {
  AgentCreateOptions,
  AgentFactory,
  AgentResumeOptions,
  LiveAgent,
} from './session-router.ts';
import type { OutboxDeps } from './outbox.ts';

// ---------------------------------------------------------------------------
// 1. Native token validation
// ---------------------------------------------------------------------------

/** A `credentialRef(...)` resolves to a native daemon token within these bounds. */
export const NATIVE_TOKEN_MIN_CHARS = 32;
export const NATIVE_TOKEN_MAX_CHARS = 1024;

/**
 * Validate the native token shape without ever echoing the token in the error.
 * Accepts only [32,1024] chars of printable ASCII (0x20–0x7E), no CR/LF.
 */
export function validateNativeToken(token: string): string {
  if (
    token.length < NATIVE_TOKEN_MIN_CHARS ||
    token.length > NATIVE_TOKEN_MAX_CHARS
  ) {
    throw new Error(
      `Native token must be ${NATIVE_TOKEN_MIN_CHARS}-${NATIVE_TOKEN_MAX_CHARS} chars`,
    );
  }
  for (let i = 0; i < token.length; i++) {
    const code = token.charCodeAt(i);
    if (code === 0x0a || code === 0x0d) {
      throw new Error('Native token must not contain newlines');
    }
    if (code < 0x20 || code > 0x7e) {
      throw new Error('Native token must be printable ASCII');
    }
  }
  return token;
}

// ---------------------------------------------------------------------------
// 2. Native client adapter
// ---------------------------------------------------------------------------

/**
 * Structural subset of the shared `NativeWeChatClient` this module needs.
 * No runtime import from `@agent-wechat/shared`; callers pass any object
 * satisfying these methods (the real client, or a test fake).
 */
export interface SharedNativeClientLike {
  status(): Promise<any>;
  accounts(): Promise<any>;
  events(accountId: string, after?: string, limit?: number): Promise<any>;
  watch(
    accountId: string,
    after: string,
    signal: AbortSignal,
    opts?: { onActivity?: () => void },
  ): AsyncGenerator<any>;
  ack(accountId: string, consumerId: string, cursor: string): Promise<void>;
  message(accountId: string, messageId: string): Promise<any>;
  messages(
    accountId: string,
    chatJid: string,
    opts: { limit?: number; before?: string; after?: string },
    signal?: AbortSignal,
  ): Promise<any>;
  sendText(
    accountId: string,
    threadId: string,
    text: string,
    key: string,
    quoteOrOptions?: any,
  ): Promise<any>;
  sendByKey(accountId: string, key: string): Promise<any>;
}

/** Pick/coerce a native status object into the port's view shape. */
function toStatusView(raw: any): NativeStatusView {
  const version: any = raw?.version ?? {};
  const identity: NativeIdentityView = {
    product: String(version.product ?? ''),
    backend: String(version.backend ?? ''),
    version: String(version.version ?? ''),
    apiVersion: Number(version.apiVersion ?? 0),
    capabilities: Array.isArray(version.capabilities)
      ? version.capabilities.map((capability: unknown) => String(capability))
      : [],
  };
  return {
    serverId: String(raw?.serverId ?? ''),
    pid: Number(raw?.pid ?? 0),
    ownershipGeneration: String(raw?.ownershipGeneration ?? ''),
    version: identity,
  };
}

/** Pick/coerce a native event object into the port's view shape. */
function toEventView(raw: any): NativeEventView {
  return {
    cursor: String(raw?.cursor ?? ''),
    kind: String(raw?.kind ?? ''),
    data: (raw?.data && typeof raw.data === 'object' ? raw.data : {}) as Record<string, unknown>,
    createdAt: String(raw?.createdAt ?? ''),
  };
}

/** Wrap a shared native client as a `NativeClientPort`. */
export function createNativeClientAdapter(client: SharedNativeClientLike): NativeClientPort {
  return {
    status: async () => toStatusView(await client.status()),

    accounts: async () => {
      const page = await client.accounts();
      const items: unknown[] = Array.isArray(page?.items) ? page.items : [];
      return items.map((item) => asAccountId(String(item)));
    },

    events: async (
      accountId: AccountId,
      after?: string,
      limit?: number,
    ): Promise<EventPage> => {
      const page = await client.events(accountId, after, limit);
      const items: unknown[] = Array.isArray(page?.items) ? page.items : [];
      return {
        items: items.map(toEventView),
        nextCursor: String(page?.nextCursor ?? ''),
      };
    },

    watch: async function* (
      accountId: AccountId,
      after: string,
      signal: AbortSignal,
      onActivity?: () => void,
    ): AsyncGenerator<NativeEventView> {
      const opts = onActivity === undefined ? undefined : { onActivity };
      for await (const raw of client.watch(accountId, after, signal, opts)) {
        yield toEventView(raw);
      }
    },

    ack: (accountId: AccountId, consumerId: string, cursor: string) =>
      client.ack(accountId, consumerId, cursor),

    message: async (accountId: AccountId, messageId: string) =>
      (await client.message(accountId, messageId)) as Record<string, unknown>,

    messages: async (
      accountId: AccountId,
      chatJid: ChatJid,
      opts: { limit?: number; before?: string; after?: string },
      signal?: AbortSignal,
    ) => {
      const page = await client.messages(accountId, chatJid, opts, signal);
      const items: unknown[] = Array.isArray(page?.items) ? page.items : [];
      return {
        items: items.map((item) => item as Record<string, unknown>),
        ...(page?.nextCursor == null ? {} : { nextCursor: String(page.nextCursor) }),
      };
    },
  };
}

// ---------------------------------------------------------------------------
// 3. Secret resolver adapter
// ---------------------------------------------------------------------------

/** Structural subset of a DSH credential resolver. */
export interface CredentialResolverLike {
  resolve(ref: string): Promise<{ value: string; source: string } | undefined>;
}

/** Wrap a DSH credential resolver as a `SecretResolverPort` (no caching). */
export function createSecretResolverAdapter(
  credentials: CredentialResolverLike,
): SecretResolverPort {
  return {
    resolve: async (ref) => {
      const found = await credentials.resolve(ref);
      if (found === undefined) return undefined;
      const resolved: ResolvedSecret = {
        value: found.value,
        source: found.source,
      };
      return resolved;
    },
    resolveNativeToken: async (ref) => {
      const found = await credentials.resolve(ref);
      if (found === undefined) {
        throw new Error(`Native token credential not found for ref: ${ref}`);
      }
      return validateNativeToken(found.value);
    },
  };
}

// ---------------------------------------------------------------------------
// 4. Agent factory adapter
// ---------------------------------------------------------------------------

/** Structural subset of the DSH agent registry (`ctx.agents`). */
export interface AgentRegistryLike {
  create(options: any): Promise<any>;
  resume(options: any): Promise<any>;
}

/**
 * Turn a host agent handle into the router's `LiveAgent`. Defensive about a
 * missing `.agent` sub-handle: fall back to the handle itself for id/followup.
 * Options are passed through verbatim (the router already made them
 * exactOptionalPropertyTypes-safe).
 */
function toLiveAgent(handle: any): LiveAgent {
  const inner = handle?.agent ?? handle;
  const id = String(inner?.id ?? handle?.id ?? '');
  return {
    id,
    followup(message: unknown) {
      void inner.followup(message);
    },
    dispose() {
      return handle.dispose() as Promise<void>;
    },
  };
}

/** The host's live default model selection (`ctx.agentDefaultModel`). */
export interface AgentDefaultModelLike {
  currentSelection(): { provider: string; model: string; reasoningEffort?: string | undefined };
}

/**
 * Wrap a DSH agent registry as the router's `AgentFactory`. When the host
 * exposes a default model selection every created/resumed Agent carries
 * `agentOptions.provider/model` read live at call time — without it a fresh
 * session Agent errors with "has no provider/model" on its first turn.
 */
export function createAgentFactoryAdapter(
  agents: AgentRegistryLike,
  defaultModel?: AgentDefaultModelLike | undefined,
): AgentFactory {
  const agentOptions =
    defaultModel === undefined
      ? undefined
      : {
          get provider() {
            return defaultModel.currentSelection().provider;
          },
          get model() {
            return defaultModel.currentSelection().model;
          },
          get reasoningEffort() {
            return defaultModel.currentSelection().reasoningEffort;
          },
        };
  const withDefaults = (opts: { agentOptions?: Record<string, unknown> }): Record<string, unknown> => ({
    ...opts,
    ...(agentOptions === undefined
      ? {}
      : { agentOptions: { ...(opts.agentOptions ?? {}), ...agentOptions } }),
  });
  return {
    create: async (opts: AgentCreateOptions) => toLiveAgent(await agents.create(withDefaults(opts))),
    resume: async (opts: AgentResumeOptions) => toLiveAgent(await agents.resume(withDefaults(opts))),
  };
}

// ---------------------------------------------------------------------------
// 5. Followup → DSH UserMessage structural shape
// ---------------------------------------------------------------------------

/**
 * Build the frozen structural shape DSH expects for a UserMessage. No
 * `@deepseek-ai/dsh-llm` import: `agent.followup()` only reads these fields.
 */
export function toDshUserMessage(message: FollowupMessage): {
  readonly id: string;
  readonly role: 'user';
  readonly content: readonly unknown[];
  readonly source: { readonly kind: 'user' };
} {
  const source: { kind: 'user'; source?: string } = { kind: 'user' };
  if (message.source !== undefined) source.source = message.source;
  return Object.freeze({
    id: message.id,
    role: 'user' as const,
    content: Object.freeze([{ type: 'text', text: message.text }]),
    source: Object.freeze(source),
  });
}

// ---------------------------------------------------------------------------
// 6. Outbox transport adapter
// ---------------------------------------------------------------------------

/**
 * Wrap a shared native client as the two outbox transport functions. The
 * native send intent uses `serverId`/`localMessageId`/`clientId`; null/empty
 * serverId means the send was not accepted and we fail closed.
 */
export function createOutboxTransport(client: SharedNativeClientLike): {
  sendText: NonNullable<OutboxDeps['sendText']>;
  sendByKey: NonNullable<OutboxDeps['sendByKey']>;
} {
  /** Extract non-empty ids from a NativeSendIntent, or throw without secrets. */
  function ids(intent: any): { serverId: string; serverMessageId: string } {
    const serverId = intent?.serverId;
    if (serverId === null || serverId === undefined || String(serverId).length === 0) {
      throw new Error('Native send was not accepted (no serverId)');
    }
    const serverMessageId = String(
      intent?.localMessageId ?? intent?.clientId ?? serverId,
    );
    return { serverId: String(serverId), serverMessageId };
  }

  return {
    sendText: async (accountId, chatJid, text, key) => {
      const intent = await client.sendText(accountId, chatJid, text, key);
      return ids(intent);
    },
    sendByKey: async (accountId, key) => {
      const intent = await client.sendByKey(accountId, key);
      if (intent === undefined || intent === null) return undefined;
      const base = ids(intent);
      /*
       * `text` is required for canonical readback confirmation. Only include
       * it when the intent confidently exposes the sent text; otherwise
       * return undefined so the outbox treats the send as unconfirmed
       * (fail-closed) rather than verifying against a guessed payload.
       */
      const rawText = intent?.text ?? intent?.content;
      if (typeof rawText !== 'string') return undefined;
      return { ...base, text: rawText };
    },
  };
}
