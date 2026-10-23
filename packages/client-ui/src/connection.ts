import {
  NativeWeChatClient,
  type NativeOperation,
  type NativeMessage,
} from "@agent-wechat/shared";
import { HubSession, type PendingSend } from "./session.ts";
import { LoginAttempt, type SavedLogin } from "./login-attempt.ts";
import { MediaOutbox, type PendingMedia } from "./media-outbox.ts";
import { AvatarCache } from "./avatar-cache.ts";
import { MediaInbox } from "./media-inbox.ts";
import { MessageRecall } from "./message-recall.ts";
import { GroupMutations, validGroupReceipt } from "./group-creation.ts";
import { validQuoteReference } from "./reply.ts";
export type Connection = {
  session: HubSession;
  simulator: boolean;
  endpoint: string;
  loginAttempt: (account: string) => LoginAttempt;
  mediaOutbox: (account: string) => MediaOutbox;
  close: () => void;
  avatars: AvatarCache;
  incomingMedia: MediaInbox;
  messageRecall: (message: NativeMessage) => MessageRecall;
  logout: (account: string) => Promise<NativeOperation>;
  clearLogout: (account: string) => void;
  groupCreation: (account: string, target?: string) => GroupMutations;
};
export async function connect(
  endpoint: string,
  token: string,
  simulator = false,
): Promise<Connection> {
  const client = new NativeWeChatClient({ baseUrl: endpoint, token });
  const status = await client.status();
  if (simulator && status.version.backend !== "simulator")
    throw new Error("Expected a simulator");
  const prefix = `agentwechat-receipts:${status.serverId}:`;
  const session = new HubSession(client, {
    read(account) {
      try {
        const parsed = JSON.parse(
          sessionStorage.getItem(prefix + account) ?? "[]",
        );
        return Array.isArray(parsed)
          ? parsed
              .filter(
                (s: PendingSend) =>
                  typeof s.key === "string" &&
                  typeof s.text === "string" &&
                  typeof s.threadId === "string" &&
                  (s.quote === undefined || validQuoteReference(s.quote)),
              )
              .slice(-50)
          : [];
      } catch {
        return [];
      }
    },
    write(account, sends) {
      try {
        sessionStorage.setItem(
          prefix + account,
          JSON.stringify(sends.slice(-50)),
        );
      } catch {
        /* Storage can be disabled. */
      }
    },
  });
  await session.accounts();
  const logins = new Map<string, SavedLogin>();
  const media = new Map<string, MediaOutbox>();
  const avatars = new AvatarCache(
    client,
    Date.now,
    () => !session.snapshot().opening,
  );
  const incomingMedia = new MediaInbox(client);
  const recalls = new Map<string, MessageRecall>();
  const stopMedia = session.onMediaReady((id) => incomingMedia.ready(id));
  const groups = new Map<string, GroupMutations>();
  const logouts = new Map<string, SavedLogin>();
  const logoutKey = (account: string) =>
    `agentwechat-logout:${status.serverId}:${account}`;
  const saveLogout = (account: string, value: SavedLogin | null) => {
    if (value) logouts.set(account, value);
    else logouts.delete(account);
    try {
      if (value)
        sessionStorage.setItem(logoutKey(account), JSON.stringify(value));
      else sessionStorage.removeItem(logoutKey(account));
    } catch {
      /* Memory fallback remains available. */
    }
  };
  return {
    session,
    simulator,
    endpoint: client.baseUrl.origin,
    close() {
      session.close();
      for (const queue of media.values()) queue.close();
      avatars.close();
      stopMedia();
      incomingMedia.close();
      for (const recall of recalls.values()) recall.close();
      for (const group of groups.values()) group.close();
    },
    avatars,
    incomingMedia,
    messageRecall(message) {
      const key = JSON.stringify([
        message.accountId,
        message.id,
        message.serverId,
      ]);
      let recall = recalls.get(key);
      if (!recall) {
        const storageKey = `agentwechat-recall:${status.serverId}:${key}`;
        recall = new MessageRecall(client, message, {
          read() {
            try {
              return JSON.parse(sessionStorage.getItem(storageKey) ?? "null");
            } catch {
              return null;
            }
          },
          write(receipt) {
            try {
              sessionStorage.setItem(storageKey, JSON.stringify(receipt));
            } catch {
              /* Memory keeps the admission key when storage is disabled. */
            }
          },
        });
        recalls.set(key, recall);
      }
      return recall;
    },
    groupCreation(account, target = "create") {
      const context = JSON.stringify([account, target]);
      let creation = groups.get(context);
      if (!creation) {
        const key = `agentwechat-group-v2:${status.serverId}:${context}`;
        creation = new GroupMutations(client, account, {
          read() {
            try {
              const saved: unknown = JSON.parse(
                sessionStorage.getItem(key) ?? "null",
              );
              return validGroupReceipt(saved, target) ? saved : null;
            } catch {
              return null;
            }
          },
          write(value) {
            try {
              if (value) sessionStorage.setItem(key, JSON.stringify(value));
              else sessionStorage.removeItem(key);
            } catch {
              /* Optional metadata-only recovery. */
            }
          },
        });
        groups.set(context, creation);
      }
      return creation;
    },
    clearLogout(account) {
      saveLogout(account, null);
    },
    async logout(account) {
      let saved = logouts.get(account);
      try {
        const item = JSON.parse(
          sessionStorage.getItem(logoutKey(account)) ?? "null",
        );
        if (
          item &&
          typeof item.key === "string" &&
          item.key.length > 0 &&
          item.key.length <= 200 &&
          (item.operationId === undefined ||
            typeof item.operationId === "string")
        )
          saved = item;
      } catch {
        /* Use memory fallback. */
      }
      if (saved?.operationId)
        return client.operation(account, saved.operationId);
      const key = saved?.key ?? crypto.randomUUID();
      saveLogout(account, { key });
      const operation = await client.logout(account, key);
      saveLogout(account, { key, operationId: operation.id });
      return operation;
    },
    mediaOutbox(account) {
      let outbox = media.get(account);
      if (!outbox) {
        const key = `agentwechat-media:${status.serverId}:${account}`;
        outbox = new MediaOutbox(client, account, {
          read() {
            try {
              const items = JSON.parse(sessionStorage.getItem(key) ?? "[]");
              return Array.isArray(items)
                ? items
                    .filter(
                      (item: PendingMedia) =>
                        typeof item.key === "string" &&
                        typeof item.threadId === "string" &&
                        typeof item.name === "string" &&
                        ["file", "image", "voice", "video"].includes(item.kind),
                    )
                    .slice(-50)
                : [];
            } catch {
              return [];
            }
          },
          write(items) {
            try {
              sessionStorage.setItem(key, JSON.stringify(items));
            } catch {
              /* Optional receipt storage. */
            }
          },
        });
        media.set(account, outbox);
      }
      return outbox;
    },
    loginAttempt(account) {
      const key = `agentwechat-login:${status.serverId}:${account}`;
      return new LoginAttempt(client, account, {
        read() {
          try {
            const saved = JSON.parse(sessionStorage.getItem(key) ?? "null");
            if (
              saved &&
              typeof saved.key === "string" &&
              saved.key.length > 0 &&
              saved.key.length <= 200 &&
              !/[\r\n]/.test(saved.key) &&
              (saved.operationId === undefined ||
                (typeof saved.operationId === "string" &&
                  saved.operationId.length > 0 &&
                  saved.operationId.length <= 200))
            )
              return saved;
          } catch {
            /* Use the in-memory fallback when browser storage is unavailable. */
          }
          return logins.get(account) ?? null;
        },
        write(value) {
          if (value) logins.set(account, value);
          else logins.delete(account);
          try {
            if (value) sessionStorage.setItem(key, JSON.stringify(value));
            else sessionStorage.removeItem(key);
          } catch {
            /* The in-memory fallback still supports this page session. */
          }
        },
      });
    },
  };
}
export async function bootstrap(): Promise<Connection | null> {
  const key = new URLSearchParams(location.hash.slice(1)).get("dev");
  if (key) history.replaceState(null, "", location.pathname + location.search);
  if (!import.meta.env.DEV) return null;
  const response = await fetch("/__dev/session", {
    headers: key ? { Authorization: `Bearer ${key}` } : {},
    cache: "no-store",
  });
  if (
    !response.ok ||
    !response.headers.get("content-type")?.includes("application/json")
  )
    return null;
  const data = await response.json();
  return connect(data.endpoint, data.token, data.simulator === true);
}
export async function control(body: Record<string, unknown>): Promise<void> {
  const response = await fetch("/__dev/control", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error("Simulator control failed");
}

export async function simulatorClock(): Promise<number> {
  const response = await fetch("/__dev/state", { cache: "no-store" });
  if (!response.ok) throw new Error("Simulator clock unavailable");
  const value = await response.json();
  if (typeof value.clock !== "number")
    throw new Error("Invalid simulator clock");
  return value.clock;
}
