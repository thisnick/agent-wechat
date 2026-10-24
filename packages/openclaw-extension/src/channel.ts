import { randomUUID } from "node:crypto";
import type { ChannelPlugin } from "openclaw/plugin-sdk/core";
import { DEFAULT_ACCOUNT_ID } from "openclaw/plugin-sdk/account-id";
import { createChannelMessageAdapterFromOutbound } from "openclaw/plugin-sdk/channel-outbound";
import type { ResolvedWeChatAccount } from "./types.ts";
import { resolveWeChatAccount } from "./types.ts";
import { getWeChatRuntime } from "./runtime.ts";

import { collectWeChatStatusIssues } from "./status.ts";
import { isNativeTombstone, type NativeContact, type NativeGroup } from "@agent-wechat/shared";
import { normalizeWeChatCommandBody, normalizeWeChatId } from "./access-control.ts";
import { createWeChatMediaHandlers, wechatMediaActions } from "./outbound-options.ts";
import { nativeConfigSchema } from "./native-config.ts";
import { ensureNativeHost } from "./native-host.ts";
import { startNativeMonitor } from "./native-monitor.ts";
import { nativeLoginStart, nativeLoginWait, sendNativeText } from "./native-login.ts";

async function sendWeChatText(cfg: unknown, to: string, text: string) {
  const account = resolveWeChatAccount(cfg as Record<string, unknown>);
  if (!account?.serverUrl) throw new Error("No native daemon configured");
  return sendNativeText(account, to, text);
}

const mediaHandlers = createWeChatMediaHandlers();
const wechatMessageAdapter = createChannelMessageAdapterFromOutbound({
  id: "agent-wechat",
  capabilities: {
    text: true,
    media: true,
    messageSendingHooks: true,
  },
  outbound: {
    sendText: async ({ cfg, to, text }) => ({
      channel: "agent-wechat",
      ...await sendWeChatText(cfg, to, text),
    }),
    ...mediaHandlers,
  },
});

const meta: ChannelPlugin["meta"] = {
  id: "agent-wechat",
  label: "WeChat",
  selectionLabel: "WeChat (微信)",
  blurb: "WeChat messaging through the standalone native daemon.",
  docsPath: "wechat",
  order: 80,
};

export const wechatPlugin: ChannelPlugin<ResolvedWeChatAccount> = {
  id: "agent-wechat",
  meta,
  gatewayMethods: ["web.login.start", "web.login.wait"],

  capabilities: {
    chatTypes: ["direct", "group"],
    reactions: false,
    threads: false,
    media: true,
    reply: true,
    tts: { voice: { synthesisTarget: "voice-note", transcodesAudio: true } },
  },

  reload: { configPrefixes: ["channels.agent-wechat"] },
  actions: wechatMediaActions,

  configSchema: {
    uiHints: { "native.token": { sensitive: true } },
    schema: {
      type: "object",
      additionalProperties: false,
      properties: {
        backend: { type: "string", enum: ["native"] },
        native: nativeConfigSchema,
        enabled: { type: "boolean" },
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
              enabled: { type: "boolean" },
              requireMention: { type: "boolean" },
              groupPolicy: {
                type: "string",
                enum: ["open", "allowlist", "disabled"],
              },
              allowFrom: { type: "array", items: { type: "string" } },
            },
          },
        },
        mediaMaxMb: { type: "integer", minimum: 1, maximum: 1024 },
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
          backend: "native" as const,
          enabled: false,
          serverUrl: "",
          dmPolicy: "disabled",
          allowFrom: [],
          groupPolicy: "disabled",
          groupAllowFrom: [],
          groups: {},
          mediaMaxMb: 50,
        };
      }
      return account;
    },
    isEnabled: (account) => account.enabled && !!account.serverUrl,
    isConfigured: (account) => !!account.serverUrl,
    unconfiguredReason: () =>
      "Configure channels.agent-wechat.native with a managed or external daemon",
  },

  // ---- Security adapter ----
  security: {
    resolveDmPolicy: ({ account }) => ({
      policy: account.dmPolicy ?? "disabled",
      allowFrom: account.allowFrom ?? [],
      allowFromPath: "channels.agent-wechat.allowFrom",
      policyPath: "channels.agent-wechat.dmPolicy",
      approveHint: "Add the wxid to channels.agent-wechat.allowFrom",
      normalizeEntry: (raw: string) => raw.replace(/^(agent-)?wechat:/i, "").trim(),
    }),
  },

  // ---- Groups adapter ----
  groups: {
    resolveRequireMention: ({ cfg, groupId }) => {
      const wechat = (cfg as any)?.channels?.["agent-wechat"];
      if (!wechat) return true;
      if (!groupId) {
        return wechat.groups?.["*"]?.requireMention ?? true;
      }
      const exact = wechat.groups?.[groupId];
      if (exact?.requireMention != null) {
        return exact.requireMention;
      }
      const normalizedGroupId = normalizeWeChatId(groupId);
      if (normalizedGroupId && wechat.groups?.[normalizedGroupId]?.requireMention != null) {
        return wechat.groups[normalizedGroupId].requireMention;
      }
      return wechat.groups?.["*"]?.requireMention ?? true;
    },
  },

  // ---- Mention adapter ----
  mentions: {
    stripMentions: ({ text, ctx }) =>
      normalizeWeChatCommandBody(text, {
        isGroup: ctx.ChatType === "group",
        wasMentioned: ctx.WasMentioned === true,
      }),
  },

  // ---- Messaging adapter ----
  messaging: {
    normalizeTarget: (raw) => raw.replace(/^(agent-)?wechat:/i, "").trim() || undefined,
    targetResolver: {
      looksLikeId: (raw) => {
        const stripped = raw.replace(/^(agent-)?wechat:/i, "").trim();
        return stripped.includes("@chatroom") || stripped.startsWith("wxid_");
      },
      hint: "WeChat ID (wxid_xxx or xxx@chatroom)",
    },
  },

  // ---- Outbound adapter for OpenClaw hosts using the outbound hook ----
  outbound: {
    deliveryMode: "direct",
    sendText: async ({ cfg, to, text }) => ({
      channel: "agent-wechat" as const,
      ...await sendWeChatText(cfg, to, text),
    }),
    ...mediaHandlers,
  },

  message: wechatMessageAdapter,

  gateway: {
    startAccount: async (ctx) => {
      ctx.log?.info?.(`[wechat:${ctx.accountId}] Starting native event consumer at ${ctx.account.serverUrl}`);
      return startNativeMonitor({ account: ctx.account, abortSignal: ctx.abortSignal,
        runtime: ctx.runtime, setStatus: ctx.setStatus, log: ctx.log, cfg: ctx.cfg });
    },
    loginWithQrStart: async ({ accountId, force, timeoutMs }) => {
      const account = resolveWeChatAccount(getWeChatRuntime().config.current() as Record<string, unknown>, accountId ?? undefined);
      if (!account?.native) return { message: "Configure channels.agent-wechat.native before logging in" };
      return nativeLoginStart(account, { force, timeoutMs });
    },
    logoutAccount: async ({ accountId }) => {
      const account = resolveWeChatAccount(getWeChatRuntime().config.current() as Record<string, unknown>, accountId ?? undefined);
      if (!account?.native) return { cleared: false };
      const { client } = await ensureNativeHost(account.native);
      await client.logout(account.native.accountId, randomUUID());
      return { cleared: false, loggedOut: true };
    },
    loginWithQrWait: async ({ accountId, timeoutMs }) => {
      const account = resolveWeChatAccount(getWeChatRuntime().config.current() as Record<string, unknown>, accountId ?? undefined);
      if (!account?.native) return { connected: false, message: "No native daemon configured" };
      return nativeLoginWait(account, timeoutMs);
    },
  },
  auth: {
    login: async ({ cfg, accountId, runtime }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>, accountId ?? undefined);
      if (!account?.native) throw new Error("Configure channels.agent-wechat.native before logging in");
      const result = await nativeLoginStart(account);
      runtime.log(result.message);
      if (result.qrDataUrl) runtime.log(result.qrDataUrl);
    },
  },
  status: {
    collectStatusIssues: collectWeChatStatusIssues,
    buildAccountSnapshot: ({ account, runtime }) => ({
      accountId: account?.accountId ?? "default", backend: "native",
      handoffUnconfirmed: (runtime as any)?.handoffUnconfirmed,
      needsReview: (runtime as any)?.needsReview,
      enabled: account?.enabled ?? false, configured: Boolean(account?.native),
      running: runtime?.running ?? false, connected: runtime?.connected ?? false,
      linked: runtime?.linked ?? false,
    }),
  },
  directory: {
    self: async ({ cfg }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.native) return null;
      const { client } = await ensureNativeHost(account.native);
      const status = await client.account(account.native.accountId);
      return typeof status.username === "string" ? { kind: "user" as const, id: status.username } : null;
    },
    listPeers: async ({ cfg, query, limit }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.native) return [];
      const { client } = await ensureNativeHost(account.native);
      const page = await client.contacts(account.native.accountId, { query: query ?? undefined, limit: Math.min(limit ?? 50, 200) });
      return page.items.filter((item): item is NativeContact => !isNativeTombstone(item)).slice(0, limit ?? 50)
        .map(item => ({ kind: "user" as const, id: item.id, name: item.displayName ?? item.id }));
    },
    listGroups: async ({ cfg, query, limit }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.native) return [];
      const { client } = await ensureNativeHost(account.native);
      const page = await client.groups(account.native.accountId, { query: query ?? undefined, limit: Math.min(limit ?? 50, 200) });
      return page.items.filter((item): item is NativeGroup => !isNativeTombstone(item)).slice(0, limit ?? 50)
        .map(item => ({ kind: "group" as const, id: item.id, name: item.displayName ?? item.id }));
    },
  },
  heartbeat: {
    checkReady: async ({ cfg }) => {
      const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.native) return { ok: false, reason: "wechat-not-configured" };
      try {
        const { client } = await ensureNativeHost(account.native);
        const status = await client.account(account.native.accountId);
        const ready = status.authenticated === true && status.connection === "online";
        return { ok: ready, reason: ready ? "ok" : "wechat-not-logged-in" };
      } catch { return { ok: false, reason: "wechat-native-unreachable" }; }
    },
  },
  setup: {
    applyAccountConfig: ({ cfg, input }: { cfg: any; accountId?: string; input: any }) => {
      const native = input.native ?? cfg.channels?.["agent-wechat"]?.native;
      if (!native) throw new Error("Set channels.agent-wechat.native: mode, accountId, token reference and consumerStateDir; managed mode needs an installed wx and dataDir, external mode needs an endpoint");
      return { ...cfg, channels: { ...cfg.channels, "agent-wechat": {
        ...cfg.channels?.["agent-wechat"], enabled: true, native,
      } } };
    },
  },
};
