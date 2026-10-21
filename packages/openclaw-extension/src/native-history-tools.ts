import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { imageResultFromFile } from "openclaw/plugin-sdk/channel-actions";
import { isNativeTombstone, NativeApiError, type NativeMessage } from "@agent-wechat/shared";
import { resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { prepareNativeAttachment } from "./native-media.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount } from "./types.ts";

const pageSchema = { limit: { type: "integer", minimum: 1, maximum: 50, default: 20 }, cursor: { type: "string", maxLength: 8192, description: "Opaque nextCursor from the same tool and query; do not invent one." } };
const names = ["wechat_history", "wechat_search", "wechat_message", "wechat_media"];
const messageIdSchema = { type: "string", minLength: 1, maxLength: 4096 };
const qualities = ["best", "original", "standard", "thumbnail", "normal", "encoded"] as const;
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw Error("Expected tool arguments");
  return value as Record<string, unknown>;
}
function string(value: unknown, name: string, max = 4096): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || /[\u0000-\u001f\u007f]/.test(value)) throw Error(`Invalid ${name}`);
  return value.trim();
}
function threadId(value: unknown): string {
  return string(string(value, "thread ID").replace(/^(?:agent-)?wechat:(?:group:)?/i, ""), "thread ID");
}
function messageResult(message: NativeMessage) {
  const kind = message.type === 3 ? "image" : message.type === 47 ? "sticker" : message.type === 34 ? "voice" : message.type === 43 ? "video" : message.type === 49 ? "file" : "unsupported";
  const body = message.type === 1 ? message.body ?? "" : `[WeChat ${kind} message; attachment content has not been retrieved by this tool]`;
  return { id: message.id, threadId: message.threadId, serverId: message.serverId, senderId: message.senderId,
    direction: message.direction, type: message.type, createdAt: message.createdAt, body: body.slice(0, 4000), bodyTruncated: body.length > 4000,
    attachment: message.type !== 1 && kind !== "unsupported" ? { kind, retrievalTool: "wechat_media", messageId: message.id } : undefined };
}

/** These lookup tools use the profile API, independently of the intake buffer.
 * Every lookup is confined to the host's trusted current WeChat conversation.
 * Owner status does not widen access or bypass sender admission. */
export function createNativeHistoryTools(context: OpenClawPluginToolContext, dependencies: {
  host?: typeof ensureNativeHost; core?: any;
} = {}): ChannelAgentTool[] {
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!currentConfig()) return [];
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.native || !selected.enabled) return [];
  if (context.messageChannel !== "agent-wechat") return [];
  if (context.agentAccountId && context.agentAccountId !== selected.accountId) return [];
  if (context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId) return [];
  let currentThread: string | undefined;
  let requesterSenderId: string;
  // Released hosts expose the trusted current route through deliveryContext
  // even when a channel does not populate NativeChannelId in its SDK context.
  const target = context.deliveryContext?.channel === "agent-wechat" ? context.deliveryContext.to : undefined;
  try {
    if (context.nativeChannelId) currentThread = threadId(context.nativeChannelId);
    if (target) {
      const deliveryThread = threadId(target);
      if (currentThread && currentThread !== deliveryThread) return [];
      currentThread ??= deliveryThread;
    }
    requesterSenderId = string(context.requesterSenderId, "requester sender ID");
  } catch { return []; }
  if (!currentThread) return [];
  const conversation = currentThread;
  const selectedId = selected.native.accountId;
  const tool = (name: string, description: string, properties: Record<string, unknown>, required: string[] = []): ChannelAgentTool => ({
    name, label: name.replaceAll("_", " "), description,
    // The pinned host's tool_call wrapper JSON-serializes image blocks. Its
    // public direct-only surface preserves actual image content for vision.
    ...(name === "wechat_media" ? { catalogMode: "direct-only" as const } : {}),
    parameters: { type: "object", additionalProperties: false, properties, required } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      const args = object(input);
      const guard = () => {
        signal?.throwIfAborted(); context.assertInvocationCurrent?.();
        const cfg = currentConfig(); const account = resolveWeChatAccount(cfg as Record<string, unknown>);
        if (!account?.enabled || !account.native || account.native.accountId !== selectedId || account.accountId !== selected.accountId) throw Error("WeChat account configuration changed; start a new turn");
        return { cfg: cfg!, account, native: account.native };
      };
      const { cfg, account, native } = guard();
      if (Object.keys(args).some(key => !Object.hasOwn(properties, key))) throw Error("Unknown lookup argument; WeChat history is restricted to the current conversation");
      const limit = args.limit ?? 20;
      if (!Number.isInteger(limit) || (limit as number) < 1 || (limit as number) > 50) throw Error("limit must be 1–50");
      const cursor = args.cursor === undefined ? undefined : string(args.cursor, "cursor", 8192);
      const query = args.query === undefined ? undefined : string(args.query, "query", 256);
      if (query && Buffer.byteLength(query) > 256) throw Error("Search query exceeds 256 UTF-8 bytes");
      if (name === "wechat_search" && !query) throw Error("A non-empty search query is required");
      const messageId = Object.hasOwn(properties, "messageId") ? string(args.messageId, "message ID") : undefined;
      if (args.download !== undefined && typeof args.download !== "boolean") throw Error("download must be a boolean");
      const quality = args.quality ?? "best";
      if (!qualities.includes(quality as typeof qualities[number])) throw Error("Invalid media quality");
      const core = dependencies.core ?? getWeChatRuntime();
      const allowed = async (sender: string | undefined): Promise<boolean> => {
        const { cfg, account } = guard();
        const result = await resolveWeChatIngress({ resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
          cfg: cfg!, accountId: account.accountId, policy: resolveWeChatPolicyContext({ account, cfg: cfg!, chatId: conversation }),
          isGroup: conversation.endsWith("@chatroom"), chatId: conversation, senderId: sender,
          readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
        return result.ingress.admission === "dispatch";
      };
      if (!await allowed(requesterSenderId)) throw Error("WeChat conversation access is no longer allowed");
      guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(native);
      guard();
      const accountId = native.accountId;
      let result: unknown;
      if (name === "wechat_message" || name === "wechat_media") {
        const message = await client.message(accountId, messageId!);
        guard();
        if (!await allowed(requesterSenderId)) throw Error("WeChat conversation access is no longer allowed");
        if (message.id !== messageId || message.accountId !== accountId || message.threadId !== conversation || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("Message is outside the permitted conversation or sender policy");
        result = messageResult(message);
        if (name === "wechat_media") {
          let prepared: Awaited<ReturnType<typeof prepareNativeAttachment>>;
          try { prepared = await prepareNativeAttachment({ client, account, message, core, cfg,
            quality: quality as typeof qualities[number], download: args.download !== false, waitMs: 2000, signal, assertCurrent: guard,
            beforeMediaAccess: async () => {
              if (!await allowed(requesterSenderId) || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("WeChat conversation access is no longer allowed");
            } }); }
          catch (error) {
            guard();
            if (!(error instanceof NativeApiError)) throw error;
            const details = { messageId: message.id, threadId: conversation,
              status: error.retryable ? "pending" : ["unsupported_profile", "unsupported_media", "unsupported_media_variant"].includes(error.code) ? "unsupported" : "unavailable",
              retryable: error.retryable, reason: error.code };
            return { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
          }
          guard();
          if (!await allowed(requesterSenderId) || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("WeChat conversation access is no longer allowed");
          const details = { messageId: message.id, threadId: conversation, status: prepared.pending ? "pending" : prepared.attachment?.status ?? "unsupported",
            attachment: prepared.attachment, variant: prepared.variant, jobId: prepared.jobId, retryable: prepared.retryable ?? false, reason: prepared.reason, body: prepared.body };
          const image = prepared.suppliedAssets?.find(asset => asset.role === "model_image");
          const output = image ? await imageResultFromFile({ label: "WeChat attachment", path: image.path, extraText: JSON.stringify(details), details })
            : { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
          guard();
          return output;
        }
      } else {
        const page = await client.messages(accountId, conversation, { limit: limit as number, cursor, ...(name === "wechat_search" ? { query } : {}) });
        const items = [];
        for (const message of page.items) {
          if (isNativeTombstone(message)) continue;
          if (message.threadId !== conversation || message.accountId !== accountId) throw Error("WeChat history response crosses a conversation boundary");
          if (message.direction === "outgoing" || await allowed(message.senderId ?? undefined)) items.push(messageResult(message));
        }
        result = { ...page, items, scope: "local_profile", searchMode: name === "wechat_search" ? "case_insensitive_substring" : undefined,
          note: "Local history may be partial. A filtered page can be empty with a nextCursor. These reads do not mark a conversation read or download attachments." };
      }
      guard();
      return { content: [{ type: "text", text: JSON.stringify(result) }], details: result };
    },
  });
  return [
    tool(names[0], "Read the current WeChat conversation's locally stored messages, newest first, beyond the recent inbound buffer. Follow nextCursor for older history; no remote history backfill or read marking.", { ...pageSchema }),
    tool(names[1], "Search the current WeChat conversation's locally stored message text by case-insensitive substring, beyond buffered messages. Follow nextCursor with the same query in this conversation.", { query: { type: "string", minLength: 1, maxLength: 256 }, ...pageSchema }, ["query"]),
    tool(names[2], "Look up a stored message by its exact opaque message ID in the current WeChat conversation. Media is described without downloading it; use wechat_media to retrieve its attachment. Long text is bounded.", { messageId: messageIdSchema }, ["messageId"]),
    tool(names[3], "Retrieve a message attachment from the current WeChat conversation. Returns a verified local file path and an image preview for vision when applicable. Uses cached bytes first; download defaults to true and requests a bounded server-owned transfer if missing. Pending results may be checked again; never automatically retry confirmed expiry or retryable=false. Does not send messages or mark the thread read.", {
      messageId: messageIdSchema,
      download: { type: "boolean", default: true, description: "Set false to use cached media only without requesting a WeChat transfer." },
      quality: { type: "string", enum: qualities, default: "best", description: "best uses the best cached image, normal video or decoded voice. Explicit variants are strict and may be unavailable." },
    }, ["messageId"]),
  ];
}
export function registerNativeHistoryTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeHistoryTools(context) }, { names });
}
