import type { ChannelPlugin, ChannelMeta } from "openclaw/plugin-sdk";
import { DEFAULT_ACCOUNT_ID } from "openclaw/plugin-sdk";
import type { ResolvedWeChatAccount } from "./types.js";
import { resolveWeChatAccount } from "./types.js";
import { getWeChatRuntime } from "./runtime.js";
import { startWeChatMonitor } from "./monitor.js";
import { wechatOnboardingAdapter } from "./onboarding.js";
import { collectWeChatStatusIssues } from "./status.js";
import { WeChatClient } from "@agent-wechat/shared";
import { loginStart, loginWait, loginTerminal } from "./login.js";
// loginWait still used by gateway.loginWithQrWait
import { createWeChatLoginTool } from "./agent-tools.js";

const meta: ChannelMeta = {
  id: "wechat",
  label: "WeChat",
  selectionLabel: "WeChat (微信)",
  blurb: "WeChat messaging via agent-wechat container.",
  docsPath: "wechat",
  aliases: ["weixin"],
  order: 80,
};

export const wechatPlugin: ChannelPlugin<ResolvedWeChatAccount> = {
  id: "wechat",
  meta,
  gatewayMethods: ["web.login.start", "web.login.wait"],

  capabilities: {
    chatTypes: ["direct", "group"],
    reactions: false,
    threads: false,
    media: true,
    reply: true,
  },

  reload: { configPrefixes: ["channels.wechat"] },

  configSchema: {
    schema: {
      type: "object",
      additionalProperties: false,
      properties: {
        enabled: { type: "boolean" },
        serverUrl: { type: "string" },
        token: { type: "string" },
        dmPolicy: {
          type: "string",
          enum: ["open", "allowlist", "disabled"],
        },
        allowFrom: { type: "array", items: { type: "string" } },
        groupPolicy: {
          type: "string",
          enum: ["open", "allowlist", "disabled"],
        },
        groupAllowFrom: { type: "array", items: { type: "string" } },
        groups: {
          type: "object",
          additionalProperties: {
            type: "object",
            properties: {
              requireMention: { type: "boolean" },
            },
          },
        },
        pollIntervalMs: { type: "integer", minimum: 100 },
        authPollIntervalMs: { type: "integer", minimum: 1000 },
      },
    },
  },

  // ---- Config adapter ----
  config: {
    listAccountIds: (_cfg) => [DEFAULT_ACCOUNT_ID],
    resolveAccount: (cfg, accountId) => {
      const account = resolveWeChatAccount(
        cfg as unknown as Record<string, unknown>,
        accountId ?? undefined,
      );
      if (!account) {
        return {
          accountId: accountId ?? DEFAULT_ACCOUNT_ID,
          enabled: false,
          serverUrl: "",
          token: undefined,
          dmPolicy: "disabled",
          allowFrom: [],
          groupPolicy: "disabled",
          groupAllowFrom: [],
          groups: {},
          pollIntervalMs: 1000,
          authPollIntervalMs: 30000,
        };
      }
      return account;
    },
    isEnabled: (account) => account.enabled && !!account.serverUrl,
    isConfigured: (account) => !!account.serverUrl,
    unconfiguredReason: () =>
      "No serverUrl configured. Run: openclaw channels setup wechat",
  },

  // ---- Security adapter ----
  security: {
    resolveDmPolicy: ({ account }) => ({
      policy: account.dmPolicy ?? "disabled",
      allowFrom: account.allowFrom ?? [],
      allowFromPath: "channels.wechat.allowFrom",
      policyPath: "channels.wechat.dmPolicy",
      approveHint: "Add the wxid to channels.wechat.allowFrom",
    }),
  },

  // ---- Groups adapter ----
  groups: {
    resolveRequireMention: ({ cfg, groupId }) => {
      const wechat = (cfg as any)?.channels?.wechat;
      if (!wechat) return true;
      if (groupId && wechat.groups?.[groupId]?.requireMention != null) {
        return wechat.groups[groupId].requireMention;
      }
      return true; // Default: require mention in groups
    },
  },

  // ---- Messaging adapter ----
  messaging: {
    normalizeTarget: (raw) => raw.replace(/^wechat:/i, "").trim() || undefined,
    targetResolver: {
      looksLikeId: (raw) => {
        const stripped = raw.replace(/^wechat:/i, "").trim();
        return stripped.includes("@chatroom") || stripped.startsWith("wxid_");
      },
      hint: "WeChat ID (wxid_xxx or xxx@chatroom)",
    },
  },

  // ---- Outbound adapter ----
  outbound: {
    deliveryMode: "direct",
    sendText: async ({ cfg, to, text }) => {
      const account = resolveWeChatAccount(
        cfg as unknown as Record<string, unknown>,
      );
      if (!account?.serverUrl) {
        throw new Error("No serverUrl configured");
      }
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      const result = await client.sendMessage({ chatId: to, text });
      if (!result.success) {
        throw new Error(result.error ?? "Send failed");
      }
      return {
        channel: "wechat" as const,
        messageId: `wechat:${to}:${Date.now()}`,
      };
    },
    sendMedia: async ({ cfg, to, text, mediaUrl }) => {
      const account = resolveWeChatAccount(
        cfg as unknown as Record<string, unknown>,
      );
      if (!account?.serverUrl) {
        throw new Error("No serverUrl configured");
      }
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      if (mediaUrl) {
        const fsmod = await import("fs/promises");
        const pathmod = await import("path");

        let base64: string;
        let mimeType: string;
        let filename: string;
        if (mediaUrl.startsWith("http://") || mediaUrl.startsWith("https://")) {
          const res = await fetch(mediaUrl);
          const buffer = await res.arrayBuffer();
          base64 = Buffer.from(buffer).toString("base64");
          mimeType = res.headers.get("content-type") ?? "application/octet-stream";
          const urlPath = new URL(mediaUrl).pathname;
          filename = pathmod.basename(urlPath) || "file";
        } else {
          const buf = await fsmod.readFile(mediaUrl);
          base64 = buf.toString("base64");
          filename = pathmod.basename(mediaUrl);
          const ext = pathmod.extname(mediaUrl).toLowerCase().replace(".", "");
          const extMime: Record<string, string> = {
            png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg",
            gif: "image/gif", webp: "image/webp",
          };
          mimeType = extMime[ext] ?? "application/octet-stream";
        }

        const isImage = mimeType.startsWith("image/");
        const result = isImage
          ? await client.sendMessage({
              chatId: to,
              text: text || undefined,
              image: { data: base64, mimeType },
            })
          : await client.sendMessage({
              chatId: to,
              text: text || undefined,
              file: { data: base64, filename },
            });
        if (!result.success) {
          throw new Error(result.error ?? "Send media failed");
        }
        return {
          channel: "wechat" as const,
          messageId: `wechat:${to}:${Date.now()}`,
        };
      }
      // Text-only fallback
      const result = await client.sendMessage({
        chatId: to,
        text: text || undefined,
      });
      if (!result.success) {
        throw new Error(result.error ?? "Send failed");
      }
      return {
        channel: "wechat" as const,
        messageId: `wechat:${to}:${Date.now()}`,
      };
    },
  },

  // ---- Gateway adapter ----
  gateway: {
    startAccount: async (ctx) => {
      ctx.log?.info?.(
        `[wechat:${ctx.accountId}] Starting monitor (polling ${ctx.account.serverUrl})`,
      );
      return startWeChatMonitor({
        account: ctx.account,
        abortSignal: ctx.abortSignal,
        runtime: ctx.runtime,
        setStatus: ctx.setStatus,
        log: ctx.log,
        cfg: ctx.cfg,
      });
    },

    loginWithQrStart: async ({ accountId, force, timeoutMs }) => {
      const cfg = getWeChatRuntime().config.loadConfig();
      const account = resolveWeChatAccount(
        cfg as Record<string, unknown>,
        accountId ?? undefined,
      );
      if (!account?.serverUrl) {
        return { message: "No serverUrl configured. Run: openclaw channels setup wechat" };
      }
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });

      try {
        const result = await loginStart(client, accountId ?? DEFAULT_ACCOUNT_ID, { timeoutMs, force });
        return result;
      } catch (err) {
        return {
          message: `Login failed: ${err instanceof Error ? err.message : String(err)}`,
        };
      }
    },

    logoutAccount: async ({ accountId }) => {
      const cfg = getWeChatRuntime().config.loadConfig();
      const account = resolveWeChatAccount(
        cfg as Record<string, unknown>,
        accountId ?? undefined,
      );
      if (!account?.serverUrl) return { cleared: false };
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      try {
        const result = await client.logout();
        return { cleared: result.success, loggedOut: result.success };
      } catch {
        return { cleared: false };
      }
    },

    loginWithQrWait: async ({ accountId, timeoutMs }) => {
      const result = await loginWait(accountId ?? DEFAULT_ACCOUNT_ID, { timeoutMs });
      return result;
    },
  },

  // ---- Auth adapter ----
  auth: {
    login: async ({ cfg, accountId, runtime }) => {
      const account = resolveWeChatAccount(
        cfg as Record<string, unknown>,
        accountId ?? undefined,
      );
      if (!account?.serverUrl) {
        throw new Error(
          "No serverUrl configured. Run: openclaw channels setup wechat",
        );
      }

      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });

      // Check if already logged in
      try {
        const auth = await client.authStatus();
        if (auth.status === "logged_in") {
          runtime.log(
            `Already logged in${auth.loggedInUser ? ` as ${auth.loggedInUser}` : ""}`,
          );
          return;
        }
      } catch {
        // Auth check failed — proceed with login anyway
      }

      runtime.log("Starting WeChat login...\n");

      const { connected, message } = await loginTerminal(client, {
        onEvent: (event) => {
          switch (event.type) {
            case "status":
              runtime.log(`Status: ${event.message}`);
              break;
            case "qr":
              runtime.log("Scan this QR code with WeChat:\n");
              if (event.qrData) {
                // Print QR to terminal using qrcode-terminal if available,
                // otherwise show the data URL
                try {
                  const qrTerminal = require("qrcode-terminal");
                  const qrInput = event.qrBinaryData
                    ? Buffer.from(event.qrBinaryData as number[]).toString("utf-8")
                    : event.qrData;
                  qrTerminal.generate(qrInput, { small: true }, (qr: string) => {
                    runtime.log(qr);
                  });
                } catch {
                  // qrcode-terminal not available — show data URL hint
                  if (event.qrDataUrl) {
                    runtime.log("(QR data URL available — open in browser to scan)");
                  }
                }
              }
              runtime.log("\nWaiting for scan...\n");
              break;
            case "phone_confirm":
              runtime.log(
                `\n${event.message || "Please confirm login on your phone"}\n`,
              );
              break;
            case "login_success":
              runtime.log("\nLogin successful!");
              if (event.userId) {
                runtime.log(`User: ${event.userId}`);
              }
              break;
            case "login_timeout":
              runtime.log("\nLogin timed out. Please try again.");
              break;
            case "error":
              runtime.log(`\nError: ${event.message}`);
              break;
          }
        },
      });

      if (!connected) {
        throw new Error(message);
      }
    },
  },

  // ---- Status adapter ----
  status: {
    collectStatusIssues: collectWeChatStatusIssues,
  },

  // ---- Agent tools adapter ----
  agentTools: (({ cfg }: { cfg?: any }) => {
    const account = resolveWeChatAccount(cfg as Record<string, unknown>);
    if (!account?.serverUrl) return [];
    return [createWeChatLoginTool(account)];
  }) as any,

  // ---- Directory adapter ----
  directory: {
    self: async ({ cfg }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.serverUrl) return null;
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      try {
        const auth = await client.authStatus();
        if (auth.status !== "logged_in" || !auth.loggedInUser) return null;
        return { kind: "user" as const, id: auth.loggedInUser };
      } catch {
        return null;
      }
    },
    listPeers: async ({ cfg, query, limit }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.serverUrl) return [];
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      try {
        const chats = query
          ? await client.findChats(query)
          : await client.listChats(limit ?? 50);
        return chats
          .filter((c) => !c.username.includes("@chatroom"))
          .map((c) => ({
            kind: "user" as const,
            id: c.username,
            name: c.name,
          }));
      } catch {
        return [];
      }
    },
    listGroups: async ({ cfg, query, limit }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.serverUrl) return [];
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      try {
        const chats = query
          ? await client.findChats(query)
          : await client.listChats(limit ?? 50);
        return chats
          .filter((c) => c.username.includes("@chatroom"))
          .map((c) => ({
            kind: "group" as const,
            id: c.username,
            name: c.name,
          }));
      } catch {
        return [];
      }
    },
  },

  // ---- Heartbeat adapter ----
  heartbeat: {
    checkReady: async ({ cfg }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.serverUrl) {
        return { ok: false, reason: "wechat-not-configured" };
      }
      const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
      try {
        const auth = await client.authStatus();
        if (auth.status !== "logged_in") {
          return { ok: false, reason: "wechat-not-logged-in" };
        }
        return { ok: true, reason: "ok" };
      } catch {
        return { ok: false, reason: "wechat-unreachable" };
      }
    },
  },

  // ---- Setup adapter (channels add) ----
  setup: {
    applyAccountConfig: ({ cfg, input }: { cfg: any; accountId?: string; input: any }) => {
      const serverUrl = input.url || input.httpUrl || "http://localhost:6174";
      const token = input.token;
      return {
        ...cfg,
        channels: {
          ...cfg.channels,
          wechat: {
            ...cfg.channels?.wechat,
            enabled: true,
            serverUrl,
            ...(token ? { token } : {}),
          },
        },
      };
    },
  },

  // ---- Onboarding adapter ----
  onboarding: wechatOnboardingAdapter,
};
