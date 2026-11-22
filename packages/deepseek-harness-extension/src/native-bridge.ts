/**
 * M1 native bridge — committed-live-only inbound consumption.
 *
 * Ports the semantics of OpenClaw `native-monitor.ts` without its prepared-
 * boundary hooks: an event is admitted only if it is a live incoming
 * `message.committed`, within the 24h/100 catch-up window, and its identity
 * matches the stored message. Everything else only advances the cursor.
 *
 * The bridge is ports-driven and side-effect-free apart from the two injected
 * stores, so it is testable with fakes and never touches a live bot.
 */
import {
  asAccountId,
  asChatJid,
  asSenderId,
  asServerId,
  wechatNativeMessageId,
  type AccountId,
  type ChatJid,
  type InboundMessage,
  type MessageKind,
  type SenderId,
} from './contracts.ts';
import type { NativeClientPort, NativeEventView } from './ports.ts';
import type { EventPage } from './ports.ts';
import { NativeApiError } from '@agent-wechat/shared';
import { debugTrace } from './debug-trace.ts';

export const CATCH_UP_WINDOW_MS = 24 * 60 * 60 * 1000;
export const CATCH_UP_LIMIT = 100;
export const BACKOFF_INITIAL_MS = 250;
export const BACKOFF_MAX_MS = 30_000;

/** Minimal cursor custody the bridge owns; adapters map this to the journal table. */
export interface CursorStore {
  get(accountId: AccountId): Promise<string>;
  set(accountId: AccountId, cursor: string): Promise<void>;
}

export interface BridgeDeps {
  readonly client: NativeClientPort;
  readonly cursors: CursorStore;
  readonly serverId: string;
  readonly consumerId: string;
  /** Admission callback — the bridge does not import the admission module directly. */
  readonly answerable: (message: InboundMessage) => boolean | Promise<boolean>;
  readonly now?: () => number;
  readonly log?: (level: 'info' | 'warn' | 'error', message: string) => void;
  /** Held-for-review events: cursor retained, never auto-retried. */
  readonly onNeedsReview?: (reason: string, event: NativeEventView) => void;
}

export interface BridgeCallbacks {
  /** Called for each admitted inbound message, in cursor order. */
  onMessage(message: InboundMessage): Promise<void> | void;
  /** Called when a parse/identity error is held for review (never auto-retried). */
  onNeedsReview?(reason: string, event: NativeEventView): void;
}

const withinWindow = (time: string | null | undefined, now: number): boolean => {
  const at = time ? Date.parse(time) : Number.NaN;
  return !Number.isFinite(at) || at >= now - CATCH_UP_WINDOW_MS;
};

const isGroupChat = (chatJid: ChatJid): boolean => chatJid.endsWith('@chatroom');

/** Map WeChat numeric message type + native fields to our MessageKind. */
const messageKind = (type: number, appMessageType?: number): MessageKind => {
  if (type === 1) return 'text';
  if (type === 3 || type === 43) return 'image';
  if (type === 34) return 'voice';
  if (type === 47 || appMessageType === 8) return 'sticker';
  if (type === 49 || appMessageType === 5) return 'share';
  if (type === 10000) return 'system';
  return 'file';
};

/** Extract a deterministic inbound record from a stored native message. */
export const toInboundMessage = (
  serverId: string,
  message: Record<string, unknown>,
): InboundMessage => {
  const id = typeof message.id === 'string' ? message.id : '';
  const accountId = asAccountId(typeof message.accountId === 'string' ? message.accountId : '');
  const threadId = typeof message.threadId === 'string' ? message.threadId : '';
  const msgServerId = typeof message.serverId === 'string' ? message.serverId : '';
  const senderId = typeof message.senderId === 'string' ? asSenderId(message.senderId) : undefined;
  const type = typeof message.type === 'number' ? message.type : 1;
  const appMessageType = typeof message.appMessageType === 'number' ? message.appMessageType : undefined;
  const mentions = message.mentions as { self?: boolean | null; all?: boolean | null } | undefined;
  const mentioned = mentions?.self === true || mentions?.all === true;
  if (!id || !accountId || !threadId) throw new Error('Native stored message identity is incomplete');
  // Per-message uniqueness: the Agent inbox rejects two pending followups
  // with the same id, so the dispatch id must vary per message, not per chat.
  const dispatchId = wechatNativeMessageId(
    accountId,
    asServerId(serverId),
    asChatJid(threadId),
    msgServerId || id,
  );
  return {
    id: dispatchId,
    ref: {
      accountId,
      chatJid: asChatJid(threadId),
      serverId: asServerId(serverId),
      ...(senderId ? { senderId } : {}),
    },
    serverMessageId: msgServerId,
    kind: messageKind(type, appMessageType),
    text: typeof message.body === 'string' ? message.body : '',
    media: [],
    mentioned,
    createdAt: typeof message.createdAt === 'string' ? message.createdAt : '',
  };
};

export interface AcceptResult {
  readonly message?: InboundMessage;
  readonly advancedCursor: boolean;
}

/**
 * Take in one event. Returns a message only for an answerable live incoming
 * committed message; all other events advance the cursor and return nothing.
 * On identity mismatch / missing id, the cursor is retained and a needs_review
 * is reported (never auto-retried).
 */
export async function acceptEvent(
  deps: BridgeDeps,
  accountId: AccountId,
  cursor: string,
  event: NativeEventView,
): Promise<AcceptResult> {
  if (BigInt(event.cursor) <= BigInt(cursor)) return { advancedCursor: false };
  const now = deps.now?.() ?? Date.now();

  if (event.kind === 'message.recalled') {
    return { advancedCursor: true }; // recall custody handled by the router/delivery layer
  }
  if (event.kind !== 'message.committed') return { advancedCursor: true };

  const data = event.data as { messageId?: unknown; threadId?: unknown; origin?: unknown; direction?: unknown };
  if (typeof data.messageId !== 'string') {
    deps.onNeedsReview?.('committed-message-id-missing', event);
    return { advancedCursor: false };
  }
  const origin = typeof data.origin === 'string' ? data.origin : 'unknown';
  if (origin !== 'live' || (data.direction != null && data.direction !== 'incoming')) {
    return { advancedCursor: true };
  }
  if (!withinWindow(event.createdAt, now)) return { advancedCursor: true };

  const stored = await deps.client.message(accountId, data.messageId);
  if (
    stored.id !== data.messageId ||
    stored.accountId !== accountId ||
    (typeof data.threadId === 'string' && stored.threadId !== data.threadId)
  ) {
    deps.onNeedsReview?.('committed-identity-mismatch', event);
    return { advancedCursor: false };
  }
  if (stored.direction !== 'incoming') return { advancedCursor: true };

  const message = toInboundMessage(deps.serverId, stored);
  if (!withinWindow(message.createdAt, now)) return { advancedCursor: true };
  if (!(await deps.answerable(message))) return { advancedCursor: true };
  return { message, advancedCursor: true };
}

export interface RunOptions {
  readonly accountId: AccountId;
  readonly signal: AbortSignal;
  readonly callbacks: BridgeCallbacks;
}

/**
 * NativeConsumer: catch-up pages then a live SSE watch with bounded backoff.
 * One loop per account; `run` resolves when the signal aborts or the stream
 * ends terminally (caller re-launches with backoff on resolve).
 */
export class NativeConsumer {
  private readonly deps: BridgeDeps;
  constructor(deps: BridgeDeps) {
    this.deps = deps;
  }

  /** One catch-up pass: page from the stored cursor up to CATCH_UP_LIMIT live messages. */
  async catchUp(accountId: AccountId, callbacks: BridgeCallbacks): Promise<void> {
    let cursor = await this.deps.cursors.get(accountId);
    debugTrace('wechat', `[catchup] stored cursor=${cursor} account=${accountId}`);
    if (cursor === '0') {
      // Resume at the first event of the 24h window, never before the stored cursor.
      const firstRecent = await this.firstRecentCursor(accountId);
      debugTrace('wechat', `[catchup] firstRecent=${firstRecent ?? 'undefined'}`);
      if (firstRecent && BigInt(firstRecent) > BigInt(cursor)) cursor = firstRecent;
    }
    let processed = 0;
    let rewound = false;
    for (let page = 0; page < 100 && processed < CATCH_UP_LIMIT; page++) {
      let result: EventPage;
      try {
        result = await this.deps.client.events(accountId, cursor, 200);
      } catch (error) {
        if (error instanceof NativeApiError && (error.code === 'event_cursor_expired' || error.status === 410)) {
          // The stored cursor fell behind the journal retention floor: rescan
          // from the floor within this same attempt instead of crash-looping.
          await this.deps.cursors.set(accountId, '0');
          const firstRecent = await this.firstRecentCursor(accountId);
          if (firstRecent === undefined) throw error;
          cursor = firstRecent;
          continue;
        }
        throw error;
      }
      if (result.items.length === 0) {
        if (processed === 0 && cursor !== '0' && !rewound) {
          // The daemon can rebuild or compact its journal with lower sequence
          // numbers; a stored cursor past the tail answers empty pages forever
          // instead of 410. Verify the journal tail and rewind when it has
          // moved below the stored cursor.
          const tail = await this.journalTail(accountId, cursor);
          rewound = true;
          if (tail !== undefined && BigInt(tail) < BigInt(cursor)) {
            debugTrace('wechat', `[catchup] stored cursor ${cursor} ahead of journal tail ${tail}; rewinding`);
            cursor = tail;
            await this.deps.cursors.set(accountId, cursor);
            continue;
          }
        }
        break;
      }
      const before = cursor;
      for (const event of result.items) {
        const res = await acceptEvent(this.deps, accountId, cursor, event);
        if (res.advancedCursor) {
          cursor = event.cursor;
          processed++;
        }
        if (res.message) await callbacks.onMessage(res.message);
        if (processed >= CATCH_UP_LIMIT) break;
      }
      if (cursor === before) {
        // A retained cursor means a needs_review event; stop rather than spin.
        break;
      }
      await this.deps.cursors.set(accountId, cursor);
      await this.deps.client.ack(accountId, this.deps.consumerId, cursor);
      if (result.items.length < 200) break;
    }
    await this.deps.cursors.set(accountId, cursor);
  }

  /**
   * One journal page probe that recognises the daemon's retention floor.
   * Cursors below the journal retention window answer 410
   * (event_cursor_expired); `'expired'` marks that case and `undefined` marks
   * an unrelated failure the caller must back off from.
   */
  private async probeJournal(accountId: AccountId, after: string): Promise<EventPage | 'expired' | undefined> {
    try {
      return await this.deps.client.events(accountId, after, 1);
    } catch (error) {
      if (error instanceof NativeApiError && (error.code === 'event_cursor_expired' || error.status === 410)) {
        debugTrace('wechat', `[probe] expired after=${after}`);
        return 'expired';
      }
      debugTrace('wechat', `[probe] error after=${after}: ${error instanceof Error ? error.message : String(error)}`);
      return undefined;
    }
  }

  /**
   * Binary-search the journal's current tail cursor. The predicate "events
   * exist after c" is monotone: below the retention floor probes answer 410
   * (treated like empty), between floor and tail they answer events, past the
   * tail they answer empty. Returns the highest event cursor seen, or
   * undefined when the journal holds no events.
   */
  private async journalTail(accountId: AccountId, stored: string): Promise<string | undefined> {
    const upper = BigInt(stored);
    // Phase A: escape the retention floor with doubling probes (clamped to the
    // stored cursor) until the journal answers with something other than 410.
    let lo = 0n;
    let cursor = 1n;
    let step = 1n;
    let page = await this.probeJournal(accountId, cursor.toString());
    for (let i = 0; i < 64 && page === 'expired'; i++) {
      lo = cursor;
      cursor = cursor + step;
      step *= 2n;
      if (cursor > upper) cursor = upper;
      if (cursor === lo) break;
      page = await this.probeJournal(accountId, cursor.toString());
    }
    if (page === undefined || page === 'expired') return undefined;
    // Phase B: three-way monotone walk — below the floor probes stay expired,
    // between floor and tail they answer events, past the tail they answer
    // empty. Converge on the highest retained event cursor.
    let best: string | undefined;
    const seed = page.items[page.items.length - 1];
    if (seed !== undefined) {
      best = seed.cursor;
      lo = BigInt(seed.cursor);
    }
    let hi = cursor > upper ? upper : cursor;
    if (BigInt(stored) > hi) hi = upper;
    for (let i = 0; i < 48 && lo + 1n < hi; i++) {
      const mid = (lo + hi) / 2n;
      const probe = await this.probeJournal(accountId, mid.toString());
      if (probe === undefined) return best;
      if (probe === 'expired') {
        lo = mid;
        continue;
      }
      const last = probe.items[probe.items.length - 1];
      if (last === undefined) {
        hi = mid;
        continue;
      }
      best = last.cursor;
      lo = BigInt(last.cursor);
    }
    return best;
  }

  /**
   * Scan for the first event within the catch-up window (doubling probes plus
   * a binary walk-back). A fresh consumer cannot always start at cursor 0:
   * the daemon expires cursors below its retention floor, so expired probes
   * jump forward exponentially until the journal answers again.
   */
  private async firstRecentCursor(accountId: AccountId): Promise<string | undefined> {
    const now = this.deps.now?.() ?? Date.now();
    // Newest position known to hold only stale (or expired) events…
    let stale: string | undefined;
    // …and a position whose first event is inside the window.
    let fresh: string | undefined;
    let cursor = '0';
    let step = 1n;
    for (let i = 0; i < 64 && fresh === undefined; i++) {
      const page = await this.probeJournal(accountId, cursor);
      if (page === undefined) return undefined;
      if (page === 'expired') {
        stale = cursor;
        cursor = (BigInt(cursor) + step).toString();
        step *= 2n;
        continue;
      }
      const first = page.items[0];
      if (!first) return cursor === '0' ? undefined : cursor;
      if (withinWindow(first.createdAt, now)) {
        fresh = cursor;
        break;
      }
      stale = first.cursor;
      cursor = (BigInt(first.cursor) + step).toString();
      step *= 2n;
    }
    if (fresh === undefined) return undefined;
    // Binary walk-back to the first retained recent event; expired probes are
    // simply older than the retention floor and count as stale.
    let lo = stale;
    let hi = BigInt(fresh);
    while (lo !== undefined && BigInt(lo) + 1n < hi) {
      const mid = (BigInt(lo) + hi) / 2n;
      const probe = await this.probeJournal(accountId, mid.toString());
      if (probe === undefined) return undefined;
      const item = probe === 'expired' ? undefined : probe.items[0];
      if (item && withinWindow(item.createdAt, now)) hi = BigInt(item.cursor);
      else lo = mid.toString();
    }
    return hi.toString();
  }

  /** Run catch-up + live watch until aborted. Caller applies backoff on resolve. */
  async run(options: RunOptions): Promise<void> {
    const { accountId, signal, callbacks } = options;
    await this.catchUp(accountId, callbacks);
    if (signal.aborted) return;

    let cursor = await this.deps.cursors.get(accountId);
    for await (const event of this.deps.client.watch(accountId, cursor, signal)) {
      if (signal.aborted) break;
      const res = await acceptEvent(this.deps, accountId, cursor, event);
      if (res.advancedCursor) cursor = event.cursor;
      if (res.message) await callbacks.onMessage(res.message);
      await this.deps.cursors.set(accountId, cursor);
      await this.deps.client.ack(accountId, this.deps.consumerId, cursor);
    }
  }
}

/** Backoff schedule helper: 250ms doubling to 30s. */
export class Backoff {
  #current = BACKOFF_INITIAL_MS;
  next(): number {
    const value = this.#current;
    this.#current = Math.min(this.#current * 2, BACKOFF_MAX_MS);
    return value;
  }
  reset(): void {
    this.#current = BACKOFF_INITIAL_MS;
  }
}

export { isGroupChat };
