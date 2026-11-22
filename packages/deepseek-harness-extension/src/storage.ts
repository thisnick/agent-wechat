/**
 * Persistent storage declaration for the WeChat plugin (DESIGN.md §3.2).
 *
 * The host exposes a zod-schema-validated KV domain via
 * `ctx.storageDomain.open(spec)`. This module owns:
 *  - the zod record schemas (reused by adapters and tests),
 *  - the `DomainSpec` handed to `ctx.storageDomain.open`,
 *  - a tiny synchronous wrapper that narrows `domain.table(name)` to typed
 *    `KvTable` handles.
 *
 * Storage deliberately uses plain `string` ids, NOT the branded ids from
 * `contracts.ts`, so this layer stays decoupled from adapters (they cast).
 * Every object schema uses `.passthrough()` so a newer writer may add fields
 * without breaking older readers.
 *
 * NOTE on the host API (confirmed from `@deepseek-ai/dsh-storage-domain`
 * `lib/types/spec.d.ts`): a table declaration is `{ valueSchema, __key? }`
 * — NOT `{ schema }` — while the global singleton is `{ schema, initial }`.
 * `defineDomain`/`domainTable` are identity/validation helpers; we build the
 * spec with them so misconfiguration fails at module load rather than at open.
 */

import { z } from 'zod';
import { defineDomain, domainTable } from '@deepseek-ai/dsh-storage-domain';
import type { DomainSpec } from '@deepseek-ai/dsh-storage-domain';

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/** Plugin/domain identity used for diagnostics and config routes. */
export const WECHAT_DOMAIN_NAME = 'agent-wechat';
/**
 * Host-safe backend unit name. The host UNIT_NAME_RE is `/^[a-z][a-z0-9_]*$/`
 * — hyphens are rejected by both `defineDomain()` and the storage backend's
 * `open()` — so the spec's `name` uses an underscore while the plugin identity
 * above stays `agent-wechat` (lead decision, 2026-11-23).
 */
export const WECHAT_STORAGE_UNIT_NAME = 'agent_wechat';
export const WECHAT_DOMAIN_VERSION = 1;

// ---------------------------------------------------------------------------
// Reusable field schemas
// ---------------------------------------------------------------------------

/** ISO-8601 timestamp strings (plain strings — adapters own the format). */
const timestamp = z.string();
/** Ownership generation of an account, bumped on stop/start (DESIGN.md §10). */
const generation = z.number().int().nonnegative();
/** CAS token for route resume/dispose races. */
const revision = z.number().int().nonnegative();

// ---------------------------------------------------------------------------
// Record schemas
// ---------------------------------------------------------------------------

/** accounts — accountId → account lifecycle state. */
export const AccountRecordSchema = z
  .object({
    serverId: z.string(),
    status: z.string(),
    lastSeenAt: timestamp,
    generation,
  })
  .passthrough();
export type AccountRecord = z.infer<typeof AccountRecordSchema>;

/** threads — routeKey → durable per-thread routing binding. */
export const ThreadRecordSchema = z
  .object({
    routeKey: z.string(),
    sessionId: z.string(),
    accountId: z.string(),
    chatJid: z.string(),
    policyFingerprint: z.string(),
    revision,
    lastActivityAt: timestamp,
  })
  .passthrough();
export type ThreadRecord = z.infer<typeof ThreadRecordSchema>;

/** journal — cursor-key → processed inbound event (dedupe / restart cursor). */
export const JournalRecordSchema = z
  .object({
    accountId: z.string(),
    cursor: z.string(),
    eventKind: z.string(),
    createdAt: timestamp,
  })
  .passthrough();
export type JournalRecord = z.infer<typeof JournalRecordSchema>;

/** custody — runId → prepared-run custody (native dispatch proof). */
export const CustodyRecordSchema = z
  .object({
    runId: z.string(),
    accountId: z.string(),
    chatJid: z.string(),
    messageIds: z.array(z.string()),
    status: z.string(),
    createdAt: timestamp,
  })
  .passthrough();
export type CustodyRecord = z.infer<typeof CustodyRecordSchema>;

/** outbox — stable idempotency key → durable outbound send record. */
export const OutboxRecordSchema = z
  .object({
    key: z.string(),
    accountId: z.string(),
    chatJid: z.string(),
    kind: z.string(),
    payload: z.string(),
    status: z.string(),
    serverId: z.string().optional(),
    serverMessageId: z.string().optional(),
    reason: z.string().optional(),
    createdAt: timestamp,
    updatedAt: timestamp,
  })
  .passthrough();
export type OutboxRecord = z.infer<typeof OutboxRecordSchema>;

/** recalls — serverMessageId → recall tombstone. */
export const RecallRecordSchema = z
  .object({
    serverMessageId: z.string(),
    accountId: z.string(),
    chatJid: z.string(),
    recalledAt: timestamp,
  })
  .passthrough();
export type RecallRecord = z.infer<typeof RecallRecordSchema>;

/** config_generation — singleton config-generation counter (global slot). */
export const ConfigGenerationRecordSchema = z
  .object({
    generation,
  })
  .passthrough();
export type ConfigGenerationRecord = z.infer<typeof ConfigGenerationRecordSchema>;

// ---------------------------------------------------------------------------
// Domain declaration
// ---------------------------------------------------------------------------

/** The exact spec object passed to `ctx.storageDomain.open`. */
export function wechatDomainSpec(): DomainSpec {
  return defineDomain({
    name: WECHAT_STORAGE_UNIT_NAME,
    version: WECHAT_DOMAIN_VERSION,
    global: {
      schema: ConfigGenerationRecordSchema,
      initial: { generation: 0 },
    },
    tables: {
      accounts: domainTable<string, AccountRecord>(AccountRecordSchema),
      threads: domainTable<string, ThreadRecord>(ThreadRecordSchema),
      journal: domainTable<string, JournalRecord>(JournalRecordSchema),
      custody: domainTable<string, CustodyRecord>(CustodyRecordSchema),
      outbox: domainTable<string, OutboxRecord>(OutboxRecordSchema),
      recalls: domainTable<string, RecallRecord>(RecallRecordSchema),
      config_generation: domainTable<string, ConfigGenerationRecord>(
        ConfigGenerationRecordSchema,
      ),
    },
  });
}

// ---------------------------------------------------------------------------
// Typed table wrapper
// ---------------------------------------------------------------------------

/**
 * Minimal shape of `domain.table(name)`. Kept structural (identical to the
 * host `KvTable` signature) so tests can supply a hand-written fake without
 * importing host runtime types or performing casts.
 */
export interface TableView<K extends string, V> {
  get(key: K): V | undefined;
  entries(): IterableIterator<[K, V]>;
  keys(): IterableIterator<K>;
  readonly size: number;
  put(key: K, value: V): Promise<void>;
  delete(key: K): Promise<boolean>;
  deleteMany(keys: readonly K[]): Promise<number>;
  update(key: K, fn: (current: V) => V): Promise<V>;
}

/** Minimal shape of an open domain required by `openWechatTables`. */
export interface DomainView {
  table(name: string): TableView<string, unknown>;
}

/** Narrowed, typed accessors for every declared WeChat table. */
export interface WechatTables {
  accounts: TableView<string, AccountRecord>;
  threads: TableView<string, ThreadRecord>;
  journal: TableView<string, JournalRecord>;
  custody: TableView<string, CustodyRecord>;
  outbox: TableView<string, OutboxRecord>;
  recalls: TableView<string, RecallRecord>;
  configGeneration: TableView<string, ConfigGenerationRecord>;
}

/**
 * Return typed table handles from an already-open domain. Pure/synchronous —
 * no I/O; the caller owns the domain's lifecycle (close via ctx.effect).
 */
export function openWechatTables(domain: DomainView): WechatTables {
  return {
    accounts: domain.table('accounts') as TableView<string, AccountRecord>,
    threads: domain.table('threads') as TableView<string, ThreadRecord>,
    journal: domain.table('journal') as TableView<string, JournalRecord>,
    custody: domain.table('custody') as TableView<string, CustodyRecord>,
    outbox: domain.table('outbox') as TableView<string, OutboxRecord>,
    recalls: domain.table('recalls') as TableView<string, RecallRecord>,
    configGeneration: domain.table('config_generation') as TableView<
      string,
      ConfigGenerationRecord
    >,
  };
}

// ---------------------------------------------------------------------------
// Calls memory domain (M5) — separate unit so call transcripts/summaries can
// be wiped without touching message-route state.
// ---------------------------------------------------------------------------

export const CALLS_STORAGE_UNIT_NAME = 'agent_wechat_calls';
export const CALLS_DOMAIN_VERSION = 1;

/** CallSummary persisted shape (mirrors src/calls/memory.ts CallSummary). */
export const CallSummaryRecordSchema = z.object({
  endedAt: z.number(),
  seconds: z.number(),
  direction: z.enum(['incoming', 'outgoing']),
  summary: z.string(),
});
export type CallSummaryRecord = z.infer<typeof CallSummaryRecordSchema>;

/** Full summaries table: memoryKey → recent summaries for that contact. */
export const CallSummariesSchema = z.record(z.string(), z.array(CallSummaryRecordSchema));

/**
 * Domain spec for durable call memory. The whole table lives in the domain
 * global singleton (one JSON value) — summaries are few (≤500 contacts × 3)
 * and always rewritten wholesale by the summary store.
 */
export function wechatCallsDomainSpec(): DomainSpec {
  return defineDomain({
    name: CALLS_STORAGE_UNIT_NAME,
    version: CALLS_DOMAIN_VERSION,
    global: {
      schema: CallSummariesSchema,
      initial: {},
    },
    tables: {},
  });
}
