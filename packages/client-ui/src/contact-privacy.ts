import type {
  NativeOperation,
  NativeContactPrivacy,
  NativeContactPrivacySettings,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";

export type ContactPrivacy = NativeContactPrivacySettings;
export type ContactPrivacySnapshot = NativeContactPrivacy;
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
} as const;
/** The dialog keeps frozen inputs here so an inspected retry cannot change its intent. */
export interface ContactDetailActions {
  readPrivacy(id: string, signal: AbortSignal): Promise<ContactPrivacySnapshot>;
  submit(change: ContactChange): Promise<NativeOperation>;
}
/** Translate card actions to the typed SDK without changing frozen intent inputs. */
export function contactDetailActions(
  connection: Connection,
  account: string,
): ContactDetailActions {
  const client = connection.session.client;
  return {
    readPrivacy: async (id, signal) =>
      client.contactPrivacy(account, id, signal),
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
