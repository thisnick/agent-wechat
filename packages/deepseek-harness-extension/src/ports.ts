/**
 * The 8 ports defined in DESIGN.md §2 layering.
 *
 * Ports are the seams between the plugin and the outside world (native daemon,
 * DSH host, speech providers). Every port is an interface so M1–M5 can be
 * implemented behind fakes and tested without a live bot, per AGENTS.md:24.
 *
 * A `HostRuntime` bundle (also here) is the explicit dependency holder that
 * replaces OpenClaw's global PluginRuntime holder — enabling multi-account and
 * test isolation.
 */

import type {
  AccountId,
  AdmissionDecision,
  AdmittedPart,
  ChatJid,
  FollowupMessage,
  InboundMessage,
  OutboundRecord,
  PreparedRun,
  RouteBinding,
  RouteKey,
  SaveFileInput,
  SaveImageInput,
  ServerId,
  SessionId,
  StoredImageData,
  ToolPrincipal,
  TranscribeInput,
  Transcript,
} from './contracts.js';

// ---------------------------------------------------------------------------
// Port 1: NativeClient — native daemon HTTP/SSE bridge
// ---------------------------------------------------------------------------

/** Minimal native identity shape (maps to shared NativeStatus/NativeIdentity). */
export interface NativeIdentityView {
  readonly product: string;
  readonly backend: string;
  readonly version: string;
  readonly apiVersion: number;
  readonly capabilities: readonly string[];
}

export interface NativeStatusView {
  readonly serverId: string;
  readonly pid: number;
  readonly ownershipGeneration: string;
  readonly version: NativeIdentityView;
}

/** A committed event from the native event log (shared NativeEvent subset). */
export interface NativeEventView {
  readonly cursor: string;
  readonly kind: string;
  readonly data: Record<string, unknown>;
  readonly createdAt: string;
}

export interface EventPage {
  readonly items: readonly NativeEventView[];
  readonly nextCursor: string;
}

/**
 * Read side of the native daemon. The adapter wraps `NativeWeChatClient`
 * from `@agent-wechat/shared`; methods here are only the subset the bridge uses.
 */
export interface NativeClientPort {
  status(): Promise<NativeStatusView>;
  accounts(): Promise<readonly AccountId[]>;
  /** Fetch one event page after `cursor` (pagination for catch-up). */
  events(accountId: AccountId, after?: string, limit?: number): Promise<EventPage>;
  /** Live SSE stream. `onActivity` feeds the idle watchdog. */
  watch(
    accountId: AccountId,
    after: string,
    signal: AbortSignal,
    onActivity?: () => void,
  ): AsyncGenerator<NativeEventView>;
  /** Acknowledge a consumer cursor so the daemon can trim its log. */
  ack(accountId: AccountId, consumerId: string, cursor: string): Promise<void>;
  /** Read one stored message by id (used for canonical readback). */
  message(accountId: AccountId, messageId: string): Promise<Record<string, unknown>>;
  /** List messages with an optional "after" bound for history catch-up. */
  messages(
    accountId: AccountId,
    chatJid: ChatJid,
    opts: { limit?: number; before?: string; after?: string },
    signal?: AbortSignal,
  ): Promise<{ items: readonly Record<string, unknown>[]; nextCursor?: string }>;
}

// ---------------------------------------------------------------------------
// Port 2: SecretResolver — DSH credentials (re-resolve per operation)
// ---------------------------------------------------------------------------

/** A `credentialRef(...)` string from `@deepseek-ai/dsh-credentials`. */
export type CredentialRef = string;

export interface ResolvedSecret {
  readonly value: string;
  readonly source: string;
}

export interface SecretResolverPort {
  /** Resolve on every operation; never cache so rotation takes effect at once. */
  resolve(ref: CredentialRef): Promise<ResolvedSecret | undefined>;
  /** Enforce the native token shape (32–1024 printable ASCII) without leaking it. */
  resolveNativeToken(ref: CredentialRef): Promise<string>;
}

// ---------------------------------------------------------------------------
// Port 3: TrustedToolContext — host-derived identity, never from model args
// ---------------------------------------------------------------------------

export interface TrustedToolContextPort {
  /**
   * Resolve the principal for the currently executing tool call. The adapter
   * MUST derive this from `exec.agent.id` via the route registry; it must not
   * accept any account/sender fields from model-supplied arguments.
   */
  principalForAgent(agentId: string): Promise<ToolPrincipal | undefined>;
  /** Re-check a grant and the config generation before every mutation. */
  revalidate(
    principal: ToolPrincipal,
    toolName: string,
    currentGeneration: number,
  ): Promise<AdmissionDecision>;
  /**
   * Wrap a mutation so it revalidates at execution time (async gap safe),
   * throwing UnavailableError / authorization errors if it no longer holds.
   */
  withGrant<T>(
    agentId: string,
    toolName: string,
    run: (principal: ToolPrincipal) => Promise<T>,
  ): Promise<T>;
}

// ---------------------------------------------------------------------------
// Port 4: MediaStore — DSH attachments + native download
// ---------------------------------------------------------------------------

export interface MediaStorePort {
  saveImage(input: SaveImageInput): Promise<{ ref: string }>;
  readImage(ref: string, signal?: AbortSignal): Promise<StoredImageData>;
  saveFile(input: SaveFileInput): Promise<{ ref: string }>;
  /** Download from native daemon into DSH attachment storage; return refs. */
  ingestNativeMedia(
    accountId: AccountId,
    assetId: string,
    signal?: AbortSignal,
  ): Promise<{ ref: string; mimeType: string; bytes: number }>;
  /** Prepare DSH prompt parts (text/image/file) for model consumption. */
  admitPromptParts(parts: readonly AdmittedPart[]): Promise<readonly AdmittedPart[]>;
  /** Remove transient media on stop, keeping durable attachments. */
  disposeTransient(): Promise<void>;
}

// ---------------------------------------------------------------------------
// Port 5: Transcriber — voice/audio transcription (fail-closed)
// ---------------------------------------------------------------------------

export interface TranscriberPort {
  readonly available: boolean;
  transcribe(input: TranscribeInput): Promise<Transcript>;
}

// ---------------------------------------------------------------------------
// Port 6: SessionRouter — per-thread Agent lifecycle
// ---------------------------------------------------------------------------

export interface SessionRouterPort {
  /** Look up an existing binding by stable route key. */
  binding(routeKey: RouteKey): Promise<RouteBinding | undefined>;
  /** Create a new durable Agent session for a route, or resume if evicted. */
  ensure(
    routeKey: RouteKey,
    opts: {
      cwd?: string;
      agentOptions?: Record<string, unknown>;
      setup?: (ctx: unknown, agent: unknown) => void | Promise<void>;
    },
  ): Promise<RouteBinding>;
  /** Inject a message into the thread's Agent as a followup (wakes the turn). */
  followup(routeKey: RouteKey, message: FollowupMessage): Promise<void>;
  /** Map a host Agent id back to its route binding (tool identity). */
  bindingForAgent(agentId: string): Promise<RouteBinding | undefined>;
  /** Evict an idle Agent: dispose handle, keep the durable session log. */
  evict(routeKey: RouteKey): Promise<void>;
  /** Bump the revision (used by revalidation and resume-race guards). */
  refresh(routeKey: RouteKey): Promise<RouteBinding>;
}

// ---------------------------------------------------------------------------
// Port 7: OutboundJournal — durable, idempotent send pipeline
// ---------------------------------------------------------------------------

export interface OutboundJournalPort {
  /** Enqueue or replay a send under the same idempotency key. */
  enqueue(record: Omit<OutboundRecord, 'status' | 'createdAt' | 'updatedAt'>): Promise<OutboundRecord>;
  get(key: string): Promise<OutboundRecord | undefined>;
  /** Mark as sent only after canonical readback matches. */
  verify(
    key: string,
    readback: { serverId: ServerId; serverMessageId: string; text: string },
  ): Promise<OutboundRecord>;
  markNeedsReview(key: string, reason: string): Promise<OutboundRecord>;
  /** Pending/sent records for restart recovery; MUST NOT auto-resend. */
  pending(): Promise<readonly OutboundRecord[]>;
}

// ---------------------------------------------------------------------------
// Port 8: PreparedRunSink — native dispatch proof / claimed handoff
// ---------------------------------------------------------------------------

export interface PreparedRunSinkPort {
  prepare(run: Omit<PreparedRun, 'status' | 'createdAt'>): Promise<PreparedRun>;
  /** Claim on `agent/inbox/claimed`; returns undefined if already consumed. */
  claim(runId: string, agentId: string): Promise<PreparedRun | undefined>;
  consume(runId: string): Promise<void>;
  release(runId: string): Promise<void>;
  /** Runs prepared but not yet claimed at startup (handoff ambiguity → review). */
  unresolved(accountId: AccountId): Promise<readonly PreparedRun[]>;
}

// ---------------------------------------------------------------------------
// Bundle runtime — explicit dependency holder (no global singleton)
// ---------------------------------------------------------------------------

export interface HostRuntime {
  readonly native: NativeClientPort;
  readonly secrets: SecretResolverPort;
  readonly toolContext: TrustedToolContextPort;
  readonly media: MediaStorePort;
  readonly transcriber: TranscriberPort;
  readonly router: SessionRouterPort;
  readonly outbound: OutboundJournalPort;
  readonly prepared: PreparedRunSinkPort;
  /** Monotonic config generation; consumers snapshot it and revalidate. */
  generation(): number;
}
