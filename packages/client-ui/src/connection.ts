import { NativeWeChatClient } from "@agent-wechat/shared";
import { HubSession, type PendingSend } from "./session.ts";
import { LoginAttempt, type SavedLogin } from "./login-attempt.ts";
export type Connection = {
  session: HubSession;
  simulator: boolean;
  endpoint: string;
  loginAttempt: (account: string) => LoginAttempt;
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
                  typeof s.threadId === "string",
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
  return {
    session,
    simulator,
    endpoint: client.baseUrl.origin,
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
