import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { isNativeTombstone, type NativeMessage } from "@agent-wechat/shared";
import { resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount } from "./types.ts";

const threadSchema = { type: "string", minLength: 1, maxLength: 4096, description: "WeChat thread ID. Defaults to this conversation. Owners outside groups can choose another thread." };
const pageSchema = { limit: { type: "integer", minimum: 1, maximum: 50, default: 20 }, cursor: { type: "string", maxLength: 8192, description: "Opaque nextCursor from the same tool and query; do not invent one." } };
const names = ["wechat_chats", "wechat_history", "wechat_search", "wechat_message"];
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw Error("Expected tool arguments");
  return value as Record<string, unknown>;
}
function string(value: unknown, name: string, max = 4096): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || /[\u0000-\u001f\u007f]/.test(value)) throw Error(`Invalid ${name}`);
  return value.trim();
}
function threadId(value: unknown): string {
  return string(value, "thread ID").replace(/^(?:agent-)?wechat:(?:group:)?/i, "");
}
function messageResult(message: NativeMessage) {
  const kind = message.type === 3 ? "image" : message.type === 47 ? "sticker" : message.type === 34 ? "voice" : message.type === 43 ? "video" : message.type === 49 ? "file" : "unsupported";
  const body = message.type === 1 ? message.body ?? "" : `[WeChat ${kind} message; attachment content has not been retrieved by this tool]`;
  return { id: message.id, threadId: message.threadId, serverId: message.serverId, senderId: message.senderId,
    direction: message.direction, type: message.type, createdAt: message.createdAt, body: body.slice(0, 4000), bodyTruncated: body.length > 4000 };
}

/** These read-only tools use the profile API, independently of the intake buffer.
 * Group turns are always confined to their current thread, including owners.
 * Cross-thread lookup requires the host's trusted owner bit outside a group. */
export function createNativeHistoryTools(context: OpenClawPluginToolContext, dependencies: {
  host?: typeof ensureNativeHost; core?: any;
} = {}): ChannelAgentTool[] {
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!currentConfig()) return [];
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.native || !selected.enabled) return [];
  const inWeChat = context.messageChannel === "agent-wechat";
  let currentThread: string | undefined;
  if (inWeChat) {
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
    } catch { return []; }
  }
  if (inWeChat && !currentThread) return [];
  const crossThread = context.senderIsOwner === true && !currentThread?.endsWith("@chatroom");
  if (!crossThread && (!currentThread || !context.requesterSenderId)) return [];
  const selectedId = selected.native.accountId;
  const tool = (name: string, description: string, properties: Record<string, unknown>, required: string[] = []): ChannelAgentTool => ({
    name, label: name.replaceAll("_", " "), description,
    parameters: { type: "object", additionalProperties: false, properties, required } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      const args = object(input);
      const guard = () => { signal?.throwIfAborted(); context.assertInvocationCurrent?.(); };
      guard();
      const cfg = currentConfig(); const account = resolveWeChatAccount(cfg as Record<string, unknown>);
      if (!account?.enabled || !account.native || account.native.accountId !== selectedId || account.accountId !== selected.accountId) throw Error("WeChat account configuration changed; start a new turn");
      const limit = args.limit ?? 20;
      if (!Number.isInteger(limit) || (limit as number) < 1 || (limit as number) > 50) throw Error("limit must be 1–50");
      const cursor = args.cursor === undefined ? undefined : string(args.cursor, "cursor", 8192);
      const query = args.query === undefined ? undefined : string(args.query, "query", 256);
      if (query && Buffer.byteLength(query) > 256) throw Error("Search query exceeds 256 UTF-8 bytes");
      const target = args.threadId === undefined ? currentThread : threadId(args.threadId);
      if (!crossThread && target !== currentThread) throw Error("WeChat history is restricted to the current conversation");
      if (name !== "wechat_chats" && !target) throw Error("Specify a threadId; use wechat_chats to find one");
      if (name === "wechat_search" && !query) throw Error("A non-empty search query is required");
      const core = dependencies.core ?? getWeChatRuntime();
      const allowed = async (thread: string, sender: string | undefined): Promise<boolean> => {
        if (crossThread) return true;
        const result = await resolveWeChatIngress({ resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
          cfg: cfg!, accountId: account.accountId, policy: resolveWeChatPolicyContext({ account, cfg: cfg!, chatId: thread }),
          isGroup: thread.endsWith("@chatroom"), chatId: thread, senderId: sender,
          readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
        return result.ingress.admission === "dispatch";
      };
      if (!crossThread && !await allowed(currentThread!, context.requesterSenderId)) throw Error("WeChat conversation access is no longer allowed");
      guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(account.native);
      const accountId = account.native.accountId;
      let result: unknown;
      if (name === "wechat_chats") {
        if (crossThread) result = await client.threads(accountId, { limit: limit as number, cursor, query });
        else result = { items: [await client.thread(accountId, currentThread!)], nextCursor: null, scope: "current_conversation" };
      } else if (name === "wechat_message") {
        const message = await client.message(accountId, string(args.messageId, "message ID"));
        if (message.accountId !== accountId || message.threadId !== target || (!crossThread && message.direction !== "outgoing" && !await allowed(target!, message.senderId ?? undefined))) throw Error("Message is outside the permitted conversation or sender policy");
        result = messageResult(message);
      } else {
        const page = await client.messages(accountId, target!, { limit: limit as number, cursor, ...(name === "wechat_search" ? { query } : {}) });
        const items = [];
        for (const message of page.items) {
          if (isNativeTombstone(message)) continue;
          if (message.threadId !== target || message.accountId !== accountId) throw Error("WeChat history response crosses a conversation boundary");
          if (message.direction === "outgoing" || await allowed(target!, message.senderId ?? undefined)) items.push(messageResult(message));
        }
        result = { ...page, items, scope: "local_profile", searchMode: name === "wechat_search" ? "case_insensitive_substring" : undefined,
          note: "Local history may be partial. A filtered page can be empty with a nextCursor. These reads do not mark a conversation read or download attachments." };
      }
      guard();
      return { content: [{ type: "text", text: JSON.stringify(result) }], details: result };
    },
  });
  return [
    tool(names[0], "Find locally known WeChat conversations by name or ID. Group/non-owner turns see only the current conversation. Use returned thread IDs with wechat_history or wechat_search.", { query: { type: "string", minLength: 1, maxLength: 256 }, ...pageSchema }),
    tool(names[1], "Read locally stored WeChat messages, newest first, beyond the recent inbound buffer. Follow nextCursor for older history; no remote history backfill or read marking.", { threadId: threadSchema, ...pageSchema }),
    tool(names[2], "Search a WeChat conversation's locally stored message text by case-insensitive substring. This searches stored history beyond buffered messages. Follow nextCursor with the same query/thread.", { threadId: threadSchema, query: { type: "string", minLength: 1, maxLength: 256 }, ...pageSchema }, ["query"]),
    tool(names[3], "Look up a stored WeChat message by its exact opaque message ID within a permitted conversation. Media is described without downloading it; long text is bounded.", { threadId: threadSchema, messageId: { type: "string", minLength: 1, maxLength: 4096 } }, ["messageId"]),
  ];
}
export function registerNativeHistoryTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeHistoryTools(context) }, { names });
}
