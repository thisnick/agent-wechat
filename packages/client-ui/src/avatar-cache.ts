import { NativeApiError, type NativeWeChatClient } from "@agent-wechat/shared";
import { pause } from "./session.ts";

export class AvatarCache {
  private client: NativeWeChatClient;
  private values = new Map<string, string>();
  private pending = new Map<string, Promise<void>>();
  private missing = new Set<string>();
  private requested = new Set<string>();
  private refreshing = new Map<string, Promise<void>>();
  private queue: (() => void)[] = [];
  private active = 0;
  private reading = 0;
  private reads: (() => void)[] = [];
  private generations = new Map<string, number>();
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  constructor(client: NativeWeChatClient) {
    this.client = client;
  }
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  get = (account: string, username: string): string =>
    this.values.get(`${account}:${username}`) ?? "";
  async load(account: string, username: string, force = false): Promise<void> {
    const key = `${account}:${username}`;
    if (
      !username ||
      this.controller.signal.aborted ||
      (!force && this.pending.has(key))
    )
      return this.pending.get(key);
    const generation = (this.generations.get(key) ?? 0) + 1;
    this.generations.set(key, generation);
    const request = (async () => {
      try {
        const content = await this.read(() =>
          this.client.avatar(account, username, this.controller.signal),
        );
        if (
          this.controller.signal.aborted ||
          this.generations.get(key) !== generation
        )
          return;
        const url = URL.createObjectURL(
          new Blob([new Uint8Array(content.bytes).buffer], {
            type: content.contentType,
          }),
        );
        const old = this.values.get(key);
        if (old) URL.revokeObjectURL(old);
        this.values.set(key, url);
        this.missing.delete(key);
        for (const listener of this.listeners) listener();
      } catch (error) {
        if (
          error instanceof NativeApiError &&
          error.code === "media_not_cached" &&
          this.generations.get(key) === generation &&
          !this.controller.signal.aborted
        )
          this.missing.add(key);
      }
    })();
    this.pending.set(key, request);
    await request;
  }
  async ensure(account: string, username: string): Promise<void> {
    await this.load(account, username);
    const key = `${account}:${username}`;
    if (
      !this.missing.has(key) ||
      this.requested.has(key) ||
      this.controller.signal.aborted
    )
      return;
    this.requested.add(key);
    // Lazy requests are best effort. An unavailable avatar keeps its initials.
    await this.refresh(account, username).catch(() => {});
  }
  async refresh(account: string, username: string): Promise<void> {
    const key = `${account}:${username}`;
    const pending = this.refreshing.get(key);
    if (pending) return pending;
    const request = new Promise<void>((resolve, reject) => {
      this.queue.push(() => {
        if (this.controller.signal.aborted) {
          resolve();
          return;
        }
        this.active++;
        void this.download(account, username)
          .then(resolve, reject)
          .finally(() => {
            this.active--;
            this.drain();
          });
      });
    });
    this.refreshing.set(key, request);
    this.drain();
    try {
      await request;
    } finally {
      this.refreshing.delete(key);
    }
  }
  private drain(): void {
    while (this.active < 4 && this.queue.length) this.queue.shift()!();
  }
  private async read<T>(fetch: () => Promise<T>): Promise<T> {
    await new Promise<void>((resolve) => {
      const start = () => {
        this.reading++;
        resolve();
      };
      if (this.reading < 2) start();
      else this.reads.push(start);
    });
    try {
      if (this.controller.signal.aborted)
        throw new Error("Avatar cache closed");
      return await fetch();
    } finally {
      this.reading--;
      this.reads.shift()?.();
    }
  }
  private async download(account: string, username: string): Promise<void> {
    const operation = await this.client.refreshAvatar(account, username);
    const deadline = Date.now() + 125000;
    while (!this.controller.signal.aborted && Date.now() < deadline) {
      const current = await this.client.operation(
        account,
        operation.id,
        this.controller.signal,
      );
      if (current.state === "succeeded") {
        await this.load(account, username, true);
        return;
      }
      if (
        ["failed", "cancelled", "expired", "rejected"].includes(current.state)
      )
        throw new Error("Avatar refresh could not finish.");
      await pause(700, this.controller.signal);
    }
    if (!this.controller.signal.aborted)
      throw new Error("Avatar refresh is taking longer than expected.");
  }
  close(): void {
    this.controller.abort();
    this.drain();
    for (const url of this.values.values()) URL.revokeObjectURL(url);
    this.values.clear();
    this.listeners.clear();
  }
}
