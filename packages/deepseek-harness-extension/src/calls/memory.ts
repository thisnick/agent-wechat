/**
 * Call memory (DESIGN.md §6.3): the last few call summaries per contact, the
 * next call's voice model may receive as recent context. This is plugin-side
 * call memory, not WeChat state. Summaries persist in the harness storage
 * domain through a {@link CallMemoryTable} — never as plaintext audio (the
 * plugin records no audio at all).
 *
 * Summary keys are policy-scoped: read-only callers never read an owner
 * call's summaries. Untagged legacy entries have unknown authority and stay
 * unread by policy-scoped calls.
 */
import type { CallToolPolicy } from './config.ts';

export type CallSummary = { endedAt: number; seconds: number; direction: 'incoming' | 'outgoing'; summary: string };

const SUMMARIES_PER_CONTACT = 3;
const MAX_CONTACTS = 500;
const MAX_SUMMARY_CHARS = 1200;
import { clipText } from './transcript.ts';

/** Minimal KV view over the storage domain (one JSON value per key). */
export interface CallMemoryTable {
  get(key: string): Promise<Record<string, CallSummary[]> | undefined>;
  put(key: string, value: Record<string, CallSummary[]>): Promise<void>;
}

/** In-memory adapter for tests and non-durable deployments. */
export function memoryCallMemoryTable(): CallMemoryTable {
  const map = new Map<string, Record<string, CallSummary[]>>();
  return {
    async get(key) {
      const value = map.get(key);
      return value ? structuredClone(value) : undefined;
    },
    async put(key, value) {
      map.set(key, structuredClone(value));
    },
  };
}

export const memoryKey = (accountId: string, contactId: string, policy?: CallToolPolicy): string =>
  policy ? JSON.stringify([accountId, contactId, policy]) : `${accountId}/${contactId}`;

export class CallSummaryStore {
  private writing: Promise<void> = Promise.resolve();

    private readonly table: CallMemoryTable;

  constructor(table: CallMemoryTable) {
    this.table = table;
  }

  async recent(accountId: string, contactId: string, policy?: CallToolPolicy): Promise<CallSummary[]> {
    const all = (await this.table.get('summaries')) ?? {};
    return all[memoryKey(accountId, contactId, policy)] ?? [];
  }

  record(accountId: string, contactId: string, summary: CallSummary, policy?: CallToolPolicy): Promise<void> {
    // One writer at a time: each write starts from the previous one's result.
    const next = this.writing.then(async () => {
      const all = (await this.table.get('summaries')) ?? {};
      const key = memoryKey(accountId, contactId, policy);
      all[key] = [
        { ...summary, summary: clipText(summary.summary, MAX_SUMMARY_CHARS) },
        ...(all[key] ?? []),
      ].slice(0, SUMMARIES_PER_CONTACT);
      const kept = Object.entries(all)
        .sort(([, a], [, b]) => (b[0]?.endedAt ?? 0) - (a[0]?.endedAt ?? 0))
        .slice(0, MAX_CONTACTS);
      await this.table.put('summaries', Object.fromEntries(kept));
    });
    this.writing = next.catch(() => {});
    return next;
  }
}

/** Recap prompt fragment handed to the voice model for the next call. */
export function formatSummaryRecap(summaries: CallSummary[]): string {
  if (summaries.length === 0) return '';
  const lines = summaries.map(item => `- (${item.direction}, ${Math.round(item.seconds)}s) ${item.summary}`);
  return `Recent calls with this contact:\n${lines.join('\n')}`;
}
