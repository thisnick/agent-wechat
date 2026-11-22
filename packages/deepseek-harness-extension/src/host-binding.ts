/**
 * M2/M3 host binding — DESIGN.md §2 layering.
 *
 * This is the ONLY module that reaches real DSH services. Everything below it
 * stays pure and host-agnostic (see `ports.ts`, `runtime.ts`). The binding:
 *   - opens the durable storage domain (`ctx.storageDomain`),
 *   - registers the reply projection (`ctx.sessionProjections`),
 *   - resolves the daemon token via `ctx.credentials` (per operation),
 *   - binds the Agent registry (`ctx.agents`) as the router's factory,
 *   - constructs the native client from `@agent-wechat/shared`,
 *   - assembles router + outbox + runtime and starts the inbound consumers,
 *   - drains committed replies on `session/event` turn completions.
 *
 * Host typing is deliberately structural: the runtime `@deepseek-ai/*` brand
 * helpers (`credentialRef`, `SessionId`) are identity casts at runtime, so this
 * module validates the same grammars locally and casts, keeping the bundle
 * free of heavyweight host imports. All registrations own an effect so fiber
 * disposal closes the domain, unregisters the projection, stops the loops and
 * removes the event listener.
 *
 * Import note: `.ts` specifiers everywhere (strip-types test runner, no emit).
 */

import { NativeCallsClient, NativeWeChatClient } from '@agent-wechat/shared';
import type { NativeCallWebSocketFactory } from '@agent-wechat/shared';
import {
  asAccountId,
  asSessionId,
  asServerId,
  stableHash32,
  type FollowupMessage,
  type InboundMessage,
  type RouteBinding,
} from './contracts.ts';
import { inboundFollowupText } from './inbound-media.ts';
import { debugTrace, traceStep } from './debug-trace.ts';
import { unwrapVolatileConfig } from './config-unwrap.ts';
import type { ReplyEventView, ReplyState } from './reply-collector.ts';
import { extractAssistantText, replyProjection } from './reply-collector.ts';
import {
  openWechatTables,
  wechatCallsDomainSpec,
  wechatDomainSpec,
  type TableView,
  type WechatTables,
} from './storage.ts';
import {
  createNativeClientAdapter,
  createSecretResolverAdapter,
  createAgentFactoryAdapter,
  toDshUserMessage,
  type AgentDefaultModelLike,
  createOutboxTransport,
  type AgentRegistryLike,
  type CredentialResolverLike,
  type SharedNativeClientLike,
} from './host-adapters.ts';
import { createRequire } from 'node:module';
import { randomUUID } from 'node:crypto';
import { resolveWeChatCallsConfig } from './calls/config.ts';
import type { ResolvedWeChatCallsConfig, ResolvedWeChatVoiceCallsConfig } from './calls/config.ts';
import { CallCoordinator, type CallsClientLike } from './calls/coordinator.ts';
import { CallSummaryStore } from './calls/memory.ts';
import { createOpenAISpeechProvider, type OpenAISocketFactory } from './calls/speech-openai.ts';
import { clipText } from './calls/transcript.ts';
import type { AdmissionConfig } from './admission.ts';
import type { LiveAgent, SessionRouter } from './session-router.ts';
import { createSessionRouter } from './session-router.ts';
import type { Outbox } from './outbox.ts';
import { createOutbox } from './outbox.ts';
import { createWechatRuntime, type WechatRuntime } from './runtime.ts';
import { createTrustedToolContext, DEFAULT_GRANTS } from './tool-context.ts';
import { currentChatTools } from './tools/chat.ts';
import { accountTools } from './tools/account.ts';
import { ownerTools } from './tools/owner.ts';
import { callTools } from './tools/call.ts';
import {
  createHostMediaSink,
  registerWechatTools,
  type HostAttachmentsLike,
} from './tools/register.ts';
import type { NativeToolsLike } from './tools/shared.ts';

// ---------------------------------------------------------------------------
// Structural host surface (real DSH Context satisfies this at runtime)
// ---------------------------------------------------------------------------

type LogFn = (level: 'info' | 'warn' | 'error', message: string) => void;

interface CredentialView {
  resolve(ref: string): Promise<{ value: string; source: string } | undefined>;
}

interface ProjectionHandleLike<S> {
  stateOf(session: unknown): S;
  dispose(): Promise<void>;
}

interface SessionProjectionRegistryLike {
  register(definition: unknown): Promise<ProjectionHandleLike<ReplyState>>;
}

interface StorageDomainLike {
  table(name: string): TableView<string, unknown>;
  global: { get(): { generation?: number }; set(value: { generation: number }): Promise<void> };
  close(): Promise<void>;
}

interface StorageDomainFacilityLike {
  open(spec: unknown): Promise<StorageDomainLike>;
}

/** The calls memory domain exposes only its global JSON record. */
interface CallsDomainLike {
  global: {
    get(): Record<string, unknown>;
    set(value: Record<string, unknown>): Promise<void>;
  };
  close(): Promise<void>;
}

interface SessionsLike {
  get(id: string): unknown;
  list(): readonly unknown[];
}

/** The subset of Cordis Context the binding touches. */
export interface HostContextLike {
  storageDomain: StorageDomainFacilityLike;
  sessionProjections: SessionProjectionRegistryLike;
  credentials: CredentialView;
  agents: AgentRegistryLike;
  sessions: SessionsLike;
  /** Host default model selection (`ctx.agentDefaultModel`); absent → Agents carry no provider/model. */
  agentDefaultModel?: AgentDefaultModelLike | undefined;
  /** DSH tool registry (`ctx.tools`); absent → tool registration is skipped. */
  tools?:
    | {
        register(
          definition: unknown,
          options?: { readonly when: (context: unknown) => boolean | Promise<boolean> },
        ): () => void;
      }
    | undefined;
  /** DSH attachment store (`ctx.attachments`); absent → no media sink. */
  attachments?: HostAttachmentsLike | undefined;
  /** DSH HTTP host (`ctx.webServer`); absent → the settings status endpoint is skipped. */
  webServer?: HostWebServerLike | undefined;
  /**
   * Cordis dependency-gated scope (`ctx.inject(deps, cb)`); used to bind the
   * settings status route on a context where `webServer` resolves (the service
   * is provided by another plugin, so plain property access can be undefined).
   */
  inject?: (deps: readonly string[], callback: (scoped: HostContextLike) => unknown) => unknown;
  effect(execute: () => unknown, label?: string): unknown;
  on(name: string, listener: (...args: any[]) => void, options?: unknown): () => void;
  logger?: Record<string, (...args: any[]) => void> | undefined;
}

/**
 * Minimal structural view of the host web server so the channel can expose a
 * read-only connection-status endpoint for the settings page. The handler owns
 * the whole response lifecycle (no framework), mirroring `dsh-host-webserver`.
 */
export interface HostWebServerLike {
  register(route: {
    readonly kind: 'exact' | 'prefix';
    readonly path: string;
    readonly handler: (
      req: { readonly method?: string },
      res: {
        writeHead(status: number, headers: Record<string, string>): unknown;
        end(body?: string): unknown;
      },
    ) => void | Promise<void>;
  }): () => void;
}

/** JSON payload served by the settings status endpoint (no secrets). */
export interface WechatStatusReport {
  readonly service: 'wechat-channel';
  readonly running: boolean;
  readonly attempts: number;
  readonly serverId?: string;
  readonly error?: string;
  readonly endpoint?: string;
  readonly accountId?: string;
  readonly checkedAt: string;
}

export function wechatStatusReport(
  state: { readonly running: boolean; readonly attempts: number; readonly serverId?: string; readonly error?: string },
  view: {
    readonly native?: {
      readonly endpoint?: string;
      readonly accountId?: string;
      /** Accepted so callers can pass a full config view; never surfaced. */
      readonly tokenCredentialRef?: string;
    };
  },
): WechatStatusReport {
  return {
    service: 'wechat-channel',
    running: state.running,
    attempts: state.attempts,
    ...(state.serverId === undefined ? {} : { serverId: state.serverId }),
    ...(state.error === undefined ? {} : { error: state.error }),
    ...(view.native?.endpoint === undefined ? {} : { endpoint: view.native.endpoint }),
    ...(view.native?.accountId === undefined ? {} : { accountId: view.native.accountId }),
    checkedAt: new Date().toISOString(),
  };
}

/**
 * Register `GET /plugins/wechat-channel/status` on the host web server. The
 * route lives outside the activation loop so the settings page can watch the
 * channel through park/retry progress; disposing the binding effect removes it.
 */
export function bindStatusRoute(
  webServer: HostWebServerLike,
  readState: () => {
    readonly running: boolean;
    readonly attempts: number;
    readonly serverId?: string;
    readonly error?: string;
  },
  view: { readonly native?: { readonly endpoint?: string; readonly accountId?: string } },
): (() => void) | undefined {
  try {
    return webServer.register({
      kind: 'exact',
      path: '/plugins/wechat-channel/status',
      handler: (req, res) => {
        if (req.method !== 'GET' && req.method !== 'HEAD') {
          res.writeHead(405, { Allow: 'GET, HEAD', 'Content-Length': '0' });
          res.end();
          return;
        }
        const body = JSON.stringify(wechatStatusReport(readState(), view));
        res.writeHead(200, {
          'Content-Type': 'application/json; charset=utf-8',
          'Cache-Control': 'no-store',
          'Content-Length': String(Buffer.byteLength(body)),
        });
        res.end(req.method === 'HEAD' ? undefined : body);
      },
    });
  } catch {
    // A name collision or a closed server must not block channel startup.
    return undefined;
  }
}

// ---------------------------------------------------------------------------
// Config surface
// ---------------------------------------------------------------------------

export interface WechatConfigView {
  readonly native?: {
    readonly accountId?: string;
    readonly endpoint?: string;
    readonly tokenCredentialRef?: string;
    readonly readinessTimeoutMs?: number;
  };
  readonly dmPolicy?: 'disabled' | 'allowlist' | 'open';
  readonly groupPolicy?: 'disabled' | 'allowlist';
  readonly dmAllowFrom?: readonly string[];
  readonly groupAllowFrom?: readonly string[];
  readonly groupSenderAllowFrom?: readonly string[];
  readonly requireMentionInGroups?: boolean;
  readonly historyLimit?: number;
  /** Tool-layer configuration (M3): owner wxids and grant overrides. */
  readonly tools?: {
    readonly ownerAllowFrom?: readonly string[];
    /** Per-scope grant overrides; a scope omitted keeps its default list. */
    readonly grants?: {
      readonly currentChat?: readonly string[];
      readonly owner?: readonly string[];
      readonly archive?: readonly string[];
      readonly call?: readonly string[];
    };
  };
  /** Calls configuration (M5); validated by resolveWeChatCallsConfig. */
  readonly calls?: unknown;
}

/** Credential refs are POSIX identifiers — same grammar the host enforces. */
const CREDENTIAL_REF_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
const DEFAULT_TOKEN_REF = 'WECHAT_DAEMON_TOKEN';
const DEFAULT_ENDPOINT = 'http://127.0.0.1:6790';
const DEFAULT_ACCOUNT = 'default';
const CONSUMER_ID = 'agent-wechat-host';

const asLog = (ctx: HostContextLike): LogFn => {
  const logger = ctx.logger;
  return (level, message) => {
    const sink = logger?.[level];
    if (typeof sink === 'function') sink.call(logger, message);
  };
};

/** Resolve the daemon token ref, rejecting an invalid ref before host branding. */
function resolveTokenRef(config: WechatConfigView): string {
  const ref = config.native?.tokenCredentialRef ?? DEFAULT_TOKEN_REF;
  if (!CREDENTIAL_REF_RE.test(ref)) {
    throw new Error(
      `wechat: native.tokenCredentialRef "${ref}" is not a valid credential ref ` +
        `(must match ${String(CREDENTIAL_REF_RE)}); set a valid ref or leave the default`,
    );
  }
  return ref;
}

function admissionFromConfig(config: WechatConfigView): AdmissionConfig {
  return {
    dmPolicy: config.dmPolicy ?? 'disabled',
    groupPolicy: config.groupPolicy ?? 'disabled',
    dmAllowFrom: [...(config.dmAllowFrom ?? [])],
    groupAllowFrom: [...(config.groupAllowFrom ?? [])],
    groupSenderAllowFrom: [...(config.groupSenderAllowFrom ?? [])],
    requireMentionInGroups: config.requireMentionInGroups ?? true,
  };
}

function sessionIdFor(routeKey: string) {
  return asSessionId(`wht-${stableHash32(routeKey)}`);
}

function toFollowup(message: InboundMessage, _binding: RouteBinding) {
  // No `source` provenance string: the followup object is spliced verbatim
  // into the session log, and the v4 format requires message `source` to be
  // an object `{kind}` — a bare string corrupts the log on the next resume.
  return {
    id: message.id,
    text: inboundFollowupText(message),
  };
}

/** The router delivers the FollowupMessage as a producer-owned UserMessage. */
function toUserMessage(message: FollowupMessage, _binding: RouteBinding) {
  return toDshUserMessage(message);
}

// ---------------------------------------------------------------------------
// Assembly (testable with fakes — no ctx.effect / ctx.on here)
// ---------------------------------------------------------------------------

export interface WechatBinding {
  readonly runtime: WechatRuntime;
  readonly replyHandle: ProjectionHandleLike<ReplyState>;
  readonly domain: StorageDomainLike;
  readonly tables: WechatTables;
  readonly projectionHandle: ProjectionHandleLike<ReplyState>;
  /** Durable outbound journal (exposed for diagnostics + tests). */
  readonly outbound: Outbox;
  readonly serverId: string;
  readonly consumerId: string;
  /** Unregister every registered WeChat tool (idempotent no-op when absent). */
  readonly disposeTools: () => void;
  /** Call coordinator (M5); present only when calls resolve with a speech key. */
  readonly coordinator?: CallCoordinator;
  /** Tear down calls wiring (event listener + calls domain). Idempotent. */
  readonly disposeCalls?: () => Promise<void>;
  /** Fold a committed session event into the reply projection state (test seam). */
  applyReply(state: ReplyState, event: ReplyEventView): ReplyState;
}

/** Structural native client the binding needs (real client or a test fake). */
export type BindingNativeClient = SharedNativeClientLike;

/**
 * Test seam: build the native client from endpoint + resolved token.
 * Defaults to the real `NativeWeChatClient` from `@agent-wechat/shared`.
 */
export type NativeClientFactory = (options: {
  readonly baseUrl: string;
  readonly token: string;
}) => BindingNativeClient;

export interface AssemblyDeps {
  readonly ctx: HostContextLike;
  readonly config: WechatConfigView;
  readonly log: LogFn;
  /** Optional test seam replacing the real native client. */
  readonly createNativeClient?: NativeClientFactory;
}

const defaultNativeClientFactory: NativeClientFactory = ({ baseUrl, token }) =>
  new NativeWeChatClient({ baseUrl, token }) as unknown as BindingNativeClient;

/** Minimal `ws` constructor view (structurally assignable to both sockets). */
type WsCtor = new (
  url: string,
  options?: { protocols?: string[]; headers?: Record<string, string>; maxPayload?: number },
) => unknown;

let wsCtor: WsCtor | undefined;

/**
 * Resolve `ws` lazily — a plugin load never touches the module until a calls
 * feature or realtime provider actually needs a socket. `ws` (not undici's
 * global WebSocket) is required because the calls client must set the
 * Authorization header, which the WHATWG client does not allow.
 */
function requireWs(): WsCtor {
  if (wsCtor === undefined) {
    wsCtor = (createRequire(import.meta.url)('ws') as { WebSocket: WsCtor }).WebSocket;
  }
  return wsCtor;
}

/** Calls control/media sockets: Authorization header + frame payload cap. */
const nodeCallSocketFactory: NativeCallWebSocketFactory = (url, options) => {
  const WebSocket = requireWs();
  return new WebSocket(url, {
    headers: { ...options.headers },
    ...(options.maxPayload !== undefined ? { maxPayload: options.maxPayload } : {}),
  }) as never;
};

/** OpenAI realtime sockets: auth rides the subprotocol, no headers needed. */
const realtimeSocketFactory: OpenAISocketFactory = (url, protocols) => {
  const WebSocket = requireWs();
  return new WebSocket(url, { protocols }) as never;
};

/** Every NativeToolsLike method the WeChat tools may call. */
const TOOL_CLIENT_METHODS = [
  'threads',
  'thread',
  'messages',
  'message',
  'searchMessages',
  'mediaAssets',
  'mediaContentStream',
  'sendText',
  'sendShare',
  'recallMessage',
  'stickerCollections',
  'customStickers',
  'customSticker',
  'prepareSticker',
  'addCustomSticker',
  'moveCustomSticker',
  'removeCustomSticker',
  'login',
  'operation',
  'disconnect',
  'logoutLocal',
  'forgetCredentials',
  'accounts',
  'contacts',
  'contact',
  'lookupContacts',
  'searchContacts',
  'removeContact',
  'contactRequests',
  'contactRequest',
  'sendContactRequest',
  'acceptContactRequest',
  'deleteContactRequest',
  'group',
  'groupMembers',
] as const;

/**
 * Tool-facing native client that resolves a fresh client per call so a
 * credential rotation is picked up on the next tool invocation.
 */
function createRoutedToolClient(fresh: () => Promise<BindingNativeClient>): NativeToolsLike {
  const out: Record<string, (...args: unknown[]) => Promise<unknown>> = {};
  for (const method of TOOL_CLIENT_METHODS) {
    out[method] = (...args: unknown[]) =>
      fresh().then((client) => {
        const fn = (client as unknown as Record<string, ((...a: unknown[]) => unknown) | undefined>)[
          method
        ];
        if (typeof fn !== 'function') {
          throw new Error(`wechat: native client is missing tool method "${method}"`);
        }
        return fn(...args);
      });
  }
  return out as unknown as NativeToolsLike;
}

/**
 * Async assembly of every module against real host services. The caller owns
 * an effect that disposes the returned handles and stops the runtime.
 */
export async function assembleWechatBinding(deps: AssemblyDeps): Promise<WechatBinding> {
  const { ctx, log } = deps;
  // Volatile schemastery fields arrive as frozen reference containers; unwrap
  // once per attempt so parked retries also pick up live settings updates.
  const config = unwrapVolatileConfig(deps.config);

  // 1. Durable storage domain + typed tables.
  const domain = await traceStep('assemble', 'storage-domain open', () =>
    ctx.storageDomain.open(wechatDomainSpec()),
  );
  let tables: WechatTables;
  try {
    tables = openWechatTables(domain);
  } catch (error) {
    await domain.close();
    throw error;
  }

  let replyHandle: ProjectionHandleLike<ReplyState>;
  // Calls wiring (M5) — assigned inside the try so the catch can tear it down.
  let coordinator: CallCoordinator | undefined;
  let disposeCalls: (() => Promise<void>) | undefined;
  try {
    // 2. Host-driven reply projection (committed assistant text per turn).
    replyHandle = await traceStep('assemble', 'projection register', () =>
      ctx.sessionProjections.register(replyProjection as unknown as Parameters<
        SessionProjectionRegistryLike['register']
      >[0]),
    );
  } catch (error) {
    await domain.close();
    throw error;
  }

  try {
    // 3. Credentials (re-resolve per operation, never cache the secret).
    const tokenRef = resolveTokenRef(config);
    const secrets = createSecretResolverAdapter(ctx.credentials as CredentialResolverLike);
    const endpoint = config.native?.endpoint ?? DEFAULT_ENDPOINT;
    const makeClient = deps.createNativeClient ?? defaultNativeClientFactory;

    // Build a fresh client whenever the resolved token rotates. The latest
    // token doubles as the calls socket credential (rotation is picked up on
    // the next coordinator reconnect, which rebuilds its client).
    let cachedToken: string | undefined;
    let cachedClient: BindingNativeClient | undefined;
    let latestToken = '';
    const freshClient = async (): Promise<BindingNativeClient> => {
      const token = await secrets.resolveNativeToken(tokenRef);
      if (cachedClient === undefined || cachedToken !== token) {
        cachedClient = makeClient({ baseUrl: endpoint, token });
        cachedToken = token;
      }
      latestToken = token;
      return cachedClient;
    };

    // Long-lived read client for the bridge/SSE (rotates on plugin reload).
    const readClient = await traceStep('assemble', `credential resolve (${tokenRef})`, () =>
      freshClient(),
    );
    const native = createNativeClientAdapter(readClient);
    const status = await traceStep('assemble', `hub status (${endpoint})`, () =>
      readClient.status(),
    );
    const serverId = asServerId(String(status.serverId ?? ''));

    // The mutation path re-resolves the token and builds a fresh client per send.
    const transport = {
      sendText: async (accountId: any, chatJid: any, text: string, key: string) =>
        createOutboxTransport(await freshClient()).sendText(accountId, chatJid, text, key),
      sendByKey: async (accountId: any, key: string) =>
        createOutboxTransport(await freshClient()).sendByKey(accountId, key),
    };

    // 4. Agent factory + session router. The host's live default model
    // selection rides along so fresh session Agents can actually run a turn.
    const agentFactory = createAgentFactoryAdapter(ctx.agents, ctx.agentDefaultModel);
    const router: SessionRouter = createSessionRouter({
      tables: { threads: tables.threads },
      agents: agentFactory,
      sessionIdFor,
      toUserMessage,
      now: Date.now,
    });

    // 5. Durable outbox with canonical readback (never auto-resend).
    const outbox: Outbox = createOutbox({
      tables: { outbox: tables.outbox },
      sendText: transport.sendText,
      sendByKey: transport.sendByKey,
      now: Date.now,
    });

    // 6. Session-id -> route reverse lookup (restart-safe, scans durable threads).
    const sessionRoutes = (sessionId: string): RouteBinding | undefined => {
      for (const [, record] of tables.threads.entries()) {
        if (record.sessionId === sessionId) return record as unknown as RouteBinding;
      }
      return undefined;
    };

    // 7. Reply state source bound to the registered projection handle.
    const replySource = {
      stateOf(sessionId: string): ReplyState | undefined {
        const session = ctx.sessions.get(sessionId);
        if (session === undefined) return undefined;
        return replyHandle.stateOf(session);
      },
    };

    // 8. Monotonic config generation from the durable global.
    const generation = (): number => domain.global.get().generation ?? 0;

    // 9. Orchestration runtime.
    const runtime = createWechatRuntime({
      native,
      router,
      outbox,
      outboxTable: tables.outbox,
      journal: tables.journal,
      admission: admissionFromConfig(config),
      replySource,
      sessionRoutes,
      serverId,
      consumerId: CONSUMER_ID,
      toFollowup,
      log,
      generation,
      idleEvictMs: 0, // idle eviction is wired by the timer in a later milestone
    });

    // 10. Tool registration (M3): 16 WeChat tools behind the trusted context.
    // Each call resolves a fresh client (token rotation) and derives identity
    // from the calling Agent id — never from model arguments.
    const grants = {
      currentChat: config.tools?.grants?.currentChat ?? DEFAULT_GRANTS.currentChat,
      owner: config.tools?.grants?.owner ?? DEFAULT_GRANTS.owner,
      archive: config.tools?.grants?.archive ?? DEFAULT_GRANTS.archive,
      call: config.tools?.grants?.call ?? DEFAULT_GRANTS.call,
    };
    const toolContext = createTrustedToolContext({
      bindingForAgent: (agentId: string) => router.bindingForAgent(agentId),
      config: {
        ownerAllowFrom: [...(config.tools?.ownerAllowFrom ?? [])],
        grants,
      },
      generation,
    });
    const toolClient: NativeToolsLike = createRoutedToolClient(freshClient);
    const media =
      ctx.attachments === undefined ? undefined : createHostMediaSink(ctx.attachments);
    // 11. Calls (M5) — fail-closed: without a resolvable speech API key the
    // coordinator is never created, the calls control socket never connects
    // and wechat_call is not registered (inbound callers time out instead).
    const callsConfig = resolveWeChatCallsConfig(config.calls);
    debugTrace('assemble', `calls config ${callsConfig === undefined ? 'disabled' : 'enabled'}`);
    const callsVoice: ResolvedWeChatVoiceCallsConfig | undefined = callsConfig?.voice;
    if (callsVoice !== undefined) {
      const apiKeyRef = callsVoice.realtime.apiKeyCredentialRef;
      const resolvedKey =
        apiKeyRef === undefined
          ? undefined
          : await ctx.credentials.resolve(apiKeyRef).catch(() => undefined);
      const apiKey =
        typeof resolvedKey?.value === 'string' && resolvedKey.value.length > 0
          ? resolvedKey.value
          : undefined;
      if (apiKey === undefined) {
        log('warn', 'wechat: calls configured but no speech API key resolved — calls stay disabled');
      } else {
        const speech = createOpenAISpeechProvider({
          apiKey,
          socketFactory: realtimeSocketFactory,
          ...(callsVoice.realtime.model !== undefined
            ? { realtimeModel: callsVoice.realtime.model }
            : {}),
          ...(callsVoice.transcription.model !== undefined
            ? { transcriptionModel: callsVoice.transcription.model }
            : {}),
          ...(callsVoice.realtime.voice !== undefined
            ? { voice: callsVoice.realtime.voice }
            : {}),
        });

        // Per-call consultation sessions: create (or resume) the session
        // agent, deliver the caller utterance and await the assistant text.
        const pendingCallReplies = new Map<
          string,
          Array<{ resolve: (text: string) => void; reject: (error: Error) => void }>
        >();
        const offCallEvents = ctx.on(
          'session/event',
          (session: unknown, event: { type?: string; data?: any }) => {
            if (event?.type !== 'assistant/message') return;
            const id = (session as { id?: string } | undefined)?.id;
            if (id === undefined) return;
            const waiters = pendingCallReplies.get(id);
            if (waiters === undefined || waiters.length === 0) return;
            // Interrupted output is a cancelled prefix, never the reply.
            if (event.data?.message?.interrupted) return;
            const text = extractAssistantText(event.data?.message?.content ?? []);
            if (!text) return;
            const waiter = waiters.shift();
            if (waiter === undefined) return;
            if (waiters.length === 0) pendingCallReplies.delete(id);
            waiter.resolve(text);
          },
          { global: true },
        );

        const timeoutMs = callsVoice.consultation.timeoutMs;
        const runCallTurn = async (sessionKey: string, text: string): Promise<string> => {
          let handle: LiveAgent;
          try {
            handle = await agentFactory.create({ sessionId: sessionKey });
          } catch {
            handle = await agentFactory.resume({ resumeSessionId: sessionKey });
          }
          let timer: ReturnType<typeof setTimeout> | undefined;
          let waiter: { resolve: (text: string) => void; reject: (error: Error) => void } | undefined;
          try {
            return await new Promise<string>((resolve, reject) => {
              waiter = { resolve, reject };
              const waiters = pendingCallReplies.get(sessionKey) ?? [];
              waiters.push(waiter);
              pendingCallReplies.set(sessionKey, waiters);
              timer = setTimeout(() => {
                reject(new Error(`consultation turn timed out after ${timeoutMs}ms`));
              }, timeoutMs);
              handle.followup(toDshUserMessage({ id: `call-${randomUUID()}`, text }));
            });
          } finally {
            if (timer !== undefined) clearTimeout(timer);
            // On timeout the waiter is still queued — remove it so a late
            // assistant message cannot resolve a future turn's promise.
            if (waiter !== undefined) {
              const waiters = pendingCallReplies.get(sessionKey);
              if (waiters !== undefined) {
                const index = waiters.indexOf(waiter);
                if (index >= 0) {
                  waiters.splice(index, 1);
                  if (waiters.length === 0) pendingCallReplies.delete(sessionKey);
                }
              }
            }
            await handle.dispose().catch(() => {});
          }
        };

        // Durable call memory in its own domain unit (wipable separately).
        const callsDomain = (await ctx.storageDomain.open(
          wechatCallsDomainSpec(),
        )) as unknown as CallsDomainLike;
        const callMemory = new CallSummaryStore({
          get: async () => callsDomain.global.get() as never,
          put: async (_key, value) => {
            await callsDomain.global.set(value as unknown as Record<string, unknown>);
          },
        });

        coordinator = new CallCoordinator({
          // callsVoice being defined implies callsConfig is defined; the
          // separate binding is only a TS narrowing limitation.
          config: callsConfig as ResolvedWeChatCallsConfig,
          accountId: config.native?.accountId ?? DEFAULT_ACCOUNT,
          ownerAllowFrom: [...(config.tools?.ownerAllowFrom ?? [])],
          speech,
          createClient: () =>
            new NativeCallsClient({
              baseUrl: endpoint,
              token: latestToken,
              accountId: config.native?.accountId ?? DEFAULT_ACCOUNT,
              webSocketFactory: nodeCallSocketFactory,
            }) as unknown as CallsClientLike,
          runCallTurn,
          summarizeCall: (sessionKey, transcript) =>
            runCallTurn(
              sessionKey,
              'Summarize this call transcript in two or three sentences for future recall. '
                + `Reply with the summary only.\n\n${clipText(transcript, 24_000)}`,
            ),
          callMemory,
          log,
          now: Date.now,
        });

        disposeCalls = async () => {
          offCallEvents();
          coordinator?.stop();
          await callsDomain.close().catch(() => {});
        };
      }
    }

    const toolDeps = {
      client: toolClient,
      context: toolContext,
      accountId: asAccountId(config.native?.accountId ?? DEFAULT_ACCOUNT),
      log,
      now: Date.now,
      ...(media === undefined ? {} : { media }),
      ...(coordinator === undefined ? {} : { calls: coordinator }),
      ...(callsVoice?.outboundToolAllowFrom === undefined
        ? {}
        : { outboundToolAllowFrom: callsVoice.outboundToolAllowFrom }),
    };

    const wechatTools = [
      ...currentChatTools(toolDeps),
      ...accountTools(toolDeps),
      ...ownerTools(toolDeps),
      ...(coordinator === undefined ? [] : callTools(toolDeps)),
    ];
    debugTrace('assemble', `registering ${wechatTools.length} tools (ctx.tools ${ctx.tools === undefined ? 'absent' : 'present'})`);
    const disposeTools: () => void =
      ctx.tools === undefined
        ? () => {}
        : registerWechatTools({
            tools: wechatTools,
            // Boundary cast: the registry's ToolDefinition widens exec beyond
            // ToolRunContextLike; the tool layer only reads agent.id/signal.
            register: (definition, registrationOptions) =>
              ctx.tools!.register(definition, registrationOptions),
            ...(media === undefined
              ? {}
              : { resolveImageRef: (ref: string) => media.refOf(ref) }),
            // M4: per-step model-side visibility. Tools the calling Agent is
            // not granted (owner/archive scope, non-WeChat agents) never reach
            // the model's declarations or dispatch. Never throws.
            activation: async (toolName, context) => {
              const agentId = context.agent?.id;
              if (agentId === undefined) return false;
              try {
                const principal = await toolContext.forAgent(agentId);
                return principal !== undefined && toolContext.allows(principal, toolName);
              } catch {
                return false;
              }
            },
          });

    debugTrace('assemble', 'tools registered');
    return {
      runtime,
      replyHandle,
      domain,
      tables,
      projectionHandle: replyHandle,
      outbound: outbox,
      serverId,
      consumerId: CONSUMER_ID,
      disposeTools,
      ...(coordinator === undefined ? {} : { coordinator }),
      ...(disposeCalls === undefined ? {} : { disposeCalls }),
      applyReply: (state, event) => replyProjection.apply(state, event),
    };
  } catch (error) {
    await disposeCalls?.().catch(() => {});
    await replyHandle.dispose().catch(() => {});
    await domain.close();
    throw error;
  }
}

// ---------------------------------------------------------------------------
// Public binding entry (registers host effects + session event listener)
// ---------------------------------------------------------------------------

export interface HostBindingResult {
  /**
   * Resolves once the first activation attempt has settled (either running or
   * parked for retry). NEVER rejects: a missing native daemon must not take the
   * host down, so failures are reported through {@link getState} instead.
   */
  readonly ready: Promise<void>;
  /** Diagnostics snapshot (safe at any time). */
  getState(): {
    running: boolean;
    serverId?: string;
    /** Last activation error, present while parked/retrying. */
    error?: string;
    /** Number of completed activation attempts (diagnostics). */
    attempts: number;
  };
}

/** Retry schedule for a daemon that is not up yet (or went away). */
const RETRY_INITIAL_MS = 1_000;
const RETRY_MAX_MS = 30_000;

/**
 * Own the plugin lifetime on the calling fiber.
 *
 * Fail-soft contract: a missing/unreachable native daemon, an unresolved
 * credential or any other activation error only parks the channel and retries
 * with bounded backoff. It never rejects `ready`, never throws out of the
 * effect, and therefore never prevents the host from starting.
 */
export function bindWechatHost(
  ctx: HostContextLike,
  config: WechatConfigView,
  options: {
    readonly createNativeClient?: NativeClientFactory;
    /** Override the first retry delay (tests). */
    readonly retryInitialMs?: number;
  } = {},
): HostBindingResult {
  const log = asLog(ctx);
  let state: {
    running: boolean;
    serverId?: string;
    error?: string;
    attempts: number;
  } = { running: false, attempts: 0 };

  let settled = false;
  let resolveReady!: () => void;
  const ready = new Promise<void>((resolve) => {
    resolveReady = resolve;
  });
  const settle = (): void => {
    if (settled) return;
    settled = true;
    resolveReady();
  };

  const retryInitial = options.retryInitialMs ?? RETRY_INITIAL_MS;

  ctx.effect(() => {
    const life = new AbortController();
    let cleanup: (() => Promise<void>) | undefined;

    // Settings-page connection status: registered before the activation loop
    // so the page can watch the channel through park/retry progress. Reads the
    // unwrapped config snapshot (endpoint/account only — no secrets). The host
    // web server is provided by another plugin, so bind inside a dependency-
    // gated `ctx.inject(['webServer'])` scope (workbuddy's pattern); a plain
    // `ctx.webServer` property can be undefined there.
    const statusView = unwrapVolatileConfig(config) as WechatConfigView;
    let disposeStatusRoute: (() => void) | undefined;
    if (typeof ctx.inject === 'function') {
      ctx.inject(['webServer'], (scoped) => {
        if (scoped.webServer !== undefined && disposeStatusRoute === undefined) {
          disposeStatusRoute = bindStatusRoute(scoped.webServer, () => state, statusView);
        }
      });
    } else if (ctx.webServer !== undefined) {
      disposeStatusRoute = bindStatusRoute(ctx.webServer, () => state, statusView);
    }

    const sleep = (ms: number): Promise<void> =>
      new Promise<void>((resolve) => {
        if (life.signal.aborted) return resolve();
        const timer = setTimeout(() => {
          life.signal.removeEventListener('abort', onAbort);
          resolve();
        }, ms);
        const onAbort = (): void => {
          clearTimeout(timer);
          resolve();
        };
        life.signal.addEventListener('abort', onAbort, { once: true });
      });

    /** Background activation loop: assemble → start, or park and retry. */
    const activate = async (): Promise<void> => {
      let delay = retryInitial;
      while (!life.signal.aborted) {
        state = { ...state, attempts: state.attempts + 1 };
        debugTrace('activate', `attempt ${state.attempts} begin`);
        try {
          const binding = await assembleWechatBinding({
            ctx,
            config,
            log,
            ...(options.createNativeClient === undefined
              ? {}
              : { createNativeClient: options.createNativeClient }),
          });

          // Turn-completion listener: drain committed replies into the outbox.
          const offEvent = ctx.on(
            'session/event',
            (session: unknown, event: ReplyEventView & { data?: any }) => {
              if (event?.type !== 'turn/end') return;
              if (event.data?.reason?.kind !== 'completed') return;
              const id = (session as { id?: string } | undefined)?.id;
              if (id === undefined) return;
              void binding.runtime.drainReplies(id).catch((error: unknown) => {
                log('error', `wechat: reply drain failed: ${errorText(error)}`);
              });
            },
            { global: true },
          );

          // Start inbound consumers for the configured account.
          const accountId = asAccountId(
            (unwrapVolatileConfig(config.native) as WechatConfigView['native'] | undefined)?.accountId ??
              DEFAULT_ACCOUNT,
          );
          try {
            await binding.runtime.start([accountId]);
            binding.coordinator?.start();
          } catch (error) {
            // Roll this attempt back completely before parking: registered
            // tools must not linger while the channel retries.
            binding.disposeTools();
            await binding.disposeCalls?.().catch(() => {});
            await binding.runtime.stop().catch(() => {});
            await binding.replyHandle.dispose().catch(() => {});
            await binding.domain.close().catch(() => {});
            throw error;
          }
          state = {
            running: binding.runtime.running,
            serverId: binding.serverId,
            attempts: state.attempts,
          };
          debugTrace(
            'activate',
            `activated serverId=${binding.serverId} running=${String(binding.runtime.running)}`,
          );
          cleanup = async () => {
            offEvent();
            binding.disposeTools();
            await binding.disposeCalls?.().catch(() => {});
            await binding.runtime.stop();
            await binding.replyHandle.dispose().catch(() => {});
            await binding.domain.close().catch(() => {});
            state = { running: false, serverId: binding.serverId, attempts: state.attempts };
          };
          settle();
          return;
        } catch (error) {
          const message = errorText(error);
          state = { running: false, error: message, attempts: state.attempts };
          // Fail-soft: the host keeps running; this channel simply stays parked.
          log('warn', `wechat: channel parked (${message}); retrying in ${delay}ms`);
          debugTrace('activate', `parked: ${message}; retry in ${delay}ms`);
          settle();
          await sleep(delay);
          delay = Math.min(delay * 2, RETRY_MAX_MS);
        }
      }
    };

    void activate();

    // Return the disposer immediately so Cordis can always unload the effect.
    return async () => {
      life.abort();
      if (cleanup !== undefined) await cleanup();
      if (disposeStatusRoute !== undefined) {
        try {
          disposeStatusRoute();
        } catch {
          // Route already gone; disposal must not mask the original cleanup.
        }
      }
    };
  }, 'agent-wechat.host');

  return {
    ready,
    getState: () => ({ ...state }),
  };
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
