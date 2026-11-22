/**
 * M2 orchestration layer — DESIGN.md §3–§5.
 *
 * This module is the coordination seam that stitches the already-pure modules
 * together:
 *   - native bridge (committed-live-only inbound, cursor custody)
 *   - admission (DM / group / mention policy, before any media work)
 *   - session router (per-thread Agent create/resume/followup)
 *   - reply collector (committed assistant text -> delivery plans)
 *   - outbox (durable, idempotent send with canonical readback)
 *
 * It stays host-agnostic at runtime: every external effect is injected, so the
 * whole round-trip can be exercised with in-memory fakes (AGENTS.md:24 — never
 * send to a live bot in tests). The host binding lives in the entry module.
 *
 * Import note: `.ts` specifiers everywhere (strip-types test runner, no emit).
 */
import {
  asAccountId,
  asChatJid,
  makeRouteKey,
  type AccountId,
  type ChatJid,
  type FollowupMessage,
  type InboundMessage,
  type OutboundRecord,
  type RouteBinding,
  type RouteKey,
} from './contracts.ts';
import { admitInbound, type AdmissionConfig } from './admission.ts';
import { isGroupChat, NativeConsumer, Backoff, type BridgeCallbacks } from './native-bridge.ts';
import {
  planDeliveries,
  type DeliveryPlan,
  type ReplyState,
} from './reply-collector.ts';
import type { NativeClientPort } from './ports.ts';
import type { SessionRouter } from './session-router.ts';
import type { Outbox } from './outbox.ts';
import type { JournalRecord, TableView } from './storage.ts';
import { debugTrace } from './debug-trace.ts';

// ---------------------------------------------------------------------------
// Public interfaces
// ---------------------------------------------------------------------------

export type RuntimeLogLevel = 'info' | 'warn' | 'error';

/** Where the committed reply state for a session comes from (bound to the host projection). */
export interface ReplyStateSource {
  stateOf(sessionId: string): ReplyState | undefined;
}

export interface RuntimeDeps {
  readonly native: NativeClientPort;
  readonly router: SessionRouter;
  readonly outbox: Outbox;
  readonly outboxTable: TableView<string, unknown>;
  readonly journal: TableView<string, JournalRecord>;
  readonly admission: AdmissionConfig;
  readonly replySource: ReplyStateSource;
  /** Optional session-id -> route binding reverse lookup (host projection). */
  readonly sessionRoutes?: (sessionId: string) => RouteBinding | undefined | Promise<RouteBinding | undefined>;
  /** Known route keys, used as a fallback when no sessionRoutes hook is supplied. */
  readonly knownRoutes?: readonly string[];
  /** Stable native identity + consumer identity for cursor ack. */
  readonly serverId: string;
  readonly consumerId: string;
  /** Convert an admitted inbound message + binding into a host followup message. */
  readonly toFollowup: (message: InboundMessage, binding: RouteBinding) => FollowupMessage;
  readonly log?: (level: RuntimeLogLevel, message: string) => void;
  readonly now?: () => number;
  /** Monotonic config generation, snapshotted at dispatch time. */
  readonly generation?: () => number;
  /** Idle eviction threshold; 0 disables the idle timer. */
  readonly idleEvictMs?: number;
}

export interface DispatchResult {
  readonly admitted: boolean;
  readonly reason: string;
  readonly routeKey?: RouteKey;
  readonly binding?: RouteBinding;
}

export interface WechatRuntime {
  /** Start all account consumers + outbox recovery. Idempotent. */
  start(accountIds: readonly AccountId[]): Promise<void>;
  /** Abort all loops and wait for quiescence. Idempotent. */
  stop(): Promise<void>;
  /** Admit + route + followup a single inbound message. */
  handleInbound(message: InboundMessage): Promise<DispatchResult>;
  /** Drain committed replies for a session into the outbox and send them. */
  drainReplies(sessionId: string): Promise<readonly OutboundRecord[]>;
  /** Recover pending outbox rows at startup (lookup only, never resend). */
  recoverOutbox(): Promise<readonly OutboundRecord[]>;
  readonly running: boolean;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const CURSOR_PREFIX = 'cursor:';
const cursorKey = (accountId: AccountId): string => `${CURSOR_PREFIX}${accountId}`;

/** Journal-backed cursor custody for the bridge (one cursor per account). */
export function createJournalCursorStore(
  journal: TableView<string, JournalRecord>,
): {
  get(accountId: AccountId): Promise<string>;
  set(accountId: AccountId, cursor: string): Promise<void>;
} {
  return {
    async get(accountId) {
      return journal.get(cursorKey(accountId))?.cursor ?? '0';
    },
    async set(accountId, cursor) {
      // The record MUST satisfy JournalRecordSchema: the storage domain
      // validates every stored row against it when the domain is (re)opened,
      // so a partial record parks the channel permanently on next start.
      await journal.put(cursorKey(accountId), {
        accountId,
        cursor,
        eventKind: 'cursor',
        createdAt: new Date().toISOString(),
      });
    },
  };
}

/** Derive admission input from a committed inbound message. */
function admissionInput(message: InboundMessage) {
  return {
    accountId: message.ref.accountId,
    chatJid: message.ref.chatJid,
    ...(message.ref.senderId === undefined ? {} : { senderId: message.ref.senderId }),
    isGroup: isGroupChat(message.ref.chatJid),
    mentioned: message.mentioned,
  };
}

/** All outbox keys already used, for restart-safe reply de-duplication. */
function enqueuedKeys(outboxTable: TableView<string, unknown>): Set<string> {
  const keys = new Set<string>();
  for (const [key] of outboxTable.entries()) keys.add(key);
  return keys;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createWechatRuntime(deps: RuntimeDeps): WechatRuntime {
  const log = deps.log ?? (() => {});
  const now = deps.now ?? Date.now;
  const generation = deps.generation ?? (() => 0);

  const cursors = createJournalCursorStore(deps.journal);
  const controller = new AbortController();
  const loops = new Set<Promise<void>>();
  let running = false;

  /** Admission + durable routing + followup injection. */
  async function handleInbound(message: InboundMessage): Promise<DispatchResult> {
    const decision = admitInbound(deps.admission, admissionInput(message));
    if (!decision.admitted) {
      log('info', `wechat: inbound rejected (${decision.reason}) id=${message.id}`);
      return { admitted: false, reason: decision.reason };
    }

    const routeKey = makeRouteKey(message.ref.accountId, message.ref.chatJid);
    const binding = await deps.router.ensure(routeKey, {
      policyFingerprint: decision.policyFingerprint,
    });
    // A followup wakes the Agent turn; never use `inject` for wakeups (DESIGN.md §3.4).
    await deps.router.followup(routeKey, deps.toFollowup(message, binding));
    log('info', `wechat: dispatched id=${message.id} route=${routeKey} gen=${generation()}`);
    return { admitted: true, reason: 'admitted', routeKey, binding };
  }

  /** Turn committed replies from the projection into durable, idempotent sends. */
  async function drainReplies(sessionId: string): Promise<readonly OutboundRecord[]> {
    const state = deps.replySource.stateOf(sessionId);
    if (state === undefined) return [];
    const binding = await findBindingBySession(sessionId);
    if (binding === undefined) {
      log('warn', `wechat: no route binding for session=${sessionId}; replies not sent`);
      return [];
    }
    const already = enqueuedKeys(deps.outboxTable);
    const plans = planDeliveries(sessionId, state, already);
    const results: OutboundRecord[] = [];
    for (const plan of plans) {
      const record = await deps.outbox.enqueue({
        key: plan.key,
        accountId: binding.accountId,
        chatJid: binding.chatJid,
        kind: 'text',
        payload: plan.text,
      });
      const delivered = await deps.outbox.deliver(record.key);
      if (delivered !== undefined) results.push(delivered);
    }
    return results;
  }

  /** Reverse-lookup the route binding for a session (via live router state). */
  async function findBindingBySession(sessionId: string): Promise<RouteBinding | undefined> {
    // The router exposes bindingForAgent (agent id), not session id. The host
    // binding supplies the session->route mapping through `sessionRoutes` when
    // available; otherwise fall back to scanning known route bindings.
    const fromHook = await deps.sessionRoutes?.(sessionId);
    if (fromHook !== undefined) return fromHook;
    for (const routeKey of deps.knownRoutes ?? []) {
      const binding = await deps.router.binding(routeKey as RouteKey);
      if (binding?.sessionId === sessionId) return binding;
    }
    return undefined;
  }

  async function recoverOutbox(): Promise<readonly OutboundRecord[]> {
    const recovered = await deps.outbox.recover();
    if (recovered.length > 0) {
      log('info', `wechat: recovered ${recovered.length} pending outbound row(s)`);
    }
    return recovered;
  }

  async function runAccount(accountId: AccountId, signal: AbortSignal): Promise<void> {
    const backoff = new Backoff();
    const answerable = (message: InboundMessage): boolean =>
      admitInbound(deps.admission, admissionInput(message)).admitted;
    const callbacks: BridgeCallbacks = {
      onMessage: async (message) => {
        await handleInbound(message);
      },
      onNeedsReview: (reason) => log('warn', `wechat: held for review (${reason})`),
    };
    const consumer = new NativeConsumer({
      client: deps.native,
      cursors,
      serverId: deps.serverId,
      consumerId: deps.consumerId,
      answerable,
      now,
      log: (level, msg) => log(level, msg),
      onNeedsReview: (reason) => log('warn', `wechat: bridge needs_review (${reason})`),
    });

    while (!signal.aborted) {
      try {
        debugTrace('wechat', `[consumer] attempt begin account=${accountId}`);
        await consumer.run({ accountId, signal, callbacks });
        debugTrace('wechat', `[consumer] watch ended account=${accountId}; restarting`);
        backoff.reset();
        if (signal.aborted) break;
        await sleep(backoff.next(), signal);
      } catch (error) {
        if (signal.aborted) break;
        debugTrace('wechat', `[consumer] failed account=${accountId}: ${errorText(error)}`);
        log('error', `wechat: consumer ${accountId} failed: ${errorText(error)}`);
        await sleep(backoff.next(), signal);
      }
    }
    debugTrace('wechat', `[consumer] loop exit account=${accountId}`);
  }

  async function start(accountIds: readonly AccountId[]): Promise<void> {
    if (running) return;
    running = true;
    debugTrace('wechat', `[runtime] start accounts=${accountIds.join(',')}`);
    void recoverOutbox();
    for (const accountId of accountIds) {
      const task = runAccount(accountId, controller.signal).finally(() => loops.delete(task));
      loops.add(task);
    }
  }

  async function stop(): Promise<void> {
    if (!running) return;
    running = false;
    controller.abort();
    await Promise.allSettled([...loops]);
    loops.clear();
  }

  return {
    start,
    stop,
    handleInbound,
    drainReplies,
    recoverOutbox,
    get running() {
      return running;
    },
  };
}

// ---------------------------------------------------------------------------
// Small local utilities (kept here to avoid a utility module for two helpers)
// ---------------------------------------------------------------------------

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise<void>((resolve) => {
    if (signal.aborted) return resolve();
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = (): void => {
      clearTimeout(timer);
      resolve();
    };
    signal.addEventListener('abort', onAbort, { once: true });
  });
}
