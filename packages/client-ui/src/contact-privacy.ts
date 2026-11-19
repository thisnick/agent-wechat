import {
  NativeApiError,
  type NativeOperation,
  type NativeContactPrivacySettings,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";

export type ContactPrivacy = NativeContactPrivacySettings;
/**
 * Settings and the account-bound version read from a contact's `privacy` and `privacyVersion`,
 * with the remark read alongside them. The version also guards remark edits.
 */
export type ContactPrivacySnapshot = {
  privacy: ContactPrivacy;
  version: string;
  remark: string | null;
  remarkPending: boolean;
};
/** The daemon's limit for a remark, in UTF-8 bytes after trimming. */
export const MAX_CONTACT_REMARK_BYTES = 256;
/** WeChat confirms a saved remark through a separate lookup that no event announces. */
export const REMARK_POLL_MS = 2_000;
export const REMARK_POLL_LIMIT = 30;
/** Friend edits save on their own: privacy after a short pause, the remark once typing stops. */
export const PRIVACY_AUTOSAVE_MS = 300;
export const REMARK_AUTOSAVE_MS = 800;
/** How long a field says "Saved" after its change is confirmed. */
export const SAVED_FLASH_MS = 2_000;
export const defaultContactPrivacy = (): ContactPrivacy => ({
  chatsOnly: false,
  hideMyPosts: false,
  hideTheirPosts: false,
});
export function sameContactPrivacy(a: ContactPrivacy, b: ContactPrivacy) {
  return (
    a.chatsOnly === b.chatsOnly &&
    a.hideMyPosts === b.hideMyPosts &&
    a.hideTheirPosts === b.hideTheirPosts
  );
}
export type ContactChange = { key: string; operation?: NativeOperation } & (
  | {
      action: "send";
      input: {
        recipientRef: string;
        message: string;
        privacy?: ContactPrivacy;
      };
    }
  | {
      action: "accept";
      requestId: string;
      version: string;
      privacy?: ContactPrivacy;
    }
  | { action: "delete"; requestId: string; version: string }
  | { action: "remove"; contactId: string }
  | {
      action: "privacy";
      contactId: string;
      input: { privacy: ContactPrivacy; expectedVersion: string };
    }
  | {
      action: "remark";
      contactId: string;
      /** `null` clears the remark. */
      input: { remark: string | null; expectedVersion: string };
    }
);
/** Older pending receipts predate frozen targets and privacy choices. Keep their wire inputs unchanged. */
export function restoreContactChange(
  saved: ContactChange | undefined,
  target: { requestId?: string; version?: string; contactId?: string },
): ContactChange | undefined {
  if (!saved) return saved;
  if (saved.action === "accept" || saved.action === "delete")
    return {
      ...saved,
      requestId: saved.requestId ?? target.requestId!,
      version: saved.version ?? target.version!,
    };
  if (saved.action === "remove")
    return { ...saved, contactId: saved.contactId ?? target.contactId! };
  return saved;
}
export const contactChangeActions = {
  send: "send_request",
  accept: "accept_request",
  delete: "delete_request",
  remove: "remove_contact",
  privacy: "update_privacy",
  remark: "update_remark",
} as const;
/** The dialog keeps frozen inputs here so an inspected retry cannot change its intent. */
export interface ContactDetailActions {
  submit(change: ContactChange): Promise<NativeOperation>;
}
/** Translate card actions to the typed SDK without changing frozen intent inputs. */
export function contactDetailActions(
  connection: Connection,
  account: string,
): ContactDetailActions {
  const client = connection.session.client;
  return {
    submit: (change) => {
      switch (change.action) {
        case "send":
          return client.sendContactRequest(account, change.input, change.key);
        case "accept":
          return client.acceptContactRequest(
            account,
            change.requestId,
            { expectedVersion: change.version, privacy: change.privacy },
            change.key,
          );
        case "delete":
          return client.deleteContactRequest(
            account,
            change.requestId,
            change.version,
            change.key,
          );
        case "remove":
          return client.removeContact(account, change.contactId, change.key);
        case "privacy":
        case "remark":
          return client.updateContact(
            account,
            change.contactId,
            change.input,
            change.key,
          );
      }
    },
  };
}

/** Read-only reconciliation for a canonical snapshot still waiting on contact sync. */
export async function readContactPrivacyWithPendingRetry<T = ContactPrivacySnapshot | undefined>(
  read: (signal: AbortSignal) => Promise<T>,
  signal: AbortSignal,
  options: {
    timeoutMs?: number;
    pending?: () => void;
    wait?: (milliseconds: number, signal: AbortSignal) => Promise<void>;
  } = {},
): Promise<T> {
  const deadline = new AbortController();
  const active = AbortSignal.any([signal, deadline.signal]);
  const timer = setTimeout(() => deadline.abort(), options.timeoutMs ?? 10_000);
  const delays = [250, 500, 1000, 1500, 2000, 2000, 2500];
  const wait =
    options.wait ??
    ((milliseconds: number, waiting: AbortSignal) =>
      new Promise<void>((resolve) => {
        if (waiting.aborted) {
          resolve();
          return;
        }
        const done = () => {
          clearTimeout(pause);
          waiting.removeEventListener("abort", done);
          resolve();
        };
        const pause = setTimeout(done, milliseconds);
        waiting.addEventListener("abort", done, { once: true });
      }));
  let lastPending: NativeApiError | undefined;
  try {
    for (let attempt = 0; ; attempt++) {
      active.throwIfAborted();
      try {
        const value = await read(active);
        active.throwIfAborted();
        return value;
      } catch (error) {
        active.throwIfAborted();
        if (
          !(error instanceof NativeApiError) ||
          error.status !== 409 ||
          error.code !== "contact_privacy_pending"
        )
          throw error;
        lastPending = error;
        options.pending?.();
        if (attempt === delays.length) throw error;
        await wait(delays[attempt], active);
      }
    }
  } catch (error) {
    if (signal.aborted) signal.throwIfAborted();
    if (deadline.signal.aborted && lastPending) throw lastPending;
    throw error;
  } finally {
    clearTimeout(timer);
  }
}
