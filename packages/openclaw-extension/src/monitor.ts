import { WeChatClient } from "@agent-wechat/shared";
import type { Chat, Message, MediaResult, AuthStatus } from "@agent-wechat/shared";
import { createChannelReplyPipeline } from "openclaw/plugin-sdk/channel-reply-pipeline";
import type { ResolvedWeChatAccount } from "./types.js";
import { getWeChatRuntime } from "./runtime.js";
import { resolveWeChatAccount } from "./types.js";
import {
  applyCatchupAttachmentPolicy,
  applyLiveAttachment,
  attachmentKindForMessageType,
  attachmentKindForResult,
  buildMediaSegments,
  mediaMime,
  type WeChatAttachment,
} from "./media-delivery.js";
import {
  AttachmentTooLargeError,
  attachmentPathExists,
  saveManagedAttachment,
} from "./attachment-store.js";
import { MonitorStateStore } from "./monitor-state.js";
import { listAllChats, listMessageWindow } from "./polling.js";
import {
  normalizeWeChatCommandBody,
  resolveWeChatCommandAuthorization,
  resolveWeChatInboundAccessDecision,
  resolveWeChatMentionGate,
  resolveWeChatPolicyContext,
  type WeChatPolicyContext,
} from "./access-control.js";

// History context markers (match openclaw's built-in markers)
const HISTORY_CONTEXT_MARKER = "[Chat messages since your last reply - for context]";
const CURRENT_MESSAGE_MARKER = "[Current message - respond to this]";

export interface WeChatMonitorOptions {
  account: ResolvedWeChatAccount;
  abortSignal: AbortSignal;
  runtime: any; // PluginRuntime
  setStatus: (next: any) => void;
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void };
  cfg: any; // OpenClawConfig
}

type ProcessedMessage = {
  msg: Message;
  rawBody: string;
  commandBody: string;
  mediaPath?: string;
  mediaMime?: string;
  senderName: string;
  senderId: string;
  isGroup: boolean;
  timestamp: number;
  hasMedia: boolean;
  isMentioned: boolean;
  attachment?: WeChatAttachment;
  /** Body before generated attachment status/path annotations. */
  sourceBody: string;
};

function isProcessedMessage(value: unknown): value is ProcessedMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Partial<ProcessedMessage>;
  return !!message.msg && typeof message.rawBody === "string" &&
    typeof message.commandBody === "string" && typeof message.senderName === "string" &&
    typeof message.senderId === "string" && typeof message.isGroup === "boolean" &&
    typeof message.timestamp === "number" && typeof message.hasMedia === "boolean" &&
    typeof message.isMentioned === "boolean" && typeof message.sourceBody === "string";
}

/** Official/service accounts have IDs starting with gh_ */
function isOfficialAccount(chatId: string): boolean {
  return chatId.startsWith("gh_");
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener("abort", () => {
      clearTimeout(timer);
      resolve();
    });
  });
}

/**
 * Poll for media data, retrying until data is available or max attempts reached.
 */
async function pollMedia(
  client: WeChatClient,
  chatId: string,
  localId: number,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
  maxAttempts = 15,
  intervalMs = 1000,
): Promise<MediaResult | null> {
  let lastResult: MediaResult | null = null;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    const result = await client.getMedia(chatId, localId, "full");
    lastResult = result;
    if (result.type === "unsupported") {
      return result;
    }
    if (result.data) {
      return result;
    }
    if (attempt < maxAttempts) {
      log?.info?.(`[media] Attempt ${attempt}/${maxAttempts} for ${chatId}:${localId} returned no data, retrying...`);
      await new Promise(r => setTimeout(r, intervalMs));
    }
  }
  return lastResult;
}

function enqueueWeChatSystemEvent(text: string, contextKey: string): void {
  try {
    const core = getWeChatRuntime();
    core.system.enqueueSystemEvent(text, {
      sessionKey: "agent:main:main",
      contextKey,
    });
  } catch {
    // Don't crash the monitor if system event fails
  }
}

async function retrieveAttachment(
  client: WeChatClient,
  chatId: string,
  msg: Message,
  liveAccount: ResolvedWeChatAccount,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
  maxAttempts = 15,
): Promise<WeChatAttachment | undefined> {
  const baseType = msg.type & 0x7fffffff;
  const expectedKind = attachmentKindForMessageType(baseType);
  if (!expectedKind) return undefined;
  try {
    const result = await pollMedia(client, chatId, msg.localId, log, maxAttempts);
    if (!result) {
      return { kind: expectedKind, status: "unavailable", filename: msg.content || `message-${msg.localId}` };
    }
    if (result.type === "unsupported") {
      // Type 49 also represents links/cards; unsupported means it was not a file.
      if (baseType === 49) return undefined;
      return { kind: expectedKind, status: "unsupported", filename: msg.content || `message-${msg.localId}` };
    }
    const kind = attachmentKindForResult(result, expectedKind);
    const filename = result.filename || msg.content || `message-${msg.localId}`;
    if (!result.data) {
      return { kind, status: result.type === "pending" ? "pending" : "unavailable", filename };
    }
    const mime = mediaMime(result.format, kind);
    const saved = await saveManagedAttachment({
      data: result.data,
      mime,
      filename,
      maxBytes: liveAccount.mediaMaxMb * 1024 * 1024,
      saveMediaBuffer: getWeChatRuntime().channel.media.saveMediaBuffer,
    });
    log?.info?.(
      `[wechat:${liveAccount.accountId}] Saved ${kind} attachment for msg ${msg.localId} (${saved.size} bytes)`,
    );
    return { kind, status: "ready", filename: saved.filename, mime, path: saved.path };
  } catch (error) {
    const status = error instanceof AttachmentTooLargeError ? "too_large" : "error";
    log?.error?.(
      `[wechat:${liveAccount.accountId}] Attachment retrieval failed for msg ${msg.localId} (${status})`,
    );
    return {
      kind: expectedKind,
      status,
      filename: msg.content || `message-${msg.localId}`,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

export async function startWeChatMonitor(
  opts: WeChatMonitorOptions,
): Promise<void> {
  const { account, abortSignal, setStatus, log } = opts;
  const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
  const runtimeStateDir = getWeChatRuntime().state.resolveStateDir(process.env);
  const stateStore = new MonitorStateStore<ProcessedMessage>(runtimeStateDir, account.accountId);
  let savedState: Awaited<ReturnType<typeof stateStore.load>>;
  try {
    savedState = await stateStore.load(isProcessedMessage);
  } catch (error) {
    log?.error?.(`[wechat:${account.accountId}] Ignoring unreadable monitor state: ${String(error)}`);
    savedState = { version: 1, lastSeen: {}, groupHistory: {} };
  }
  const lastSeenId = new Map<string, number>(Object.entries(savedState.lastSeen));
  const groupHistory = new Map<string, ProcessedMessage[]>(Object.entries(savedState.groupHistory));
  const GROUP_HISTORY_LIMIT = 50;
  const persistState = async () => {
    try {
      await stateStore.save(lastSeenId, groupHistory);
    } catch (error) {
      log?.error?.(`[wechat:${account.accountId}] Failed to persist monitor state: ${String(error)}`);
    }
  };
  let lastAuthCheck = 0;
  let prevStatus: AuthStatus["status"] | undefined = undefined;

  // Report initial status as running
  setStatus({
    accountId: account.accountId,
    running: true,
    connected: true,
    linked: true,
  });

  while (!abortSignal.aborted) {
    try {
      // Read the runtime config snapshot each iteration; the host updates it on hot-reload.
      const cfg = getWeChatRuntime().config.current();

      // ---- Auth polling (every authPollIntervalMs) ----
      const now = Date.now();
      if (now - lastAuthCheck >= account.authPollIntervalMs) {
        lastAuthCheck = now;
        try {
          const auth = await client.authStatus();
          const isLinked = auth.status === "logged_in";
          setStatus({
            accountId: account.accountId,
            running: true,
            connected: true,
            linked: isLinked,
            authStatus: auth.status,
          });

          // Notify agent proactively on meaningful auth transitions
          if (prevStatus === "logged_in" && !isLinked) {
            const msg = auth.status === "app_not_running"
              ? "[WeChat] Application stopped. It will restart automatically — credentials may be cached, so you can try reconnecting using the wechat_login tool."
              : "[WeChat] Session ended. You can try reconnecting using the wechat_login tool — if credentials are cached, login may complete automatically.";
            enqueueWeChatSystemEvent(msg, "wechat:auth_lost");
          } else if (prevStatus === undefined && !isLinked) {
            enqueueWeChatSystemEvent(
              "[WeChat] Not logged in. Use the wechat_login tool to authenticate — if credentials are cached from a previous session, login may complete automatically.",
              "wechat:auth_required",
            );
          }
          prevStatus = auth.status;

          if (!isLinked) {
            log?.info?.(`[wechat:${account.accountId}] Not authenticated (status: ${auth.status})`);
            await sleep(account.pollIntervalMs, abortSignal);
            continue;
          }
        } catch (err) {
          setStatus({
            accountId: account.accountId,
            running: true,
            connected: false,
            linked: false,
            lastError: String(err),
          });
          if (prevStatus === "logged_in") {
            enqueueWeChatSystemEvent(
              "[WeChat] Cannot reach agent-wechat server. The container may have stopped.",
              "wechat:server_unreachable",
            );
          }
          prevStatus = undefined;
          log?.error?.(
            `[wechat:${account.accountId}] Auth check failed: ${err}`,
          );
          await sleep(account.pollIntervalMs, abortSignal);
          continue;
        }
      }

      // ---- Message polling ----
      let chats: Chat[];
      try {
        chats = await listAllChats(client);
      } catch (err) {
        log?.error?.(
          `[wechat:${account.accountId}] Failed to list chats: ${err}`,
        );
        await sleep(account.pollIntervalMs, abortSignal);
        continue;
      }

      // Filter to chats with unreads (skip official accounts)
      const unreadChats = chats.filter(
        (c) => c.unreadCount > 0 && !isOfficialAccount(c.username ?? c.id),
      );
      if (unreadChats.length > 0) {
        log?.info?.(
          `[wechat:${account.accountId}] ${unreadChats.length} chat(s) with unreads`,
        );
      }

      if (unreadChats.length > 0) {
        for (const chat of unreadChats) {
          if (abortSignal.aborted) break;
          await processUnreadChat(
            client,
            chat,
            lastSeenId,
            account,
            cfg,
            log,
            undefined,
            groupHistory,
            GROUP_HISTORY_LIMIT,
            persistState,
          );
        }
      }

      // ---- Catch-up: check tracked chats where lastMsgLocalId advanced past lastSeenId ----
      for (const chat of chats) {
        if (abortSignal.aborted) break;
        const chatId = chat.username ?? chat.id;
        if (isOfficialAccount(chatId)) continue; // skip official accounts
        const prevSeen = lastSeenId.get(chatId);
        if (prevSeen === undefined) continue; // not tracked yet
        if (unreadChats.some((c) => (c.username ?? c.id) === chatId)) continue; // already processed
        if (!chat.lastMsgLocalId || chat.lastMsgLocalId <= prevSeen) continue; // nothing new

        log?.info?.(
          `[wechat:${account.accountId}] Catch-up: ${chatId} lastMsgLocalId=${chat.lastMsgLocalId} > lastSeenId=${prevSeen}`,
        );
        await processUnreadChat(client, chat, lastSeenId, account, cfg, log, true, groupHistory, GROUP_HISTORY_LIMIT, persistState);
      }
    } catch (err) {
      log?.error?.(
        `[wechat:${account.accountId}] Monitor error: ${err}`,
      );
    }

    await sleep(account.pollIntervalMs, abortSignal);
  }

  setStatus({
    accountId: account.accountId,
    running: false,
    connected: false,
  });
}

/**
 * Pre-process a single message: download media, build rawBody, resolve sender info.
 */
async function prepareMessage(
  client: WeChatClient,
  msg: Message,
  chatId: string,
  chat: Chat,
  liveAccount: ResolvedWeChatAccount,
  policy: WeChatPolicyContext,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
): Promise<ProcessedMessage | null> {
  // Skip self-sent messages
  if (msg.isSelf) {
    log?.info?.(`[wechat:${liveAccount.accountId}] Skipping self-sent msg ${msg.localId}`);
    return null;
  }

  const isGroup = chatId.includes("@chatroom");
  const senderId = msg.sender ?? chatId;
  const senderName = msg.senderName ?? msg.sender ?? chat.name;
  const wasMentioned = isGroup && (msg.isMentioned === true);

  const access = resolveWeChatInboundAccessDecision({
    isGroup,
    senderId,
    policy,
  });
  if (!access.allowed) {
    log?.info?.(
      `[wechat:${liveAccount.accountId}] Blocked by policy (${access.reason}) from ${senderId}`,
    );
    return null;
  }

  const baseType = msg.type & 0x7fffffff;
  let attachment: WeChatAttachment | undefined;
  if (attachmentKindForMessageType(baseType)) {
    log?.info?.(`[wechat:${liveAccount.accountId}] Checking media for msg ${msg.localId} (type ${baseType})`);
    attachment = await retrieveAttachment(client, chatId, msg, liveAccount, log);
  }

  const timestamp = new Date(msg.timestamp).getTime();
  let sourceBody = msg.content || "";

  // Append reply context for quote/reply messages
  if (msg.reply) {
    const replySender = msg.reply.sender ?? "unknown sender";
    const quotedBody = msg.reply.content.length > 50
      ? msg.reply.content.slice(0, 50) + "..."
      : msg.reply.content;
    const replyBlock = `[Replying to ${replySender}]\n${quotedBody}\n[/Replying]`;
    sourceBody = sourceBody ? `${sourceBody}\n\n${replyBlock}` : replyBlock;
  }
  const prepared = applyLiveAttachment<ProcessedMessage>({
    msg,
    rawBody: sourceBody,
    sourceBody,
    commandBody: "",
    senderName,
    senderId,
    isGroup,
    timestamp,
    hasMedia: false,
    isMentioned: wasMentioned,
  }, sourceBody, attachment);
  prepared.commandBody = normalizeWeChatCommandBody(prepared.rawBody, {
    isGroup,
    wasMentioned,
  });
  return prepared;
}

async function refreshAttachment(
  message: ProcessedMessage,
  client: WeChatClient,
  chatId: string,
  liveAccount: ResolvedWeChatAccount,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
): Promise<ProcessedMessage> {
  if (!message.attachment) return message;
  if (message.attachment.status === "ready" && await attachmentPathExists(message.attachment.path)) {
    return message;
  }
  log?.info?.(`[wechat:${liveAccount.accountId}] Retrying attachment for msg ${message.msg.localId}`);
  const attachment = await retrieveAttachment(client, chatId, message.msg, liveAccount, log, 3);
  const refreshed = applyLiveAttachment(message, message.sourceBody, attachment);
  refreshed.commandBody = normalizeWeChatCommandBody(refreshed.rawBody, {
    isGroup: refreshed.isGroup,
    wasMentioned: refreshed.isMentioned,
  });
  return refreshed;
}

async function refreshAttachments(
  messages: ProcessedMessage[],
  client: WeChatClient,
  chatId: string,
  liveAccount: ResolvedWeChatAccount,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
): Promise<ProcessedMessage[]> {
  const output = [...messages];
  let cursor = 0;
  const worker = async () => {
    while (cursor < messages.length) {
      const index = cursor++;
      output[index] = await refreshAttachment(messages[index], client, chatId, liveAccount, log);
    }
  };
  await Promise.all(Array.from({ length: Math.min(3, messages.length) }, worker));
  return output;
}

/**
 * Dispatch a segment of one or more messages as a single LLM call.
 */
async function dispatchSegment(
  segment: ProcessedMessage[],
  client: WeChatClient,
  chatId: string,
  chat: Chat,
  liveAccount: ResolvedWeChatAccount,
  policy: WeChatPolicyContext,
  storeAllowFrom: string[],
  allowTextCommands: boolean,
  cfg: any,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
  remainingSegments?: number,
): Promise<"delivered" | "ignored" | "failed"> {
  const core = getWeChatRuntime();
  const lastMsg = segment[segment.length - 1];
  const { isGroup, senderId, senderName, timestamp, rawBody, commandBody, msg } = lastMsg;

  // Find the media attachment in this batch (at most one per batch)
  const mediaMsg = segment.find((pm) => pm.mediaPath);
  const mediaPath = mediaMsg?.mediaPath;
  const mediaMime = mediaMsg?.mediaMime;
  const attachmentPaths = segment.flatMap((pm) =>
    pm.attachment?.status === "ready" && pm.attachment.path
      ? [{
          path: pm.attachment.path,
          filename: pm.attachment.filename,
          kind: pm.attachment.kind,
          messageId: pm.msg.localId,
          sender: pm.senderName,
        }]
      : [],
  );

  log?.info?.(
    `[wechat:${liveAccount.accountId}] Dispatching segment: ${segment.length} msg(s), last=${msg.localId}` +
    `${mediaPath ? " with model media" : ""} attachments=${attachmentPaths.length}`,
  );

  const hasControlCommand =
    allowTextCommands && core.channel.commands.isControlCommandMessage(commandBody, cfg);
  const commandAuthorized = await resolveWeChatCommandAuthorization({
    cfg,
    rawBody: commandBody,
    isGroup,
    senderId,
    dmPolicy: policy.dmPolicy,
    allowFromForCommands: isGroup ? policy.effectiveGroupAllowFrom : policy.effectiveAllowFrom,
    deps: {
      shouldComputeCommandAuthorized: (raw, loadedCfg) =>
        core.channel.commands.shouldComputeCommandAuthorized(raw, loadedCfg),
      resolveCommandAuthorizedFromAuthorizers: (params) =>
        core.channel.commands.resolveCommandAuthorizedFromAuthorizers(params),
      readAllowFromStore: async () => storeAllowFrom,
    },
  });
  if (isGroup && allowTextCommands && hasControlCommand && commandAuthorized !== true) {
    log?.info?.(
      `[wechat:${liveAccount.accountId}] Dropping unauthorized group control command from ${senderId} in ${chatId}`,
    );
    return "ignored";
  }

  const mentionGate = resolveWeChatMentionGate({
    isGroup,
    requireMention: policy.requireMention,
    canDetectMention: true,
    wasMentioned: segment.some((pm) => pm.isMentioned),
    allowTextCommands,
    hasControlCommand,
    commandAuthorized: commandAuthorized === true,
  });
  if (isGroup && mentionGate.shouldSkip) {
    log?.info?.(
      `[wechat:${liveAccount.accountId}] Skipping group segment (mention required) in ${chatId}`,
    );
    return "ignored";
  }

  try {
    // Resolve routing using the last (triggering) message
    const route = core.channel.routing.resolveAgentRoute({
      cfg,
      channel: "agent-wechat",
      accountId: liveAccount.accountId,
      peer: {
        kind: isGroup ? "group" : "direct",
        id: isGroup ? chatId : senderId,
      },
    });

    const fromLabel = isGroup
      ? `group:${chat.name || chatId}`
      : senderName || `user:${senderId}`;
    const storePath = core.channel.session.resolveStorePath(
      cfg.session?.store,
      { agentId: route.agentId },
    );

    const envelopeOptions =
      core.channel.reply.resolveEnvelopeFormatOptions(cfg);
    const previousTimestamp =
      core.channel.session.readSessionUpdatedAt({
        storePath,
        sessionKey: route.sessionKey,
      });

    // Build body — with history context if batching multiple messages
    let body: string;
    let inboundHistory: Array<{ sender: string; body: string; timestamp?: number }> | undefined;

    if (segment.length === 1) {
      // Single message — format as today
      body = core.channel.reply.formatAgentEnvelope({
        channel: "WeChat",
        from: fromLabel,
        timestamp,
        previousTimestamp,
        envelope: envelopeOptions,
        body: isGroup ? `${senderName}: ${rawBody}` : rawBody,
      });
    } else {
      // Multi-message batch: earlier messages become history context
      const historyMessages = segment.slice(0, -1);

      // Format history entries
      const historyLines = historyMessages.map((pm) => {
        const entryBody = pm.isGroup ? `${pm.senderName}: ${pm.rawBody}` : pm.rawBody;
        return core.channel.reply.formatAgentEnvelope({
          channel: "WeChat",
          from: fromLabel,
          timestamp: pm.timestamp,
          envelope: envelopeOptions,
          body: entryBody,
        });
      });

      // Format current (last) message
      const currentLine = core.channel.reply.formatAgentEnvelope({
        channel: "WeChat",
        from: fromLabel,
        timestamp,
        previousTimestamp,
        envelope: envelopeOptions,
        body: isGroup ? `${senderName}: ${rawBody}` : rawBody,
      });

      // Combine with history context markers
      body = [
        HISTORY_CONTEXT_MARKER,
        ...historyLines,
        "",
        CURRENT_MESSAGE_MARKER,
        currentLine,
      ].join("\n");

      // Structured history for InboundHistory field
      inboundHistory = historyMessages.map((pm) => ({
        sender: pm.senderName,
        body: pm.rawBody,
        timestamp: pm.timestamp,
      }));
    }

    // For non-final batches, instruct agent to suppress reply (NO_REPLY token)
    if (remainingSegments && remainingSegments > 0) {
      body += `\n\n[More messages incoming — respond only with NO_REPLY]`;
    }

    // Build inbound context
    const ctxPayload = core.channel.reply.finalizeInboundContext({
      Body: body,
      BodyForAgent: rawBody,
      RawBody: rawBody,
      CommandBody: commandBody,
      InboundHistory: inboundHistory,
      From: isGroup ? `wechat:group:${chatId}` : `wechat:${senderId}`,
      To: `wechat:${chatId}`,
      SessionKey: route.sessionKey,
      AccountId: route.accountId,
      ChatType: isGroup ? "group" : "direct",
      ConversationLabel: fromLabel,
      SenderName: senderName || undefined,
      SenderId: senderId,
      Provider: "agent-wechat",
      Surface: "agent-wechat",
      MessageSid: `wechat:${chatId}:${msg.localId}`,
      WasMentioned: isGroup ? mentionGate.effectiveWasMentioned : undefined,
      CommandAuthorized: commandAuthorized,
      OriginatingChannel: "agent-wechat",
      OriginatingTo: `wechat:${chatId}`,
      Attachments: attachmentPaths,
      ...(mediaPath ? { MediaPath: mediaPath, MediaUrl: mediaPath, MediaType: mediaMime } : {}),
      ...(msg.reply ? {
        ReplyToBody: msg.reply.content.length > 50 ? msg.reply.content.slice(0, 50) + "..." : msg.reply.content,
        ReplyToSender: msg.reply.sender,
      } : {}),
    });

    // Record session
    await core.channel.session.recordInboundSession({
      storePath,
      sessionKey: ctxPayload.SessionKey ?? route.sessionKey,
      ctx: ctxPayload,
      onRecordError: (err: unknown) => {
        log?.error?.(
          `[wechat:${liveAccount.accountId}] Failed updating session meta: ${String(err)}`,
        );
      },
    });

    // Dispatch reply
    const { onModelSelected, ...prefixOptions } = createChannelReplyPipeline({
      cfg,
      agentId: route.agentId,
      channel: "agent-wechat",
      accountId: liveAccount.accountId,
    });

    let replyFailed = false;
    await core.channel.reply.dispatchReplyWithBufferedBlockDispatcher({
      ctx: ctxPayload,
      cfg,
      dispatcherOptions: {
        ...prefixOptions,
        deliver: async (payload: any) => {
          const mediaList: string[] = payload.mediaUrls?.length
            ? payload.mediaUrls
            : payload.mediaUrl
              ? [payload.mediaUrl]
              : [];

          const tableMode = core.channel.text.resolveMarkdownTableMode({
            cfg,
            channel: "agent-wechat",
            accountId: liveAccount.accountId,
          });
          const text = core.channel.text.convertMarkdownTables(
            payload.text ?? "",
            tableMode,
          );

          if (mediaList.length > 0) {
            for (const mediaUrl of mediaList) {
              try {
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
                if (isImage) {
                  await client.sendMessage({ chatId, image: { data: base64, mimeType } });
                } else {
                  await client.sendMessage({ chatId, file: { data: base64, filename } });
                }
              } catch (err) {
                log?.error?.(`[wechat:${liveAccount.accountId}] Failed to send media: ${err}`);
              }
            }
            // Send text caption separately if present
            if (text) {
              await client.sendMessage({ chatId, text });
            }
          } else if (text) {
            await client.sendMessage({ chatId, text });
          }
        },
        onError: (err: unknown, info: any) => {
          replyFailed = true;
          log?.error?.(
            `[wechat:${liveAccount.accountId}] ${info.kind} reply failed: ${String(err)}`,
          );
        },
      },
      replyOptions: {
        onModelSelected,
      },
    });
    if (replyFailed) return "failed";

    // Record activity
    core.channel.activity?.record?.({
      channel: "agent-wechat",
      accountId: liveAccount.accountId,
      direction: "inbound",
      at: timestamp,
    });

    return "delivered";
  } catch (err) {
    log?.error?.(
      `[wechat:${liveAccount.accountId}] Failed to dispatch segment (last msg ${msg.localId}): ${err}`,
    );
    return "failed";
  }
}

function bufferGroupHistory(
  groupHistory: Map<string, ProcessedMessage[]>,
  chatId: string,
  pm: ProcessedMessage,
  limit: number,
): void {
  const history = groupHistory.get(chatId) ?? [];
  history.push(pm);
  while (history.length > limit) {
    history.shift();
  }
  // LRU: refresh key insertion order
  groupHistory.delete(chatId);
  groupHistory.set(chatId, history);
  // Evict oldest groups if too many tracked
  if (groupHistory.size > 1000) {
    const first = groupHistory.keys().next().value;
    if (first) groupHistory.delete(first);
  }
}

async function processUnreadChat(
  client: WeChatClient,
  chat: Chat,
  lastSeenId: Map<string, number>,
  account: ResolvedWeChatAccount,
  cfg: any,
  log?: { info?: (...args: any[]) => void; error?: (...args: any[]) => void },
  skipOpen?: boolean,
  groupHistory?: Map<string, ProcessedMessage[]>,
  groupHistoryLimit?: number,
  persistState?: () => Promise<void>,
): Promise<void> {
  const core = getWeChatRuntime();
  // Re-resolve account from hot-reloaded config so policy changes take effect
  const liveAccount =
    resolveWeChatAccount(cfg as Record<string, unknown>, account.accountId) ??
    account;
  const chatId = chat.username ?? chat.id;
  const storeAllowFrom = await core.channel.pairing
    .readAllowFromStore({ channel: "agent-wechat", accountId: liveAccount.accountId, env: process.env })
    .catch(() => [] as string[]);
  const policy = resolveWeChatPolicyContext({
    account: liveAccount,
    cfg: cfg as any,
    chatId,
    storeAllowFrom,
  });
  const allowTextCommands = core.channel.commands.shouldHandleTextCommands({
    cfg,
    surface: "agent-wechat",
  });

  // Open the chat (triggers media downloads + clear unreads)
  if (!skipOpen) {
    log?.info?.(`[wechat:${liveAccount.accountId}] Opening chat ${chatId}...`);
    try {
      const opened = await client.openChat(chatId, true);
      if (opened.ok) {
        log?.info?.(`[wechat:${liveAccount.accountId}] Opened chat ${chatId}`);
      } else {
        log?.error?.(`[wechat:${liveAccount.accountId}] Chat selection did not complete for ${chatId}: ${opened.error ?? "unknown error"}`);
      }
    } catch (err) {
      log?.error?.(
        `[wechat:${liveAccount.accountId}] Failed to open chat ${chatId}: ${err}`,
      );
    }
  }

  // Determine how many messages to fetch
  const firstPoll = !lastSeenId.has(chatId);
  const prevLastSeen = lastSeenId.get(chatId) ?? 0;
  let messages: Message[];
  try {
    messages = await listMessageWindow(client, chatId, prevLastSeen, firstPoll, chat.unreadCount ?? 0);
  } catch (err) {
    log?.error?.(
      `[wechat:${liveAccount.accountId}] Failed to list messages for ${chatId}: ${err}`,
    );
    return;
  }

  log?.info?.(
    `[wechat:${liveAccount.accountId}] ${chatId}: fetched ${messages.length} msgs, firstPoll=${firstPoll}, prevLastSeen=${prevLastSeen}, unreadCount=${chat.unreadCount}`,
  );

  if (messages.length === 0) return;

  // On first poll, only process the last `unreadCount` messages
  // and seed lastSeenId from the rest
  let newMessages: Message[];
  if (firstPoll) {
    messages.sort((a, b) => a.localId - b.localId);
    const unread = chat.unreadCount ?? 0;
    if (unread > 0 && unread < messages.length) {
      newMessages = messages.slice(-unread);
      const seenMax = messages[messages.length - unread - 1].localId;
      lastSeenId.set(chatId, seenMax);
      await persistState?.();
    } else if (unread >= messages.length) {
      // All fetched messages are unread
      newMessages = messages;
    } else {
      // No unreads — just seed lastSeenId, don't process anything
      const maxId = messages[messages.length - 1].localId;
      lastSeenId.set(chatId, maxId);
      await persistState?.();
      return;
    }
  } else {
    newMessages = messages.filter((m) => m.localId > prevLastSeen);
    if (newMessages.length === 0) {
      // Don't update lastSeenId — if session.db reports a newer message
      // (via lastMsgLocalId) that hasn't appeared in message_N.db yet,
      // the catch-up loop will re-fire on the next poll.
      return;
    }
    newMessages.sort((a, b) => a.localId - b.localId);
  }

  log?.info?.(
    `[wechat:${liveAccount.accountId}] ${chatId}: ${newMessages.length} new msg(s) to process`,
  );

  // Pre-process all messages (filter, download media, build rawBody)
  const processed: ProcessedMessage[] = [];
  for (const msg of newMessages) {
    log?.info?.(
      `[wechat:${liveAccount.accountId}] Processing msg ${msg.localId}: type=${msg.type}, sender=${msg.sender}, isSelf=${msg.isSelf}`,
    );
    const pm = await prepareMessage(client, msg, chatId, chat, liveAccount, policy, log);
    if (pm) {
      processed.push(pm);
    }
  }

  // Group history catch-up: buffer or inject based on mention status
  const isGroup = chatId.includes("@chatroom");
  let clearBufferedHistory = false;
  const hasControlCommandInWindow =
    allowTextCommands &&
    processed.some((pm) => core.channel.commands.isControlCommandMessage(pm.commandBody, cfg));
  if (isGroup && groupHistory) {
    if (policy.requireMention) {
      const hasMention = processed.some(pm => pm.isMentioned);
      if (!hasMention && !hasControlCommandInWindow) {
        // No mention — buffer all messages and skip dispatch
        const limit = groupHistoryLimit ?? 50;
        for (const pm of processed) {
          bufferGroupHistory(groupHistory, chatId, pm, limit);
        }
        log?.info?.(`[wechat:${liveAccount.accountId}] Buffered ${processed.length} msg(s) for group history in ${chatId}`);
        const maxId = Math.max(...newMessages.map((m) => m.localId));
        lastSeenId.set(chatId, maxId);
        await persistState?.();
        return;
      }

      if (hasMention) {
        clearBufferedHistory = true;
        // Mention found — pull buffered history and prepend
        const buffered = groupHistory.get(chatId) ?? [];
        if (buffered.length > 0) {
          // Mark buffered messages as mentioned so they remain historical context.
          processed.unshift(...buffered.map((pm) => ({ ...pm, isMentioned: true })));
          log?.info?.(
            `[wechat:${liveAccount.accountId}] Injected ${buffered.length} buffered msg(s) as history in ${chatId}`,
          );
        }

        // Buffered media may have been pending or removed by retention. Retry it
        // before assembling catch-up, then retain every file path and select only
        // the latest image for OpenClaw's single media slot.
        processed.splice(0, processed.length, ...await refreshAttachments(processed, client, chatId, liveAccount, log));
        processed.splice(0, processed.length, ...applyCatchupAttachmentPolicy(processed));
      }
    } else {
      // Mention is disabled for this group; clear stale buffered entries once we reply.
      clearBufferedHistory = true;
    }
  }

  // Split into segments at media boundaries and dispatch each
  if (processed.length > 0) {
    const segments = hasControlCommandInWindow
      ? processed.map((pm) => [pm])
      : buildMediaSegments(processed);
    log?.info?.(
      `[wechat:${liveAccount.accountId}] ${chatId}: ${processed.length} dispatchable msg(s) in ${segments.length} segment(s)`,
    );
    let allDispatched = true;
    let allHandled = true;
    for (let i = 0; i < segments.length; i++) {
      const remaining = segments.length - i - 1;
      const outcome = await dispatchSegment(
        segments[i],
        client,
        chatId,
        chat,
        liveAccount,
        policy,
        storeAllowFrom,
        allowTextCommands,
        cfg,
        log,
        hasControlCommandInWindow ? undefined : remaining,
      );
      if (outcome !== "delivered") {
        allDispatched = false;
      }
      if (outcome === "failed") allHandled = false;
    }
    if (clearBufferedHistory && allDispatched && groupHistory) {
      groupHistory.set(chatId, []);
    }
    if (!allHandled) {
      // Keep the delivery cursor and buffered history unchanged so the next
      // poll retries this window. A crash after model acceptance remains
      // at-least-once rather than silently dropping the message.
      await persistState?.();
      return;
    }
  }

  // Update lastSeenId (track all messages including self-sent/filtered)
  const maxId = Math.max(...newMessages.map((m) => m.localId));
  lastSeenId.set(chatId, maxId);
  await persistState?.();
}
