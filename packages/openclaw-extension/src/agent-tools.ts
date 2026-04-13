import type { ResolvedWeChatAccount } from "./types.js";
import { WeChatClient } from "@agent-wechat/shared";

function createClient(account: ResolvedWeChatAccount) {
  return new WeChatClient({
    baseUrl: account.serverUrl,
    token: account.token,
  });
}

function normalizeChatId(raw: unknown): string | null {
  if (typeof raw !== "string") {
    return null;
  }
  const chatId = raw.replace(/^wechat:/i, "").trim();
  return chatId || null;
}

export function createWeChatLoginTool(account: ResolvedWeChatAccount) {
  const client = createClient(account);

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
          const { getActiveLoginState, loginStart } = await import("./login.js");

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

export function createWeChatReceiveTransferTool(account: ResolvedWeChatAccount) {
  const client = createClient(account);

  return {
    label: "WeChat Transfer",
    name: "wechat_receive_transfer",
    description:
      "Receive an incoming WeChat transfer in a chat. Use this when the current WeChat message is an unreceived transfer. Prefer passing the localId from the transfer message when available. If localId or transactionId are omitted, the latest receivable transfer in the chat is used.",
    parameters: {
      type: "object",
      properties: {
        chatId: {
          type: "string",
          description:
            "WeChat chat ID for the transfer, for example a wxid, a contact username, or a wechat: prefixed chat target from the current conversation context.",
        },
        localId: {
          type: "number",
          description:
            "Optional local message ID for the exact transfer to receive. Prefer the localId shown in the current transfer message when available.",
        },
        transactionId: {
          type: "string",
          description:
            "Optional transfer transaction ID. Use this when localId is unavailable.",
        },
      },
      required: ["chatId"],
    },
    execute: async (_toolCallId: string, params: unknown) => {
      const args = params as Record<string, unknown>;
      const chatId = normalizeChatId(args.chatId);
      const localId = typeof args.localId === "number"
        ? args.localId
        : undefined;
      const transactionId = typeof args.transactionId === "string" &&
          args.transactionId.trim()
        ? args.transactionId.trim()
        : undefined;

      if (!chatId) {
        return {
          content: [{
            type: "text" as const,
            text: "Failed to receive the WeChat transfer: chatId is required.",
          }],
          details: { error: true, reason: "missing_chat_id" },
        };
      }

      try {
        const result = await client.receiveTransfer(
          chatId,
          transactionId,
          localId,
        );
        const amount = result.amountText ? ` ${result.amountText}` : "";
        const details = [
          `Received the WeChat transfer${amount} in ${chatId}.`,
          result.localId != null ? `Local ID: ${result.localId}` : undefined,
          result.transactionId
            ? `Transaction ID: ${result.transactionId}`
            : undefined,
          result.transferId ? `Transfer ID: ${result.transferId}` : undefined,
          result.receivedAt ? `Received at: ${result.receivedAt}` : undefined,
        ].filter(Boolean);

        if (result.success) {
          return {
            content: [{ type: "text" as const, text: details.join("\n") }],
            details: result,
          };
        }

        const failureText = `Failed to receive the WeChat transfer in ${chatId}: ${result.error ?? "Unknown error"}.`;
        return {
          content: [{ type: "text" as const, text: failureText }],
          details: result,
        };
      } catch (err) {
        const text = `Failed to receive the WeChat transfer in ${chatId}: ${err instanceof Error ? err.message : String(err)}`;
        return {
          content: [{ type: "text" as const, text }],
          details: { error: true },
        };
      }
    },
  };
}
