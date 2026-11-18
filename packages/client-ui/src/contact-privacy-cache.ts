import { NativeApiError, type NativeContact, type NativeWeChatClient } from "@agent-wechat/shared";
import type { ContactPrivacySnapshot } from "./contact-privacy.ts";

/** Relationship settings for the active account. Never written to browser storage. */
export class ContactPrivacyCache {
  private account: string | null = null;
  private lifetime = new AbortController();
  private session = Symbol();
  private entries = new Map<string, ContactPrivacySnapshot>();
  private reads = new Map<string, symbol>();
  private listeners = new Set<() => void>();
  private denial?: NativeApiError;

  private client: Pick<NativeWeChatClient, "contact">;

  constructor(client: Pick<NativeWeChatClient, "contact">) {
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
    this.session = Symbol();
    this.account = account;
    this.denial = denial;
    this.clearEntries();
  }

  /** Invalidate snapshots within the active session so the dialog can retry its read. */
  invalidateAccount(account: string) {
    if (account !== this.account || this.denial) return;
    this.clearEntries();
  }

  private clearEntries() {
    this.lifetime.abort();
    this.lifetime = new AbortController();
    this.entries.clear();
    this.reads.clear();
    this.notify();
  }

  invalidate(account: string, id: string) {
    if (account !== this.account) return;
    this.reads.set(id, Symbol());
    if (this.entries.delete(id)) this.notify();
  }

  /** Accept an inventory only if it predates no point read or invalidation of that contact. */
  captureCatalog(account: string) {
    const lifetime = this.lifetime.signal;
    const reads = new Map(this.reads);
    return (contacts: NativeContact[]) => {
      if (lifetime.aborted || account !== this.account || this.denial) return;
      let changed = false;
      for (const contact of contacts) {
        if (this.reads.get(contact.id) !== reads.get(contact.id)) continue;
        const snapshot = snapshotFromContact(contact);
        if (snapshot) {
          if (this.entries.get(contact.id)?.version === snapshot.version) continue;
          this.entries.set(contact.id, snapshot);
          changed = true;
        } else if (this.entries.delete(contact.id)) {
          changed = true;
        }
      }
      if (changed) this.notify();
    };
  }

  async read(account: string, id: string, signal: AbortSignal): Promise<ContactPrivacySnapshot | undefined> {
    const lifetime = this.lifetime.signal;
    const session = this.session;
    if (account !== this.account) throw new DOMException("Account changed", "AbortError");
    if (this.denial) throw this.denial;
    const scopedSignal = AbortSignal.any([signal, lifetime]);
    const read = Symbol();
    this.reads.set(id, read);
    try {
      const contact = await this.client.contact(account, id, scopedSignal);
      const snapshot = snapshotFromContact(contact);
      scopedSignal.throwIfAborted();
      if (this.reads.get(id) !== read) {
        const current = this.get(account, id);
        if (current) return current;
        throw new NativeApiError(409, "contact_privacy_pending");
      }
      if (contact.privacyPending) {
        throw new NativeApiError(409, "contact_privacy_pending");
      }
      this.reads.set(id, Symbol());
      if (snapshot) {
        this.entries.set(id, snapshot);
        this.notify();
      } else if (this.entries.delete(id)) {
        this.notify();
      }
      return snapshot;
    } catch (error) {
      if (this.reads.get(id) === read) {
        this.reads.set(id, Symbol());
        if (!scopedSignal.aborted && error instanceof NativeApiError && [401, 403].includes(error.status)) this.reset(account, error);
      }
      if (lifetime.aborted && !signal.aborted && this.session === session) {
        throw new NativeApiError(409, "contact_privacy_pending");
      }
      throw error;
    }
  }
}

function snapshotFromContact(contact: NativeContact): ContactPrivacySnapshot | undefined {
  if (!contact.privacy || !contact.privacyVersion) return undefined;
  return { privacy: contact.privacy, version: contact.privacyVersion };
}
