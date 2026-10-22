import type { NativeWeChatClient } from "@agent-wechat/shared";
import { pause } from "./session.ts";

export class AvatarCache {
  private client: NativeWeChatClient;
  private values = new Map<string, string>();
  private pending = new Map<string, Promise<void>>();
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
        const content = await this.client.avatar(
          account,
          username,
          this.controller.signal,
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
        for (const listener of this.listeners) listener();
      } catch {
        /* Keep initials when the hub has no cached avatar. */
      }
    })();
    this.pending.set(key, request);
    await request;
  }
  async refresh(account: string, username: string): Promise<void> {
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
    for (const url of this.values.values()) URL.revokeObjectURL(url);
    this.values.clear();
    this.listeners.clear();
  }
}
