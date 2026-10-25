import { NativeApiError, type NativeWeChatClient } from "@agent-wechat/shared";
import type { BrowserCache } from "./browser-cache.ts";
import { pause } from "./session.ts";

export class AvatarCache {
  private client: NativeWeChatClient;
  private persistent?: BrowserCache;
  private values = new Map<string, string>();
  private pending = new Map<string, Promise<void>>();
  private missing = new Set<string>();
  private checkedAt = new Map<string, number>();
  private refreshedAt = new Map<string, number>();
  private visible = new Map<string, number>();
  private now: () => number;
  private refreshAllowed: () => boolean;
  private timer: ReturnType<typeof setInterval>;
  private refreshing = new Map<string, Promise<void>>();
  private queue: (() => void)[] = [];
  private active = 0;
  private reading = 0;
  private reads: (() => void)[] = [];
  private generations = new Map<string, number>();
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  constructor(
    client: NativeWeChatClient,
    now = Date.now,
    refreshAllowed = () => true,
    persistent?: BrowserCache,
  ) {
    this.client = client;
    this.persistent = persistent;
    this.now = now;
    this.refreshAllowed = refreshAllowed;
    this.timer = setInterval(() => {
      for (const key of this.visible.keys()) {
        const [account, username] = JSON.parse(key) as [string, string];
        void this.ensure(account, username, true);
      }
    }, 60000);
  }
  private key(account: string, username: string): string {
    return JSON.stringify([account, username]);
  }
  show(account: string, username: string): () => void {
    const key = this.key(account, username);
    this.visible.set(key, (this.visible.get(key) ?? 0) + 1);
    void this.ensure(account, username, true);
    return () => {
      const count = (this.visible.get(key) ?? 1) - 1;
      if (count) this.visible.set(key, count);
      else this.visible.delete(key);
    };
  }
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  get = (account: string, username: string): string =>
    this.values.get(this.key(account, username)) ?? "";
  async load(
    account: string,
    username: string,
    force = false,
    visibleOnly = false,
  ): Promise<void> {
    const key = this.key(account, username);
    if (
      !username ||
      this.controller.signal.aborted ||
      (!force && this.pending.has(key)) ||
      (!force && this.now() - (this.checkedAt.get(key) ?? -Infinity) < 3600000)
    )
      return this.pending.get(key);
    const generation = (this.generations.get(key) ?? 0) + 1;
    this.generations.set(key, generation);
    const request = (async () => {
      try {
        const saved =
          !force && this.persistent
            ? await this.persistent.blob("avatars", account, username)
            : undefined;
        const content = saved
          ? undefined
          : await this.read(() => {
              if (visibleOnly && !this.visible.has(key))
                throw new Error("Avatar is offscreen");
              return this.client.avatar(
                account,
                username,
                this.controller.signal,
              );
            });
        if (
          this.controller.signal.aborted ||
          this.generations.get(key) !== generation
        )
          return;
        const blob =
          saved?.blob ??
          new Blob([new Uint8Array(content!.bytes).buffer], {
            type: content!.contentType,
          });
        const url = URL.createObjectURL(blob);
        const old = this.values.get(key);
        if (old) URL.revokeObjectURL(old);
        this.values.set(key, url);
        this.checkedAt.set(key, saved?.checkedAt ?? this.now());
        if (saved?.refreshedAt) this.refreshedAt.set(key, saved.refreshedAt);
        if (!saved)
          this.persistent?.saveBlob("avatars", account, username, {
            blob,
            checkedAt: this.now(),
            refreshedAt: this.refreshedAt.get(key),
            etag: content?.etag,
          });
        this.missing.delete(key);
        for (const listener of this.listeners) listener();
      } catch (error) {
        if (
          !this.controller.signal.aborted &&
          this.generations.get(key) === generation &&
          (!visibleOnly || this.visible.has(key))
        )
          this.checkedAt.set(key, this.now());
        if (
          error instanceof NativeApiError &&
          error.code === "media_not_cached" &&
          this.generations.get(key) === generation &&
          !this.controller.signal.aborted
        ) {
          this.missing.add(key);
          this.checkedAt.set(key, this.now());
        }
      }
    })();
    this.pending.set(key, request);
    await request;
    if (this.pending.get(key) === request) this.pending.delete(key);
  }
  async ensure(
    account: string,
    username: string,
    visibleOnly = false,
  ): Promise<void> {
    const key = this.key(account, username);
    if (visibleOnly && !this.visible.has(key)) return;
    if (!this.values.has(key))
      await this.load(account, username, false, visibleOnly);
    const stale =
      this.values.has(key) &&
      this.now() - (this.checkedAt.get(key) ?? -Infinity) >= 3600000;
    if (
      (!this.missing.has(key) && (!this.values.has(key) || !stale)) ||
      this.now() - (this.refreshedAt.get(key) ?? -Infinity) < 3600000 ||
      (visibleOnly && !this.visible.has(key)) ||
      (visibleOnly && !this.refreshAllowed()) ||
      this.controller.signal.aborted
    )
      return;
    this.refreshedAt.set(key, this.now());
    // Lazy requests are best effort. An unavailable avatar keeps its initials.
    await this.refresh(account, username, visibleOnly).catch(() => {});
  }
  async refresh(
    account: string,
    username: string,
    visibleOnly = false,
  ): Promise<void> {
    const key = this.key(account, username);
    const pending = this.refreshing.get(key);
    if (pending) return pending;
    const generation = this.generations.get(key) ?? 0;
    const request = new Promise<void>((resolve, reject) => {
      this.queue.push(() => {
        if (
          this.controller.signal.aborted ||
          (this.generations.get(key) ?? 0) !== generation ||
          (visibleOnly && (!this.visible.has(key) || !this.refreshAllowed()))
        ) {
          this.refreshedAt.delete(key);
          resolve();
          return;
        }
        this.active++;
        void this.download(account, username, generation)
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
  private async download(
    account: string,
    username: string,
    generation: number,
  ): Promise<void> {
    const operation = await this.client.refreshAvatar(account, username);
    const deadline = Date.now() + 125000;
    while (!this.controller.signal.aborted && Date.now() < deadline) {
      const current = await this.client.operation(
        account,
        operation.id,
        this.controller.signal,
      );
      if (current.state === "succeeded") {
        if (
          (this.generations.get(this.key(account, username)) ?? 0) ===
          generation
        )
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
  clear(account: string): void {
    for (const key of new Set([
      ...this.values.keys(),
      ...this.pending.keys(),
      ...this.refreshing.keys(),
      ...this.generations.keys(),
    ]))
      if ((JSON.parse(key) as string[])[0] === account) {
        const url = this.values.get(key);
        if (url) URL.revokeObjectURL(url);
        this.values.delete(key);
        this.missing.delete(key);
        this.checkedAt.delete(key);
        this.refreshedAt.delete(key);
        this.generations.set(key, (this.generations.get(key) ?? 0) + 1);
      }
    for (const listener of this.listeners) listener();
  }
  close(): void {
    clearInterval(this.timer);
    this.visible.clear();
    this.controller.abort();
    this.drain();
    for (const url of this.values.values()) URL.revokeObjectURL(url);
    this.values.clear();
    this.listeners.clear();
  }
}
