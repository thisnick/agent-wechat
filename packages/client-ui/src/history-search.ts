import {
  isNativeTombstone,
  type NativeMessage,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import { readableError } from "./session.ts";

type SearchState = {
  query: string;
  items: NativeMessage[];
  nextCursor: string | null;
  loading: boolean;
  error: string | null;
};

/** One conversation owns each search; late queries cannot replace newer results. */
export class HistorySearch {
  private value: SearchState = {
    query: "",
    items: [],
    nextCursor: null,
    loading: false,
    error: null,
  };
  private generation = 0;
  private closed = false;
  private listeners = new Set<() => void>();
  private client: NativeWeChatClient;
  private account: string;
  private thread: string;
  constructor(client: NativeWeChatClient, account: string, thread: string) {
    this.client = client;
    this.account = account;
    this.thread = thread;
  }
  snapshot = (): SearchState => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };
  private publish(patch: Partial<SearchState>): void {
    if (this.closed) return;
    this.value = { ...this.value, ...patch };
    for (const listener of this.listeners) listener();
  }
  close(): void {
    this.closed = true;
    this.generation++;
    this.listeners.clear();
  }
  async search(query: string): Promise<void> {
    if (this.closed) return;
    const generation = ++this.generation;
    query = query.trim();
    this.publish({
      query,
      items: [],
      nextCursor: null,
      error: null,
      loading: !!query,
    });
    if (query) await this.read(generation);
  }
  async more(): Promise<void> {
    if (this.closed || this.value.loading || !this.value.nextCursor) return;
    this.publish({ loading: true, error: null });
    await this.read(this.generation, this.value.nextCursor);
  }
  private async read(generation: number, cursor?: string): Promise<void> {
    try {
      const page = await this.client.messages(this.account, this.thread, {
        query: this.value.query,
        limit: 50,
        ...(cursor ? { cursor } : {}),
      });
      if (this.closed || generation !== this.generation) return;
      const items = new Map(this.value.items.map((item) => [item.id, item]));
      for (const item of page.items) {
        if (isNativeTombstone(item)) items.delete(item.id);
        else items.set(item.id, item);
      }
      this.publish({
        items: [...items.values()],
        nextCursor: page.nextCursor,
        loading: false,
      });
    } catch (error) {
      if (!this.closed && generation === this.generation)
        this.publish({ error: readableError(error), loading: false });
    }
  }
}
