import { NativeApiError, type NativeContactPrivacy, type NativeWeChatClient } from "@agent-wechat/shared";

/** Admin-only snapshots for the active account. Never written to browser storage. */
export class ContactPrivacyCache {
  private account: string | null = null;
  private lifetime = new AbortController();
  private entries = new Map<string, NativeContactPrivacy>();
  private reads = new Map<string, symbol>();
  private queued = new Set<string>();
  private listeners = new Set<() => void>();
  private timer?: ReturnType<typeof setTimeout>;
  private warming = false;
  private prefetchUnavailable = false;
  private denial?: NativeApiError;

  private client: Pick<NativeWeChatClient, "contactPrivacy" | "contactPrivacySnapshots">;

  constructor(client: Pick<NativeWeChatClient, "contactPrivacy" | "contactPrivacySnapshots">) {
    this.client = client;
  }

  get(account: string, id: string | null) {
    return account === this.account && id ? this.entries.get(id) : undefined;
  }

  denied(account: string) {
    return account === this.account && !!this.denial;
  }

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  private notify() {
    for (const listener of this.listeners) listener();
  }

  reset(account: string | null, denial?: NativeApiError) {
    this.lifetime.abort();
    this.lifetime = new AbortController();
    clearTimeout(this.timer);
    this.timer = undefined;
    this.account = account;
    this.entries.clear();
    this.reads.clear();
    this.queued.clear();
    this.warming = false;
    this.denial = denial;
    this.prefetchUnavailable = !!denial;
    this.notify();
  }

  invalidate(account: string, id: string) {
    if (account !== this.account) return;
    this.reads.delete(id);
    this.queued.delete(id);
    if (this.entries.delete(id)) this.notify();
  }

  warm(account: string, ids: Iterable<string>) {
    if (account !== this.account || this.prefetchUnavailable) return;
    for (const id of ids) {
      if (!this.reads.has(id)) this.queued.add(id);
    }
    if (!this.warming && !this.timer && this.queued.size) {
      this.timer = setTimeout(() => {
        this.timer = undefined;
        void this.prefetch(account);
      }, 50);
    }
  }

  private checkAccess(account: string, error: unknown) {
    if (!(error instanceof NativeApiError)) return;
    if (error.status === 401 || error.status === 403) {
      this.reset(account, error);
    } else if (error.status === 404 || error.status === 405) {
      this.prefetchUnavailable = true;
      this.queued.clear();
    }
  }

  private async prefetch(account: string) {
    const signal = this.lifetime.signal;
    this.warming = true;
    try {
      while (this.queued.size && !signal.aborted) {
        const ids = [...this.queued].slice(0, 200);
        const read = Symbol();
        for (const id of ids) {
          this.queued.delete(id);
          this.reads.set(id, read);
        }
        try {
          const { snapshots } = await this.client.contactPrivacySnapshots(account, ids, signal);
          if (signal.aborted) return;
          for (const id of ids) {
            if (this.reads.get(id) === read && Object.hasOwn(snapshots, id)) {
              this.entries.set(id, snapshots[id]!);
            }
          }
          this.notify();
        } catch (error) {
          if (signal.aborted) return;
          for (const id of ids) {
            if (this.reads.get(id) === read) this.reads.delete(id);
          }
          this.checkAccess(account, error);
          return;
        }
      }
    } finally {
      if (!signal.aborted) {
        this.warming = false;
        this.warm(account, []);
      }
    }
  }

  async read(account: string, id: string, signal: AbortSignal): Promise<NativeContactPrivacy> {
    const lifetime = this.lifetime.signal;
    if (account !== this.account) throw new DOMException("Account changed", "AbortError");
    if (this.denial) throw this.denial;
    const scopedSignal = AbortSignal.any([signal, lifetime]);
    const read = Symbol();
    this.queued.delete(id);
    this.reads.set(id, read);
    try {
      const snapshot = await this.client.contactPrivacy(account, id, scopedSignal);
      scopedSignal.throwIfAborted();
      if (this.reads.get(id) !== read) {
        const current = this.get(account, id);
        if (current) return current;
        throw new NativeApiError(409, "contact_privacy_pending");
      }
      this.entries.set(id, snapshot);
      this.notify();
      return snapshot;
    } catch (error) {
      if (this.reads.get(id) === read) {
        this.reads.delete(id);
        if (!scopedSignal.aborted && error instanceof NativeApiError && [401, 403].includes(error.status)) this.checkAccess(account, error);
      }
      throw error;
    }
  }
}
