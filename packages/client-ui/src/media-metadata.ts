import {
  NativeApiError,
  type MediaAsset,
  type NativeWeChatClient,
} from "@agent-wechat/shared";

type Lookup = {
  id: string;
  signal: AbortSignal;
  resolve: (value: { items: MediaAsset[] }) => void;
  reject: (error: unknown) => void;
};

/** Coalesce visible chat and viewer metadata reads within the same render turn. */
export class MediaMetadata {
  private queued = new Map<string, Lookup[]>();
  private timer?: ReturnType<typeof setTimeout>;
  private controller = new AbortController();
  private client: NativeWeChatClient;
  constructor(client: NativeWeChatClient) {
    this.client = client;
  }
  lookup = (
    account: string,
    id: string,
    signal: AbortSignal,
  ): Promise<{ items: MediaAsset[] }> => {
    if (signal.aborted || this.controller.signal.aborted)
      return Promise.reject(
        new DOMException("Media lookup cancelled", "AbortError"),
      );
    if (typeof this.client.mediaAssetsBatch !== "function")
      return this.client.mediaAssets(account, id);
    return new Promise((resolve, reject) => {
      const rows = this.queued.get(account) ?? [];
      rows.push({ id, signal, resolve, reject });
      this.queued.set(account, rows);
      this.timer ??= setTimeout(() => {
        this.timer = undefined;
        void this.flush();
      }, 0);
    });
  };
  private async flush(): Promise<void> {
    const queued = this.queued;
    this.queued = new Map();
    for (const [account, rows] of queued) {
      const active = rows.filter((row) => {
        if (!row.signal.aborted && !this.controller.signal.aborted) return true;
        row.reject(new DOMException("Media lookup cancelled", "AbortError"));
        return false;
      });
      const ids = [...new Set(active.map((row) => row.id))];
      for (let offset = 0; offset < ids.length; offset += 40) {
        const selected = ids.slice(offset, offset + 40);
        const consumers = active.filter((row) => selected.includes(row.id));
        try {
          const result = await this.client.mediaAssetsBatch(
            account,
            selected,
            this.controller.signal,
          );
          for (const row of consumers) {
            const item = result.items.find((item) => item.messageId === row.id);
            if (row.signal.aborted)
              row.reject(
                new DOMException("Media lookup cancelled", "AbortError"),
              );
            else if (!item)
              row.reject(new Error("Missing media lookup result"));
            else if (item.error)
              row.reject(new NativeApiError(409, item.error));
            else row.resolve({ items: item.assets });
          }
        } catch (error) {
          for (const row of consumers) row.reject(error);
        }
      }
    }
  }
  close(): void {
    clearTimeout(this.timer);
    this.controller.abort();
    for (const rows of this.queued.values())
      for (const row of rows)
        row.reject(new DOMException("Media lookup cancelled", "AbortError"));
    this.queued.clear();
  }
}
