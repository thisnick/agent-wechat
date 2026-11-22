/**
 * Durable, idempotent outbound delivery — DESIGN.md §5.3.
 *
 * Port of OpenClaw's `native-send.ts:7-27,45-91` semantics:
 *  - the outbox row is the durable guard: a key is POSTed at most once;
 *  - an uncertain send is reconciled by the SAME idempotency key via
 *    `sendByKey`, never by a second POST;
 *  - a message counts as `sent` only after a canonical readback matches
 *    (text === payload, non-empty ids, and any previously stored ids agree);
 *  - ambiguity is held as `needs_review` and is never auto-retried
 *    (retryable:false).
 *
 * This module is pure logic plus an injected transport. It deliberately uses
 * only `import type` from `storage.ts` so that `zod` and
 * `@deepseek-ai/dsh-storage-domain` are never loaded at runtime.
 */
import {
  asAccountId,
  asChatJid,
  asServerId,
  type AccountId,
  type ChatJid,
  type OutboundKind,
  type OutboundRecord,
  type OutboundStatus,
  type ServerId,
} from './contracts.ts';
import type { OutboundJournalPort } from './ports.ts';
import type { OutboxRecord, WechatTables } from './storage.ts';

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/** Result of a POST that may have succeeded but whose outcome is uncertain. */
export interface SendResult {
  readonly serverId: string;
  readonly serverMessageId: string;
}

/** Canonical readback used both by `verify` and by `sendByKey`. */
export interface ReadbackResult extends SendResult {
  readonly text: string;
}

export interface OutboxDeps {
  readonly tables: Pick<WechatTables, 'outbox'>;
  /** Actual send. Called AT MOST ONCE per key (enforced below). */
  readonly sendText: (
    accountId: AccountId,
    chatJid: ChatJid,
    text: string,
    key: string,
  ) => Promise<SendResult>;
  /** Idempotent lookup of an already-posted message by the original key. */
  readonly sendByKey?: (accountId: AccountId, key: string) => Promise<ReadbackResult | undefined>;
  readonly now?: () => number;
  readonly readbackTimeoutMs?: number;
  readonly sleep?: (ms: number) => Promise<void>;
}

/** The seven port methods plus the two delivery helpers. */
export interface Outbox extends OutboundJournalPort {
  /** Send once, then confirm; on uncertainty reconcile by key (never resend). */
  deliver(key: string): Promise<OutboundRecord | undefined>;
  /** Startup sweep of pending rows; lookup-only, never posts. */
  recover(): Promise<readonly OutboundRecord[]>;
}

export const DEFAULT_READBACK_TIMEOUT_MS = 65_000;
export const REASON_READBACK_MISMATCH = 'canonical-readback-mismatch';
export const REASON_SEND_UNCONFIRMED = 'send-unconfirmed';

/** Poll cadence while reconciling an uncertain send. */
const POLL_INTERVAL_MS = 250;
/** Safety bound so a frozen `now()` plus a no-op `sleep()` cannot spin forever. */
const MAX_POLLS = 1000;

// ---------------------------------------------------------------------------
// Conversion / comparison helpers
// ---------------------------------------------------------------------------

/** Table rows use plain strings; the port surface uses branded contracts. */
function toOutbound(rec: OutboxRecord): OutboundRecord {
  return {
    key: rec.key,
    accountId: asAccountId(rec.accountId),
    chatJid: asChatJid(rec.chatJid),
    kind: rec.kind as OutboundKind,
    payload: rec.payload,
    status: rec.status as OutboundStatus,
    ...(rec.serverId !== undefined ? { serverId: asServerId(rec.serverId) } : {}),
    ...(rec.serverMessageId !== undefined ? { serverMessageId: rec.serverMessageId } : {}),
    ...(rec.reason !== undefined ? { reason: rec.reason } : {}),
    createdAt: rec.createdAt,
    updatedAt: rec.updatedAt,
  };
}

function stamp(ms: number): string {
  return new Date(ms).toISOString();
}

/**
 * Canonical readback check. `text` must equal the queued payload byte-for-byte;
 * ids must be present, and any ids already stored on the row must agree. A row
 * with no stored ids is a first verification — that is where the new ids are
 * recorded by `verify`.
 */
function readbackMatches(rec: OutboxRecord, readback: ReadbackResult): boolean {
  if (readback.text !== rec.payload) return false;
  if (readback.serverId.length === 0 || readback.serverMessageId.length === 0) return false;
  if (rec.serverId !== undefined && rec.serverId !== readback.serverId) return false;
  if (rec.serverMessageId !== undefined && rec.serverMessageId !== readback.serverMessageId) {
    return false;
  }
  return true;
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

export function createOutbox(deps: OutboxDeps): Outbox {
  const table = deps.tables.outbox;
  const clock = deps.now ?? Date.now;
  const timeoutMs = deps.readbackTimeoutMs ?? DEFAULT_READBACK_TIMEOUT_MS;
  const nap =
    deps.sleep ??
    ((ms: number): Promise<void> => new Promise<void>((resolve) => setTimeout(resolve, ms)));

  /**
   * Keys for which `sendText` has been invoked in THIS process. The durable row
   * alone cannot distinguish "never tried" from "tried and outcome lost"; this
   * in-memory bit closes the concurrent/duplicate-deliver window without a
   * second POST.
   */
  const attempted = new Set<string>();

  function timestamp(): string {
    return stamp(clock());
  }

  async function write(
    key: string,
    mutate: (current: OutboxRecord, at: string) => OutboxRecord,
  ): Promise<OutboundRecord> {
    const next = await table.update(key, (current) => mutate(current, timestamp()));
    return toOutbound(next);
  }

  async function requireRow(key: string): Promise<OutboxRecord> {
    const current = table.get(key);
    if (current === undefined) throw new Error(`Outbox record not found: ${key}`);
    return current;
  }

  async function holdForReview(key: string, reason: string): Promise<OutboundRecord> {
    await requireRow(key);
    return write(key, (current, at) => ({
      ...current,
      status: 'needs_review',
      reason,
      updatedAt: at,
    }));
  }

  async function verifyInternal(key: string, readback: ReadbackResult): Promise<OutboundRecord> {
    const current = await requireRow(key);
    // `sent` is final; `needs_review` is terminal (retryable:false) and must
    // not be flipped to sent by a late readback.
    if (current.status === 'sent' || current.status === 'needs_review') {
      return toOutbound(current);
    }
    if (!readbackMatches(current, readback)) {
      return holdForReview(key, REASON_READBACK_MISMATCH);
    }
    return write(key, (rec, at) => ({
      ...rec,
      status: 'sent',
      serverId: readback.serverId,
      serverMessageId: readback.serverMessageId,
      updatedAt: at,
    }));
  }

  /** Poll `sendByKey` until a canonical match or the deadline expires. */
  async function confirmByKey(rec: OutboxRecord): Promise<OutboundRecord> {
    const lookup = deps.sendByKey;
    if (lookup === undefined) return holdForReview(rec.key, REASON_SEND_UNCONFIRMED);

    const accountId = asAccountId(rec.accountId);
    const deadline = clock() + timeoutMs;
    for (let i = 0; i < MAX_POLLS; i++) {
      const found = await lookup(accountId, rec.key);
      if (found !== undefined && readbackMatches(rec, found)) {
        return verifyInternal(rec.key, found);
      }
      const remaining = deadline - clock();
      if (remaining <= 0) break;
      await nap(Math.min(POLL_INTERVAL_MS, remaining));
    }
    return holdForReview(rec.key, REASON_SEND_UNCONFIRMED);
  }

  const outbox: Outbox = {
    enqueue: async (record) => {
      const existing = table.get(record.key);
      if (existing !== undefined) {
        // Idempotent replay: same key means the same logical send. Never
        // overwrite the target/payload or create a second row.
        return toOutbound(existing);
      }
      const at = timestamp();
      const stored: OutboxRecord = {
        key: record.key,
        accountId: record.accountId,
        chatJid: record.chatJid,
        kind: record.kind,
        payload: record.payload,
        status: 'pending',
        createdAt: at,
        updatedAt: at,
      };
      await table.put(record.key, stored);
      return toOutbound(stored);
    },

    get: async (key) => {
      const rec = table.get(key);
      return rec === undefined ? undefined : toOutbound(rec);
    },

    verify: async (key, readback) => verifyInternal(key, readback),

    markNeedsReview: async (key, reason) => holdForReview(key, reason),

    pending: async () => {
      const rows: OutboundRecord[] = [];
      for (const [, rec] of table.entries()) {
        if (rec.status === 'pending') rows.push(toOutbound(rec));
      }
      return rows;
    },

    deliver: async (key) => {
      const current = table.get(key);
      if (current === undefined) return undefined;
      if (current.status === 'sent' || current.status === 'needs_review') {
        return toOutbound(current);
      }
      if (attempted.has(key)) {
        // A POST for this key already happened in this process. Reconcile by
        // key instead of posting a second time.
        return confirmByKey(current);
      }
      attempted.add(key);
      try {
        const result = await deps.sendText(
          asAccountId(current.accountId),
          asChatJid(current.chatJid),
          current.payload,
          key,
        );
        // Verify with the ids returned by the POST and the SAME payload —
        // never with a caller-supplied rendering of the message.
        return await verifyInternal(key, {
          serverId: asServerId(result.serverId),
          serverMessageId: result.serverMessageId,
          text: current.payload,
        });
      } catch {
        // The POST outcome is unknown. Do NOT resend: look it up by key.
        return confirmByKey(current);
      }
    },

    recover: async () => {
      const results: OutboundRecord[] = [];
      for (const [, rec] of table.entries()) {
        if (rec.status !== 'pending') continue;

        /*
         * Startup has no in-memory memory of whether `sendText` already
         * posted this key (the process may have died between POST and
         * verification). Re-POSTing would duplicate the message, so recovery
         * is lookup-only: ask the daemon for the message under the ORIGINAL
         * idempotency key. If it exists and reads back canonically we verify;
         * otherwise we hold `send-unconfirmed` for a human. `recover` never
         * calls `sendText` and never marks the key as attempted.
         */
        const lookup = deps.sendByKey;
        const found =
          lookup === undefined
            ? undefined
            : await lookup(asAccountId(rec.accountId), rec.key);
        if (found !== undefined && readbackMatches(rec, found)) {
          results.push(await verifyInternal(rec.key, found));
        } else {
          results.push(await holdForReview(rec.key, REASON_SEND_UNCONFIRMED));
        }
      }
      return results;
    },
  };

  return outbox;
}
