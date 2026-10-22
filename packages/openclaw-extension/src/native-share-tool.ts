import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { NativeApiError, type ShareSendRequest } from "@agent-wechat/shared";
import { resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { nativeOutboundDelivery } from "./native-send.ts";

const name = "wechat_send_share";
function text(value: unknown, label: string, max: number, multiline = false): string {
  if (typeof value !== "string" || !value.trim() || Buffer.byteLength(value) > max
    || (multiline ? /[\u0000-\u0008\u000b-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) throw Error(`Invalid ${label}`);
  return value;
}
function thread(value: unknown): string {
  return text(text(value, "current conversation", 256).replace(/^(?:agent-)?wechat:(?:group:)?/i, "").trim(), "current conversation", 256);
}
function url(value: unknown, label: string): string {
  const raw = text(value, label, 8192);
  const parsed = new URL(raw);
  if (!["https:", "http:"].includes(parsed.protocol) || !parsed.hostname || parsed.username || parsed.password) throw Error(`Invalid ${label}`);
  return raw;
}
/** Sending is confined to the same trusted route and sender admission as local
 * history. Tool arguments cannot select a different account or conversation. */
export function createNativeShareTools(context: OpenClawPluginToolContext, dependencies: { host?: typeof ensureNativeHost; core?: any } = {}): ChannelAgentTool[] {
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!currentConfig()) return [];
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.enabled || !selected.native || context.messageChannel !== "agent-wechat"
    || (context.agentAccountId && context.agentAccountId !== selected.accountId)
    || (context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId)) return [];
  let conversation: string, requester: string;
  try {
    const native = context.nativeChannelId ? thread(context.nativeChannelId) : undefined;
    const delivery = context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.to ? thread(context.deliveryContext.to) : undefined;
    if (native && delivery && native !== delivery) return [];
    conversation = native ?? delivery!;
    if (!conversation) return [];
    requester = text(context.requesterSenderId, "requester sender", 256);
  } catch { return []; }
  const configurationKey = nativeConfigurationKey(selected.native);
  const fields = ["title", "url", "description", "thumbnailUrl", "idempotencyKey"];
  return [{
    name, label: "Send WeChat share",
    description: "Send a URL share card to the current WeChat conversation, with an explicit title, URL, optional description and public thumbnail URL. Does not fetch or summarize the linked page. Use one idempotencyKey per intended share and reuse it after timeouts; never retry an uncertain send with a new key. Returns a durable send ID and state. Accepted means server acceptance; recipient rendering requires verification.",
    parameters: { type: "object", additionalProperties: false, required: ["title", "url", "idempotencyKey"], properties: {
      title: { type: "string", minLength: 1, maxLength: 16384 }, url: { type: "string", minLength: 1, maxLength: 8192 },
      description: { type: "string", maxLength: 16384 }, thumbnailUrl: { type: "string", minLength: 1, maxLength: 8192, description: "Public HTTP(S) image URL advertised as the card thumbnail; no upload or page scraping." },
      idempotencyKey: { type: "string", minLength: 1, maxLength: 200, description: "Stable unique key for this logical share. Reuse the same key and identical fields when inspecting/retrying this request." },
    } } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      if (!input || typeof input !== "object" || Array.isArray(input)) throw Error("Expected share arguments");
      const args = input as Record<string, unknown>;
      if (Object.keys(args).some(key => !fields.includes(key))) throw Error("Share sending is restricted to the current conversation");
      const key = text(args.idempotencyKey, "idempotency key", 200);
      const request: ShareSendRequest = { kind: "share", threadId: conversation, title: text(args.title, "title", 16384, true), url: url(args.url, "URL"),
        ...(args.description !== undefined ? { description: args.description === "" ? "" : text(args.description, "description", 16384, true) } : {}),
        ...(args.thumbnailUrl !== undefined ? { thumbnailUrl: url(args.thumbnailUrl, "thumbnail URL") } : {}) };
      const guard = () => {
        signal?.throwIfAborted(); context.assertInvocationCurrent?.();
        const cfg = currentConfig(), account = resolveWeChatAccount(cfg as Record<string, unknown>);
        if (!account?.enabled || !account.native || account.accountId !== selected.accountId || account.native.accountId !== selected.native!.accountId
          || nativeConfigurationKey(account.native) !== configurationKey) throw Error("WeChat account configuration changed; start a new turn");
        return { cfg: cfg!, account, native: account.native };
      };
      const core = dependencies.core ?? getWeChatRuntime();
      const authorize = async () => {
        const { cfg, account } = guard();
        const result = await resolveWeChatIngress({ resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
          cfg, accountId: account.accountId, policy: resolveWeChatPolicyContext({ account, cfg, chatId: conversation }),
          isGroup: conversation.endsWith("@chatroom"), chatId: conversation, senderId: requester,
          readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
        guard();
        if (result.ingress.admission !== "dispatch") throw Error("WeChat conversation access is no longer allowed");
      };
      await authorize();
      const { native } = guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(native);
      await authorize();
      let intent;
      try { intent = await client.sendShare(native.accountId, request, key, signal); }
      catch (error) {
        if (!(error instanceof NativeApiError) || error.status >= 500 || error.code === "timeout") {
          const details = { threadId: conversation, idempotencyKey: key, state: "submission_unknown", note: "Inspect the send by this idempotency key. Do not resend with a new key." };
          return { content: [{ type: "text", text: JSON.stringify(details) }], details, isError: true };
        }
        throw error;
      }
      if (intent.accountId !== native.accountId || intent.destination !== conversation || !intent.id) throw Error("Share receipt crosses an account or conversation boundary; inspect the original idempotency key before another send");
      const delivery = intent.state === "accepted" && intent.localMessageId && intent.serverId
        ? await nativeOutboundDelivery(client, native.accountId, conversation, [intent.localMessageId], { sendId: intent.id, state: intent.state, serverIds: [intent.serverId] }) : undefined;
      const details = { id: intent.id, threadId: conversation, idempotencyKey: key, state: intent.state, serverId: intent.serverId,
        messageId: delivery?.messageId || undefined, receiptState: delivery?.meta.receiptState, reason: intent.errorCode,
        note: ["accepted", "sent", "server_accepted"].includes(intent.state) ? "Server accepted; recipient rendering is not confirmed." : "Inspect this send ID or reuse the identical request/key. Do not send again with a new key." };
      return { content: [{ type: "text", text: JSON.stringify(details) }], details, isError: ["failed", "needs_review", "cancelled"].includes(intent.state) };
    },
  }];
}
export function registerNativeShareTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeShareTools(context) }, { names: [name] });
}
