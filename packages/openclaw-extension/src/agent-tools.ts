import type { ResolvedWeChatAccount } from "./types.js";
import { WeChatClient } from "@agent-wechat/shared";
import { loginStart, getActiveLoginState } from "./login.js";

export function createWeChatLoginTool(account: ResolvedWeChatAccount) {
  const client = new WeChatClient({
    baseUrl: account.serverUrl,
    token: account.token,
  });

  return {
    label: "WeChat Login",
    name: "wechat_login",
    description:
      "Check WeChat login status, start a login session, or log out. Calling start again returns the latest state from the existing session. When start returns qrData, generate a QR code image from it and show it to the user so they can scan it with their phone.",
    parameters: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["start", "logout", "status"],
        },
        force: {
          type: "boolean",
          description:
            "Log in with a new account (shows QR code even if already logged in)",
        },
        timeoutMs: { type: "number" },
      },
      required: ["action"],
    },
    execute: async (_toolCallId: string, params: unknown) => {
      const args = params as Record<string, unknown>;
      const action = args.action as "start" | "logout" | "status";
      const force = args.force as boolean | undefined;
      const timeoutMs = args.timeoutMs as number | undefined;

      switch (action) {
        case "status": {
          try {
            const auth = await client.authStatus();
            const text = auth.status === "logged_in"
              ? `WeChat is logged in${auth.loggedInUser ? ` as ${auth.loggedInUser}` : ""}.`
              : `WeChat status: ${auth.status.replace(/_/g, " ")}.`;
            return {
              content: [{ type: "text" as const, text }],
              details: auth,
            };
          } catch (err) {
            const text = `Failed to check WeChat status: ${err instanceof Error ? err.message : String(err)}`;
            return {
              content: [{ type: "text" as const, text }],
              details: { error: true },
            };
          }
        }

        case "start": {
          // Check for existing active login session
          const existing = getActiveLoginState(account.accountId);
          if (existing.active && !force) {
            if (existing.done && existing.connected) {
              return {
                content: [
                  { type: "text" as const, text: "Login successful." },
                ],
                details: { state: "done", connected: true },
              };
            }
            if (existing.done) {
              return {
                content: [
                  {
                    type: "text" as const,
                    text:
                      existing.error ??
                      existing.message ??
                      "Login session ended.",
                  },
                ],
                details: {
                  state: "done",
                  connected: false,
                  error: existing.error,
                },
              };
            }
            // Still in progress — return cached state
            const parts: string[] = [];
            if (existing.message) parts.push(existing.message);
            if (existing.qrData)
              parts.push(`QR data: ${existing.qrData}`);
            return {
              content: [
                {
                  type: "text" as const,
                  text: parts.join("\n") || "Login in progress...",
                },
              ],
              details: {
                state: existing.qrData ? "qr" : "waiting",
                qrData: existing.qrData,
              },
            };
          }

          // Start a new login session
          try {
            const result = await loginStart(client, account.accountId, {
              timeoutMs,
              force,
            });
            // After loginStart resolves, check state for qrData
            const state = getActiveLoginState(account.accountId);
            if (state.qrData) {
              return {
                content: [
                  {
                    type: "text" as const,
                    text: `${result.message}\nQR data: ${state.qrData}`,
                  },
                ],
                details: { state: "qr", qrData: state.qrData },
              };
            }
            return {
              content: [
                { type: "text" as const, text: result.message },
              ],
              details: { state: "waiting" },
            };
          } catch (err) {
            const text = `Failed to start WeChat login: ${err instanceof Error ? err.message : String(err)}`;
            return {
              content: [{ type: "text" as const, text }],
              details: { error: true },
            };
          }
        }

        case "logout": {
          try {
            const result = await client.logout();
            const text = result.success
              ? "WeChat logged out successfully."
              : `WeChat logout failed${result.error ? `: ${result.error}` : ""}.`;
            return {
              content: [{ type: "text" as const, text }],
              details: result,
            };
          } catch (err) {
            const text = `Failed to log out of WeChat: ${err instanceof Error ? err.message : String(err)}`;
            return {
              content: [{ type: "text" as const, text }],
              details: { error: true },
            };
          }
        }
      }
    },
  };
}
