import { QueryClient } from "@tanstack/react-query";
import { NativeApiError, type NativeContact, type NativeWeChatClient } from "@agent-wechat/shared";
import type { ContactPrivacySnapshot } from "./contact-privacy.ts";

/** A friend's relationship settings, version and remark; `null` when the hub has none to edit. */
export type ContactSnapshot = ContactPrivacySnapshot | null;

export const contactKey = (account: string, id: string) => ["contact", account, id] as const;

/**
 * Contact snapshots for the active account, in a TanStack Query cache the session
 * owns. Never written to browser storage. A contact event invalidates its snapshot,
 * which refetches in the background while the old one stays on screen; a catalog
 * page fills only snapshots that no newer point read or invalidation has touched.
 */
export class ContactQueries {
  readonly client = new QueryClient({
    defaultOptions: {
      // Snapshots change only through contact events and our own edits, which
      // invalidate them; nothing refetches on focus or on a timer by default.
      // Snapshots are kept until the session resets the account, as before: a closed
      // card's snapshot (or one the catalog filled) is never garbage-collected.
      queries: { retry: false, staleTime: Infinity, gcTime: Infinity, refetchOnWindowFocus: false, refetchOnReconnect: false },
      // Contact edits carry idempotency keys and recover through their receipts.
      mutations: { retry: 0 },
    },
  });
  private account: string | null = null;
  private denial?: NativeApiError;
  /** Replaced whenever a point read starts or ends, or the contact is invalidated. */
  private marks = new Map<string, symbol>();
  /** Replaced whenever the whole account is invalidated or reset. */
  private accountMark = Symbol();
  private listeners = new Set<() => void>();
  private contacts: Pick<NativeWeChatClient, "contact">;

  constructor(contacts: Pick<NativeWeChatClient, "contact">) {
    this.contacts = contacts;
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

  /** The cached snapshot, or undefined when none has been read. */
  get(account: string, id: string | null): ContactSnapshot | undefined {
    return account === this.account && id ? this.client.getQueryData(contactKey(account, id)) : undefined;
  }

  reset(account: string | null, denial?: NativeApiError) {
    this.account = account;
    this.denial = denial;
    this.marks.clear();
    this.accountMark = Symbol();
    this.client.removeQueries({ queryKey: ["contact"] });
    this.notify();
  }

  /** Refetch every snapshot of the active account within the session. */
  invalidateAccount(account: string) {
    if (account !== this.account || this.denial) return;
    this.accountMark = Symbol();
    void this.client.invalidateQueries({ queryKey: ["contact", account] });
  }

  invalidate(account: string, id: string) {
    if (account !== this.account) return;
    this.marks.set(id, Symbol());
    void this.client.invalidateQueries({ queryKey: contactKey(account, id) });
  }

  /** Accept an inventory only for contacts no point read or invalidation touched since it started. */
  captureCatalog(account: string) {
    const accountMark = this.accountMark;
    const marks = new Map(this.marks);
    return (contacts: NativeContact[]) => {
      if (account !== this.account || this.denial || accountMark !== this.accountMark) return;
      for (const contact of contacts) {
        if (this.marks.get(contact.id) !== marks.get(contact.id)) continue;
        const key = contactKey(account, contact.id);
        if (this.client.getQueryState(key)?.fetchStatus === "fetching") continue;
        const snapshot = snapshotFromContact(contact);
        if (snapshot) {
          // Structural sharing keeps an unchanged snapshot's identity, so it renders nothing.
          this.client.setQueryData(key, snapshot);
        } else if (this.client.getQueryData(key)) {
          // Settings became unavailable or pending: drop a closed card's copy, re-read an open one.
          this.client.removeQueries({ queryKey: key, exact: true, type: "inactive" });
          void this.client.invalidateQueries({ queryKey: key, exact: true, type: "active" });
        }
      }
    };
  }

  /** One point read. A contact still waiting on contact sync reports `contact_privacy_pending`. */
  async read(account: string, id: string, signal: AbortSignal): Promise<ContactSnapshot> {
    if (account !== this.account) throw new DOMException("Account changed", "AbortError");
    if (this.denial) throw this.denial;
    const mark = Symbol();
    this.marks.set(id, mark);
    try {
      const contact = await this.contacts.contact(account, id, signal);
      signal.throwIfAborted();
      if (contact.privacyPending) throw new NativeApiError(409, "contact_privacy_pending");
      return snapshotFromContact(contact) ?? null;
    } catch (error) {
      if (!signal.aborted && account === this.account && error instanceof NativeApiError
        && [401, 403].includes(error.status) && !this.denial) {
        this.denial = error;
        this.notify();
      }
      throw error;
    } finally {
      if (this.marks.get(id) === mark) this.marks.set(id, Symbol());
    }
  }
}

function snapshotFromContact(contact: NativeContact): ContactPrivacySnapshot | undefined {
  if (!contact.privacy || !contact.privacyVersion) return undefined;
  return {
    privacy: contact.privacy,
    version: contact.privacyVersion,
    remark: contact.remark ?? null,
    remarkPending: contact.remarkPending === true,
  };
}
