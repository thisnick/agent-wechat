import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { imageResultFromFile } from "openclaw/plugin-sdk/channel-actions";
import { isNativeTombstone, NativeApiError, type NativeMessage } from "@agent-wechat/shared";
import { resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { prepareNativeAttachment } from "./native-media.ts";
import { renderEmbeddedQuote } from "./native-quote.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeRecallInfo } from "./native-recall-state.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";

const pageSchema = { limit: { type: "integer", minimum: 1, maximum: 50, default: 20 }, cursor: { type: "string", maxLength: 8192, description: "Opaque nextCursor from the same tool and query; do not invent one." } };
const names = ["wechat_history", "wechat_search", "wechat_message", "wechat_media"];
const archiveNames = ["wechat_archive_threads", "wechat_archive_search", "wechat_archive_history", "wechat_archive_message", "wechat_archive_media"];
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
function messageResult(message: NativeMessage, retrievalTool = "wechat_media", quotePermitted = false) {
  if (nativeRecallInfo(message)) return { id: message.id, threadId: message.threadId, serverId: message.serverId,
    direction: message.direction, type: 10000, body: "[Message recalled]", status: "recalled", recall: nativeRecallInfo(message) };
  const isQuote = message.type === 49 && message.appMessageType === 57;
  const isForward = message.type === 49 && message.appMessageType === 19;
  const isLink = message.type === 49 && [3, 4, 5].includes(message.appMessageType ?? 0) && !!message.link;
  const kind = isQuote ? "quote" : isForward ? "forwarded_history" : isLink ? "link_preview" : message.type === 3 ? "image" : message.type === 47 ? "sticker" : message.type === 34 ? "voice" : message.type === 43 ? "video" : message.type === 49 && (message.appMessageType == null || message.appMessageType === 6) ? "file" : "unsupported";
  const body = message.type === 1 || isForward || isLink ? message.body ?? "" : isQuote ? renderEmbeddedQuote(message, quotePermitted).body : `[WeChat ${kind} message; attachment content has not been retrieved by this tool]`;
  const quote = message.quote && { serverId: message.quote.serverId, type: message.quote.type, state: quotePermitted ? message.quote.state : "not permitted",
    senderId: message.quote.senderId, appMessageType: message.quote.appMessageType, embeddedImage: quotePermitted ? message.quote.embeddedImage : undefined };
  return { id: message.id, threadId: message.threadId, serverId: message.serverId, senderId: message.senderId,
    direction: message.direction, type: message.type, appMessageType: message.appMessageType, quote, link: isLink ? message.link : undefined, createdAt: message.createdAt, body: body.slice(0, 4000), bodyTruncated: body.length > 4000,
    attachment: message.type !== 1 && !isForward && kind !== "unsupported" ? { kind, retrievalTool, messageId: message.id } : undefined };
}

type Dependencies = { host?: typeof ensureNativeHost; core?: any };

/** These lookup tools use the profile API, independently of the intake buffer.
 * Every lookup is confined to the host's trusted current WeChat conversation.
 * Owner status does not widen access or bypass sender admission. */
export function createNativeHistoryTools(context: OpenClawPluginToolContext, dependencies: Dependencies = {}): ChannelAgentTool[] {
  return createLookupTools(context, dependencies, false);
}

/** Optional account-wide tools. OpenClaw's tool policies authorize invocation;
 * these tools deliberately apply no owner, current-thread or sender filter. */
export function createNativeArchiveTools(context: OpenClawPluginToolContext, dependencies: Dependencies = {}): ChannelAgentTool[] {
  return createLookupTools(context, dependencies, true);
}

function createLookupTools(context: OpenClawPluginToolContext, dependencies: Dependencies, archive: boolean): ChannelAgentTool[] {
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!currentConfig()) return [];
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.native || !selected.enabled) return [];
  const configurationKey = nativeConfigurationKey(selected.native);
  if (!archive && context.messageChannel !== "agent-wechat") return [];
  if (!archive && context.agentAccountId && context.agentAccountId !== selected.accountId) return [];
  if (!archive && context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId) return [];
  let currentThread: string | undefined;
  let requesterSenderId: string | undefined;
  // Released hosts expose the trusted current route through deliveryContext
  // even when a channel does not populate NativeChannelId in its SDK context.
  const target = context.deliveryContext?.channel === "agent-wechat" ? context.deliveryContext.to : undefined;
  if (!archive) {
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
  }
  const selectedId = selected.native.accountId;
  type Action = "history" | "search" | "message" | "media" | "threads";
  const tool = (name: string, action: Action, description: string, properties: Record<string, unknown>, required: string[] = []): ChannelAgentTool => ({
    name, label: name.replaceAll("_", " "), description,
    // The pinned host's tool_call wrapper JSON-serializes image blocks. Its
    // public direct-only surface preserves actual image content for vision.
    ...(action === "media" ? { catalogMode: "direct-only" as const } : {}),
    parameters: { type: "object", additionalProperties: false, properties, required } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      const args = object(input);
      const guard = () => {
        signal?.throwIfAborted(); context.assertInvocationCurrent?.();
        const cfg = currentConfig(); const account = resolveWeChatAccount(cfg as Record<string, unknown>);
        if (!account?.enabled || !account.native || account.native.accountId !== selectedId || account.accountId !== selected.accountId
          || nativeConfigurationKey(account.native) !== configurationKey) throw Error("WeChat account configuration changed; start a new turn");
        return { cfg: cfg!, account, native: account.native };
      };
      const { cfg, account, native } = guard();
      if (Object.keys(args).some(key => !Object.hasOwn(properties, key))) throw Error(archive ? "Unknown archive lookup argument" : "Unknown lookup argument; WeChat history is restricted to the current conversation");
      const limit = args.limit ?? 20;
      if (!Number.isInteger(limit) || (limit as number) < 1 || (limit as number) > 50) throw Error("limit must be 1–50");
      const cursor = args.cursor === undefined ? undefined : string(args.cursor, "cursor", 8192);
      const query = args.query === undefined ? undefined : string(args.query, "query", 256);
      if (query && Buffer.byteLength(query) > 256) throw Error("Search query exceeds 256 UTF-8 bytes");
      if (action === "search" && !query) throw Error("A non-empty search query is required");
      const conversation = archive ? (args.threadId === undefined ? undefined : threadId(args.threadId)) : currentThread;
      if (action === "history" && !conversation) throw Error("A thread ID is required");
      const messageId = Object.hasOwn(properties, "messageId") ? string(args.messageId, "message ID") : undefined;
      if (args.download !== undefined && typeof args.download !== "boolean") throw Error("download must be a boolean");
      const quality = args.quality ?? "best";
      if (!qualities.includes(quality as typeof qualities[number])) throw Error("Invalid media quality");
      const core = dependencies.core ?? getWeChatRuntime();
      const allowed = async (sender: string | undefined): Promise<boolean> => {
        if (archive) return true;
        const { cfg, account } = guard();
        const result = await resolveWeChatIngress({ resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
          cfg: cfg!, accountId: account.accountId, policy: resolveWeChatPolicyContext({ account, cfg: cfg!, chatId: conversation! }),
          isGroup: conversation!.endsWith("@chatroom"), chatId: conversation!, senderId: sender,
          readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
        return result.ingress.admission === "dispatch";
      };
      if (!await allowed(requesterSenderId)) throw Error("WeChat conversation access is no longer allowed");
      guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(native);
      guard();
      const accountId = native.accountId;
      const visibleMessage = async (message: NativeMessage) => messageResult(message, archive ? "wechat_archive_media" : "wechat_media",
        !message.quote || archive || message.quote.direction === "outgoing" || !!message.quote.senderId && await allowed(message.quote.senderId));
      let result: unknown;
      if (action === "threads") {
        const page = await client.threads(accountId, { limit: limit as number, cursor, ...(query ? { query } : {}) });
        const items = [];
        for (const thread of page.items) {
          if (isNativeTombstone(thread)) continue;
          if (thread.accountId !== accountId) throw Error("Thread response crosses an account boundary");
          items.push({ id: thread.id, peerId: thread.peerId, kind: thread.kind, displayName: thread.displayName, unreadCount: thread.unreadCount });
        }
        result = { ...page, items, scope: "local_profile", note: "Locally stored threads; coverage may be partial. This lookup does not mark threads read." };
      } else if (action === "message" || action === "media") {
        const message = await client.message(accountId, messageId!);
        guard();
        if (!await allowed(requesterSenderId)) throw Error("WeChat conversation access is no longer allowed");
        if (message.id !== messageId || message.accountId !== accountId || (!archive && message.threadId !== conversation) || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("Message is outside the permitted conversation or sender policy");
        if (action === "message") result = await visibleMessage(message);
        if (action === "media") {
          const checkRecall = async () => {
            guard(); const latest = await client.message(accountId, messageId!); guard();
            if (latest.id !== message.id || latest.accountId !== accountId || latest.threadId !== message.threadId || latest.serverId !== message.serverId) throw Error("Native media message identity changed");
            if (nativeRecallInfo(latest)) throw new NativeApiError(410, "message_recalled", false);
            if (message.quote) {
              if (latest.quote?.state === "recalled") throw new NativeApiError(410, "message_recalled", false);
              if (!latest.quote || JSON.stringify(latest.quote) !== JSON.stringify(message.quote)) throw new NativeApiError(409, "media_source_changed", false);
            }
          };
          let prepared: Awaited<ReturnType<typeof prepareNativeAttachment>>;
          try { prepared = await prepareNativeAttachment({ client, account, message, core, cfg,
            quality: quality as typeof qualities[number], download: args.download !== false, waitMs: 2000, signal, assertCurrent: guard,
            allowQuotedMessage: async original => original.direction === "outgoing" || await allowed(original.senderId ?? undefined),
            beforeMediaAccess: async () => {
              if (!await allowed(requesterSenderId) || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("WeChat conversation access is no longer allowed");
              await checkRecall();
            } }); }
          catch (error) {
            guard();
            if (!(error instanceof NativeApiError)) throw error;
            const details = { messageId: message.id, threadId: message.threadId,
              status: error.code === "message_recalled" ? "recalled" : error.retryable ? "pending" : ["unsupported_profile", "unsupported_media", "unsupported_media_variant"].includes(error.code) ? "unsupported" : "unavailable",
              retryable: error.retryable, reason: error.code };
            return { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
          }
          guard();
          if (!await allowed(requesterSenderId) || (message.direction !== "outgoing" && !await allowed(message.senderId ?? undefined))) throw Error("WeChat conversation access is no longer allowed");
          const details = { messageId: message.id, threadId: message.threadId, status: prepared.reason === "message_recalled" ? "recalled" : prepared.pending ? "pending" : prepared.attachment?.status ?? prepared.quote?.state ?? "unsupported",
            attachment: prepared.attachment, quote: prepared.quote, link: ["message_recalled", "media_identity_unavailable", "media_source_changed"].includes(prepared.reason ?? "") ? undefined : message.link,
            variant: prepared.variant, jobId: prepared.jobId, retryable: prepared.retryable ?? false, reason: prepared.reason, body: prepared.body };
          const image = prepared.suppliedAssets?.find(asset => asset.role === "model_image");
          const output = image ? await imageResultFromFile({ label: "WeChat attachment", path: image.path, extraText: JSON.stringify(details), details })
            : { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
          guard();
          try { await checkRecall(); } catch (error) {
            if (!(error instanceof NativeApiError) || error.code !== "message_recalled") throw error;
            const details = { messageId: message.id, threadId: message.threadId, status: "recalled", reason: "message_recalled", retryable: false };
            return { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
          }
          return output;
        }
      } else {
        const options = { limit: limit as number, cursor, ...(action === "search" ? { query } : {}) };
        const page = conversation ? await client.messages(accountId, conversation, options) : await client.searchMessages(accountId, options);
        const items = [];
        for (const message of page.items) {
          if (isNativeTombstone(message)) continue;
          if ((conversation && message.threadId !== conversation) || message.accountId !== accountId) throw Error("WeChat history response crosses a conversation boundary");
          if (message.direction === "outgoing" || await allowed(message.senderId ?? undefined)) items.push(await visibleMessage(message));
        }
        result = { ...page, items, scope: "local_profile", searchMode: action === "search" ? "case_insensitive_substring" : undefined,
          note: archive ? "Account-wide local history may be partial. These reads do not mark threads read or download attachments." : "Local history may be partial. A filtered page can be empty with a nextCursor. These reads do not mark a conversation read or download attachments." };
      }
      guard();
      return { content: [{ type: "text", text: JSON.stringify(result) }], details: result };
    },
  });
  const querySchema = { type: "string", minLength: 1, maxLength: 256 };
  const mediaSchema = {
    messageId: messageIdSchema,
    download: { type: "boolean", default: true, description: "Set false to use cached media only without requesting a WeChat transfer." },
    quality: { type: "string", enum: qualities, default: "best", description: "best uses the best cached image, normal video or decoded voice. Explicit variants are strict and may be unavailable." },
  };
  if (archive) return [
    tool(archiveNames[0], "threads", "List or find locally stored threads across the configured WeChat account. Optional case-insensitive query matches thread IDs and display names. Includes groups and peers, with opaque pagination; no read marking.", { query: querySchema, ...pageSchema }),
    tool(archiveNames[1], "search", "Search locally stored messages across every thread in the configured WeChat account, including other senders. Optional threadId narrows the search. Follow nextCursor with the same query and thread selection; no remote backfill or read marking.", { query: querySchema, threadId: messageIdSchema, ...pageSchema }, ["query"]),
    tool(archiveNames[2], "history", "Read the locally stored history of any thread in the configured WeChat account, newest first, without sender filtering or read marking. Use wechat_archive_threads to find thread IDs.", { threadId: messageIdSchema, ...pageSchema }, ["threadId"]),
    tool(archiveNames[3], "message", "Look up a stored message in any thread of the configured WeChat account by its exact opaque message ID. Use wechat_archive_media to retrieve its attachment.", { messageId: messageIdSchema }, ["messageId"]),
    tool(archiveNames[4], "media", "Retrieve an attachment or quoted text/image/link from any thread in the configured WeChat account by its message ID. Use the parent quote message ID for quotes. Returns verified managed file paths and an image preview for vision. Cached bytes first; missing media requests a bounded server-owned download unless download=false. Preserve pending/expired/unavailable results and never automatically retry retryable=false. No sends or read marking.", mediaSchema, ["messageId"]),
  ];
  return [
    tool(names[0], "history", "Read the current WeChat conversation's locally stored messages, newest first, beyond the recent inbound buffer. Follow nextCursor for older history; no remote history backfill or read marking.", { ...pageSchema }),
    tool(names[1], "search", "Search the current WeChat conversation's locally stored message text by case-insensitive substring, beyond buffered messages. Follow nextCursor with the same query in this conversation.", { query: querySchema, ...pageSchema }, ["query"]),
    tool(names[2], "message", "Look up a stored message by its exact opaque message ID in the current WeChat conversation. Media is described without downloading it; use wechat_media to retrieve its attachment. Long text is bounded.", { messageId: messageIdSchema }, ["messageId"]),
    tool(names[3], "media", "Retrieve a message attachment or quoted text/image/link from the current WeChat conversation. Use the parent quote message ID for quotes. Returns a verified local file path and an image preview for vision when applicable. Uses cached bytes first; download defaults to true and requests a bounded server-owned transfer if missing. Pending results may be checked again; never automatically retry confirmed expiry or retryable=false. Does not send messages or mark the thread read.", mediaSchema, ["messageId"]),
  ];
}
export function registerNativeHistoryTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeHistoryTools(context) }, { names });
  api.registerTool({ contextVersion: 2, create: context => createNativeArchiveTools(context) }, { names: archiveNames, optional: true });
}
