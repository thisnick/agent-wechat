import type { NativeGroup, NativeMessage } from "@agent-wechat/shared";

export const HISTORY_TTL_MS = 5 * 60 * 1000;

export type CachedHistory = {
  messages: NativeMessage[];
  nextMessages: string | null;
  sourceGeneration: string;
  group: NativeGroup | null;
  validatedAt?: number;
};

/** Bounded working set; persistence is owned by the connection cache. */
export class HistoryCache {
  private entries = new Map<
    string,
    { account: string; value: CachedHistory }
  >();
  private maxChats: number;
  private maxMessages: number;
  constructor(maxChats = 20, maxMessages = 5000) {
    this.maxChats = maxChats;
    this.maxMessages = maxMessages;
  }
  get(account: string, thread: string): CachedHistory | undefined {
    const key = JSON.stringify([account, thread]);
    const entry = this.entries.get(key);
    if (entry) {
      this.entries.delete(key);
      this.entries.set(key, entry);
    }
    return entry?.value;
  }
  set(account: string, thread: string, value: CachedHistory): void {
    const key = JSON.stringify([account, thread]);
    this.entries.delete(key);
    // Do not truncate pages: their continuation cursor would then skip messages.
    if (value.messages.length > this.maxMessages) return;
    this.entries.set(key, { account, value });
    while (
      this.entries.size > this.maxChats ||
      [...this.entries.values()].reduce(
        (sum, entry) => sum + entry.value.messages.length,
        0,
      ) > this.maxMessages
    )
      this.entries.delete(this.entries.keys().next().value!);
  }
  export(account: string): [string, CachedHistory][] {
    return [...this.entries].flatMap(([key, entry]) =>
      entry.account === account
        ? [[JSON.parse(key)[1], entry.value] as [string, CachedHistory]]
        : [],
    );
  }
  clear(account?: string): void {
    for (const [key, entry] of this.entries)
      if (!account || entry.account === account) this.entries.delete(key);
  }
  delete(account: string, thread: string): void {
    this.entries.delete(JSON.stringify([account, thread]));
  }
}
