import { createHash } from "node:crypto";
import { isNativeTombstone, NativeApiError, type NativeEvent, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { createChannelMessageReplyPipeline } from "openclaw/plugin-sdk/channel-outbound";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { toInboundMediaFacts } from "openclaw/plugin-sdk/channel-inbound";
import { normalizeWeChatCommandBody, resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { NATIVE_CATCH_UP_LIMIT, NATIVE_CATCH_UP_WINDOW_MS, NativeConsumerState } from "./native-consumer-state.ts";
import { invalidateNativePreparedInputs, nativeDispatchManifest, nativeSourceMessageSid, trackNativePreparedInput } from "./native-prepared-boundary.ts";
import { nativeRecallInfo, type NativeRecallObservation } from "./native-recall-state.ts";
import { submitNativeText } from "./native-send.ts";
import { nativePayloadMediaOptions } from "./outbound-options.ts";
import { validateWeChatMentionSend } from "./outbound-mentions.ts";
import { nativeReplyMessageId, nativeReplyReference, parseWeChatReplyTo, validateWeChatReplySend } from "./outbound-reply.ts";
import { sameEmbeddedQuote } from "./native-quote.ts";
import { prepareNativeAttachment, sendNativeAttachment } from "./native-media.ts";
import { buildNativeContext, nativeGroupHistoryLimit, prepareNativeHistory, transcribeNativeAttachment } from "./native-history.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount, type ResolvedWeChatAccount } from "./types.ts";
import type { WeChatMonitorOptions } from "./types.js";
export function nativeWasMentioned(message: NativeMessage): boolean {
  // Unknown/malformed direct metadata and displayed @names never satisfy a gate.
  return message.mentions?.state === "known" && (message.mentions.self === true || message.mentions.all === true);
}
export async function projectNativeRead(client: NativeWeChatClient, accountId: string, state: NativeConsumerState): Promise<void> {
  for (const { threadId, token } of state.pendingReads()) {
    for (let attempt = 0; attempt < 2; attempt++) {
      let cursor: string | undefined; let thread;
      const seen = new Set<string>();
      for (let pageIndex = 0; pageIndex < 100; pageIndex++) {
        const page = await client.threads(accountId, cursor);
        thread = page.items.find((value): value is import("@agent-wechat/shared").NativeThread => !isNativeTombstone(value) && value.id === threadId);
        if (thread || !page.nextCursor) break;
        if (seen.has(page.nextCursor)) throw new Error("Native thread pagination did not advance");
        seen.add(page.nextCursor); cursor = page.nextCursor;
      }
      if (!thread) break;
      try {
        // The phone notification outcome is not retried either way, so the request is cleared.
        await client.markRead(accountId, thread.id, thread.readStateVersion);
        state.clearRead(threadId, token);
        break;
      } catch (error) {
        if (error instanceof NativeApiError && error.status === 409 && attempt === 0) continue;
        throw error;
      }
    }
  }
}
type NativeDispatchParams = {
  client: NativeWeChatClient; state: NativeConsumerState; account: ResolvedWeChatAccount;
  message: NativeMessage; cfg: any; core: any;
  createPipeline?: typeof createChannelMessageReplyPipeline;
  prepareAttachment?: typeof prepareNativeAttachment;
  onDelivered?: () => void;
  signal?: AbortSignal;
  /** The queued item's revision when the caller read it; defaults to the current one. */
  revision?: string;
};
class NativeInputInvalidated extends Error {}

export { NATIVE_CATCH_UP_LIMIT, NATIVE_CATCH_UP_WINDOW_MS };
type NativeIntakeOptions = {
  now?: number;
  /** False for a message that cannot start a turn, so it is not queued or counted. */
  answerable?: (message: NativeMessage) => boolean | Promise<boolean>;
};
/** A message of a kind that can start a turn. */
function nativeTriggerEligible(message: NativeMessage): boolean {
  return message.direction === "incoming" && !!message.senderId && [1, 3, 34, 43, 47, 48, 49].includes(message.type)
    && !(message.type === 49 && message.appMessageType === 74) && !(message.type === 1 && !message.body)
    && !(message.type === 48 && (!message.location || !message.body));
}
/** The admission decision dispatch makes for a message under the current sender, group and mention policy. */
async function resolveNativeAdmission(message: NativeMessage, params: { account: ResolvedWeChatAccount; cfg: any; core: any }) {
  const { account, cfg, core } = params;
  const isGroup = message.threadId.endsWith("@chatroom");
  const wasMentioned = nativeWasMentioned(message);
  const policy = resolveWeChatPolicyContext({ account, cfg, chatId: message.threadId });
  const allowTextCommands = core.channel.commands.shouldHandleTextCommands({ cfg, surface: "agent-wechat" });
  const commandBody = normalizeWeChatCommandBody(message.type === 1 ? message.body ?? "" : "", { isGroup, wasMentioned });
  const hasControlCommand = allowTextCommands && core.channel.commands.isControlCommandMessage(commandBody, cfg);
  const resolve = core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress;
  const readStoreAllowFrom = async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []);
  const senderAdmission = (senderId: string) => resolveWeChatIngress({ resolve, cfg, accountId: account.accountId,
    policy, isGroup, chatId: message.threadId, senderId, readStoreAllowFrom });
  const ingress = await resolveWeChatIngress({ resolve, cfg, accountId: account.accountId, policy, isGroup, chatId: message.threadId,
    senderId: message.senderId ?? undefined, readStoreAllowFrom, command: { allowTextCommands, hasControlCommand }, mention: { wasMentioned, allowTextCommands } });
  // A direct command from an unauthorized sender still gets its denial reply.
  const unauthorizedDirectCommand = !isGroup && ingress.ingress.reasonCode === "control_command_unauthorized";
  return { isGroup, wasMentioned, policy, allowTextCommands, commandBody, hasControlCommand, senderAdmission, ingress, unauthorizedDirectCommand,
    admitted: ingress.ingress.admission === "dispatch" || unauthorizedDirectCommand };
}
/**
 * Whether a message would start a turn under the current policy: the same
 * sender, group, mention and command checks dispatch applies. Used to queue
 * and count only messages the bot would answer.
 */
export async function nativeMayAnswer(message: NativeMessage, params: { account: ResolvedWeChatAccount; cfg: any; core: any }): Promise<boolean> {
  return nativeTriggerEligible(message) && (await resolveNativeAdmission(message, params)).admitted;
}
function withinWindow(time: string | null | undefined, now: number): boolean {
  const at = time ? Date.parse(time) : Number.NaN;
  return !Number.isFinite(at) || at >= now - NATIVE_CATCH_UP_WINDOW_MS;
}
/**
 * Take in one event. The cursor records every event already seen, and the
 * daemon commits each message once, so only messages that can start a turn
 * are queued: live, incoming and recent. Everything else only moves the cursor.
 */
export async function acceptNativeEvent(client: NativeWeChatClient, accountId: string, state: NativeConsumerState, event: NativeEvent, options: NativeIntakeOptions = {}): Promise<NativeMessage | undefined> {
  if (state.accountId !== accountId) throw Error("Native event crosses its custody account boundary");
  if (BigInt(event.cursor) <= BigInt(state.cursor)) return undefined;
  if (event.kind === "message.recalled") {
    const affected = state.acceptRecall(event.cursor, event.data as NativeRecallObservation);
    invalidateNativePreparedInputs(accountId, affected);
    return undefined;
  }
  if (event.kind !== "message.committed") { state.advance(event.cursor); return undefined; }
  if (typeof event.data.messageId !== "string") throw Error("Native committed message identity is missing; cursor retained");
  const origin = typeof event.data.origin === "string" ? event.data.origin : "unknown";
  if (origin !== "live" || (event.data.direction != null && event.data.direction !== "incoming")) { state.advance(event.cursor); return undefined; }
  const now = options.now ?? Date.now();
  if (!withinWindow(event.createdAt, now)) { state.skipped.old++; state.advance(event.cursor); return undefined; }
  const message = await client.message(accountId, event.data.messageId);
  if (message.id !== event.data.messageId || message.accountId !== accountId || message.threadId !== event.data.threadId) throw Error("Native committed message identity mismatch");
  if (message.direction !== "incoming" || (options.answerable && !(await options.answerable(message)))) { state.advance(event.cursor); return message; }
  if (!withinWindow(message.createdAt, now)) { state.skipped.old++; state.advance(event.cursor); return message; }
  const replaced = state.accept(event.cursor, message, origin);
  if (replaced) invalidateNativePreparedInputs(accountId, [message.id]);
  return message;
}
async function recheckNativeInput(client: NativeWeChatClient, state: NativeConsumerState, message: NativeMessage, available: () => boolean): Promise<void> {
  if (!available()) throw new NativeInputInvalidated();
  const current = await client.message(message.accountId, message.id);
  if (current.accountId !== message.accountId || current.threadId !== message.threadId || current.id !== message.id || current.serverId !== message.serverId) throw Error("Native input identity changed; delivery held for review");
  const recall = nativeRecallInfo(current);
  if (recall && current.serverId) {
    const affected = state.noteRecall({ ...recall, messageId: current.id, threadId: current.threadId, serverId: current.serverId });
    invalidateNativePreparedInputs(message.accountId, [current.id, ...affected]);
    throw new NativeInputInvalidated();
  }
  if (message.quote) {
    const quote = current.quote;
    if (quote?.state === "recalled") {
      const affected = state.noteRecall({ threadId: message.threadId, serverId: message.quote.serverId, observedAt: new Date().toISOString() });
      invalidateNativePreparedInputs(message.accountId, affected);
      throw new NativeInputInvalidated();
    }
    if (!quote || !sameEmbeddedQuote(message.quote, quote)) throw Error("Native embedded quote changed; delivery held for review");
  }
  if (message.type === 48 && (current.type !== 48 || current.body !== message.body || JSON.stringify(current.location) !== JSON.stringify(message.location))) {
    throw Error("Native location changed; delivery held for review");
  }
  if (!available()) throw new NativeInputInvalidated();
}
/** Re-read the trigger and its context so a recall or change since they were read stops the turn. */
async function recheckNativeInputs(client: NativeWeChatClient, state: NativeConsumerState, message: NativeMessage, revision: string | undefined, history: NativeMessage[]): Promise<void> {
  await recheckNativeInput(client, state, message, () => state.isCurrent(message, revision));
  for (const item of history) await recheckNativeInput(client, state, item, () => state.isAvailable(item));
}
/** A quote whose original was recalled is dropped; the message's own text stays. */
function withoutRecalledQuote(state: NativeConsumerState, message: NativeMessage): NativeMessage {
  return message.quote && (message.quote.state === "recalled" || state.isRecalled({ threadId: message.threadId, serverId: message.quote.serverId }))
    ? { ...message, quote: undefined } : message;
}
function nativeContextEligible(message: NativeMessage): boolean {
  return nativeTriggerEligible(message) && !nativeRecallInfo(message);
}
const HISTORY_PAGES = 5;
/**
 * Group context for a mention: the unmentioned incoming messages since the
 * last message handed to the model in this group, read from the profile.
 * Messages still waiting for their own turn, commands and recalled messages
 * are left out. Sender policy is checked by the caller.
 */
export async function readNativeGroupHistory(params: { client: NativeWeChatClient; state: NativeConsumerState; message: NativeMessage; limit: number;
  isCommand: (message: NativeMessage) => boolean }): Promise<NativeMessage[]> {
  const { client, state, message, limit } = params;
  const item = state.get(message.id), boundary = state.groupBoundary(message.threadId);
  // A mention older than the boundary had its context offered to a later turn.
  if (!limit || !item || (boundary && BigInt(item.cursor) <= BigInt(boundary.cursor))) return [];
  const since = boundary?.createdAt ? Date.parse(boundary.createdAt) : Number.NaN;
  const history: NativeMessage[] = [];
  let found = false, cursor: string | undefined;
  for (let page = 0; page < HISTORY_PAGES; page++) {
    const result = await client.messages(message.accountId, message.threadId, { cursor, limit: 200 });
    for (const entry of result.items) {
      if (isNativeTombstone(entry)) continue;
      // The list is newest first; context is what precedes the mention.
      if (!found) { found = entry.id === message.id; continue; }
      if (entry.id === boundary?.messageId || (entry.createdAt && Date.parse(entry.createdAt) < since)) return history.reverse();
      if (!nativeContextEligible(entry) || state.isRecalled(entry) || state.get(entry.id) || params.isCommand(entry)) continue;
      history.push(withoutRecalledQuote(state, entry));
      if (history.length >= limit) return history.reverse();
    }
    if (!result.nextCursor) break;
    cursor = result.nextCursor;
  }
  return history.reverse();
}
export async function dispatchNativeMessage(params: NativeDispatchParams): Promise<"ignored" | "completed" | "needs_review" | "pending"> {
  const revision = params.revision ?? params.state.get(params.message.id)?.revision;
  try { return await dispatchNativeMessageInput(params, revision); }
  catch (error) {
    const item = params.state.get(params.message.id);
    if (params.state.isRecalled(params.message)) { params.state.finish(params.message.id, revision); return "ignored"; }
    if (error instanceof NativeInputInvalidated && item?.state === "received") return "pending";
    // Never retried: the turn or its reply may already have happened.
    params.state.finish(params.message.id, revision);
    return "needs_review";
  }
}
async function dispatchNativeMessageInput(params: NativeDispatchParams, revision?: string): Promise<"ignored" | "completed" | "needs_review" | "pending"> {
  const { client, state, account, cfg, core } = params;
  const readPolicy = account.native?.readPolicy ?? "on_inbound";
  const custody = state.get(params.message.id);
  if (state.isRecalled(params.message)) { state.finish(params.message.id, revision); return "ignored"; }
  if (!custody || custody.state !== "received" || !state.isCurrent(params.message, revision)) return "needs_review";
  const message = withoutRecalledQuote(state, params.message);
  // Upload notices precede a final attachment; they are not another model turn.
  if (message.type === 49 && message.appMessageType === 74) {
    state.finish(message.id, revision); return "ignored";
  }
  if (custody.origin !== "live" || !nativeTriggerEligible(message)) {
    state.finish(message.id, revision); return "ignored";
  }
  const admission = await resolveNativeAdmission(message, { account, cfg, core });
  const { isGroup, wasMentioned, policy, allowTextCommands, commandBody, hasControlCommand, senderAdmission, ingress } = admission;
  // An unmentioned group message stays in the profile and can be read as context for a later mention.
  if (!admission.admitted) { state.finish(message.id, revision); return "ignored"; }
  let preparedMedia;
  const allowQuotedMessage = async (original: Pick<NativeMessage, "senderId" | "direction">) => original.direction === "outgoing"
    || !!original.senderId && (await senderAdmission(original.senderId)).ingress.admission === "dispatch";
  const assertCurrent = () => { params.signal?.throwIfAborted(); if (!state.isCurrent(message, revision)) throw new NativeInputInvalidated(); };
  const recheck = async (history: NativeMessage[]) => { assertCurrent(); await recheckNativeInputs(client, state, message, revision, history); assertCurrent(); };
  await recheck([]);
  preparedMedia = await (params.prepareAttachment ?? prepareNativeAttachment)({ client, account, message, core, cfg, allowQuotedMessage, assertCurrent });
  if (preparedMedia.pending) return "pending";
  preparedMedia = await transcribeNativeAttachment(preparedMedia, core, cfg);
  await recheck([]);
  // Commands neither receive nor use up group context.
  const contextual = isGroup && !hasControlCommand;
  const buffered = contextual ? await readNativeGroupHistory({ client, state, message, limit: nativeGroupHistoryLimit(cfg),
    isCommand: entry => allowTextCommands && core.channel.commands.isControlCommandMessage(
      normalizeWeChatCommandBody(entry.type === 1 ? entry.body ?? "" : "", { isGroup, wasMentioned: nativeWasMentioned(entry) }), cfg) }) : [];
  const admitted: NativeMessage[] = [];
  for (const item of buffered) {
    // Only senders the current policy admits contribute context or media.
    if (item.senderId && (await senderAdmission(item.senderId)).ingress.admission === "dispatch") admitted.push(item);
  }
  const history = await prepareNativeHistory(admitted, async historical => transcribeNativeAttachment(
    await (params.prepareAttachment ?? prepareNativeAttachment)({ client, account, message: historical, core, cfg, allowQuotedMessage,
      assertCurrent: () => { assertCurrent(); if (!state.isAvailable(historical)) throw new NativeInputInvalidated(); } }), core, cfg));
  await recheck(admitted);
  const context = buildNativeContext(history, message, preparedMedia);
  const inputBody = context.inputBody;
  const route = core.channel.routing.resolveAgentRoute({ cfg, channel: "agent-wechat", accountId: account.accountId,
    peer: { kind: isGroup ? "group" : "direct", id: isGroup ? message.threadId : message.senderId } });
  const storePath = core.channel.session.resolveStorePath(cfg.session?.store, { agentId: route.agentId });
  const body = core.channel.reply.formatAgentEnvelope({ channel: "WeChat", from: message.senderId,
    timestamp: message.createdAt ? Date.parse(message.createdAt) : undefined, envelope: core.channel.reply.resolveEnvelopeFormatOptions(cfg), body: inputBody });
  const ctx = core.channel.reply.finalizeInboundContext({ Body: body, BodyForAgent: inputBody, RawBody: inputBody,
    CommandBody: commandBody, From: isGroup ? `wechat:group:${message.threadId}` : `wechat:${message.senderId}`,
    To: `wechat:${message.threadId}`, SessionKey: route.sessionKey, AccountId: account.accountId,
    ChatType: isGroup ? "group" : "direct", SenderId: message.senderId, Provider: "agent-wechat", Surface: "agent-wechat",
    MessageSid: nativeSourceMessageSid(message), WasMentioned: isGroup ? wasMentioned : undefined,
    Attachments: context.attachments, InboundHistory: context.history,
    CommandAuthorized: core.channel.commands.shouldComputeCommandAuthorized(commandBody, cfg) ? ingress.commandAccess.authorized : undefined,
    OriginatingChannel: "agent-wechat", OriginatingTo: `wechat:${message.threadId}`,
    // OpenClaw's media contract: it lists each attachment for the agent, stages
    // it into a sandboxed workspace and hydrates only unsuppressed images/video.
    ...(context.media.length ? { media: toInboundMediaFacts(context.media) } : {}),
  });
  await core.channel.session.recordInboundSession({ storePath, sessionKey: route.sessionKey, ctx });
  await recheck(admitted);
  const pipeline = (params.createPipeline ?? createChannelMessageReplyPipeline)({ cfg, agentId: route.agentId, channel: "agent-wechat", accountId: account.accountId });
  const { onModelSelected, ...prefixOptions } = pipeline;
  // Persist before making a model-side effect. A crash in this window is uncertain,
  // and reopening holds this item for review rather than duplicating a model turn.
  const dispatchManifest = nativeDispatchManifest({ message, inputBody, sessionKey: route.sessionKey,
    agentId: route.agentId, channelAccountId: account.accountId, policy, assets: context.assets, history: admitted });
  assertCurrent();
  state.beginDispatch(message.id, dispatchManifest, { revision, boundary: contextual ? { createdAt: message.createdAt } : undefined,
    inputServerIds: [message.quote?.serverId, ...admitted.flatMap(item => [item.serverId, item.quote?.serverId])].filter((id): id is string => !!id) });
  const callback = readPolicy !== "on_inbound"
    && (message.type === 1 || (message.type === 48 && !!message.location && !!message.body) || (message.type === 49 && [1, 17, 19, 87].includes(message.appMessageType ?? 0) && !!message.body)
      || (message.type === 49 && [3, 4, 5, 51].includes(message.appMessageType ?? 0) && !!message.link)
      || (preparedMedia.quote && [1, 49].includes(preparedMedia.quote.type ?? 0) && ["resolved", "embedded"].includes(preparedMedia.quote.state))
      || (preparedMedia.attachment?.status === "ready" && preparedMedia.suppliedAssets?.length))
    ? state.preparedCallback([message.id], readPolicy) : undefined;
  if (callback) trackNativePreparedInput(dispatchManifest, callback);
  const beforeSend = () => recheck(admitted);
  let failed = false; let delivery = 0;
  try {
    await core.channel.reply.dispatchReplyWithBufferedBlockDispatcher({ ctx, cfg,
      dispatcherOptions: { ...prefixOptions,
        deliver: async (payload: any) => {
          params.signal?.throwIfAborted();
          if (state.get(message.id)?.invalidatedByRecall) return;
          await beforeSend();
          const options = nativePayloadMediaOptions(payload);
          const replyToId = parseWeChatReplyTo(payload.replyToId);
          validateWeChatMentionSend(options.mentions, { ...payload, ...options, to: message.threadId });
          validateWeChatReplySend(replyToId, { ...payload, ...options });
          const original = replyToId ? await client.message(account.native!.accountId, nativeReplyMessageId(replyToId, account.native!.accountId)) : undefined;
          if (original?.recall) throw new Error("WeChat reply target was recalled");
          const quote = replyToId && original ? nativeReplyReference(original, account.native!.accountId, message.threadId, replyToId) : undefined;
          const urls: string[] = payload.mediaUrls?.length ? payload.mediaUrls : payload.mediaUrl ? [payload.mediaUrl] : [];
          if (urls.length > 16) throw new Error("Native reply media count exceeds its bound");
          if (payload.audioAsVoice && !urls.length) throw new Error("Native voice reply requires an audio attachment");
          for (const url of urls) { const key = createHash("sha256").update(`${dispatchManifest.messageSid}\0${delivery++}`).digest("hex"); await sendNativeAttachment(client, account, message.threadId, url, payload.audioAsVoice === true, key, beforeSend); }
          if (payload.text) { await beforeSend(); const key = createHash("sha256").update(`${dispatchManifest.messageSid}\0${delivery++}`).digest("hex"); await submitNativeText(client, account.native!.accountId, message.threadId, payload.text, key, { mentions: options.mentions, quote }); }
          if (urls.length || payload.text) params.onDelivered?.();
        }, onError: () => { failed = true; } },
      // This hook is intentionally separate from onModelSelected and old
      // onAgentRunStart (which can also report direct-tool execution). Hosts that
      // implement it (OpenClaw 2026.11.2+) call it once the run's transcript is
      // prepared, before provider image filtering, and it confirms the handoff
      // without the conversation-hook grant. Hosts without it need the separately
      // verified text input/model-call and media-snapshot bridge; unsupported or
      // ambiguous handoffs leave reads unchanged.
      replyOptions: { runId: dispatchManifest.runId, onModelSelected, onPreparedAgentRunStart: callback },
    });
    params.signal?.throwIfAborted();
    const settled = state.get(message.id);
    if (settled?.revision !== revision || settled?.dispatchManifest?.runId !== dispatchManifest.runId || settled.invalidatedByRecall) {
      state.finish(message.id, revision); return state.isRecalled(message) ? "ignored" : "needs_review";
    }
    // Normal inbound completion includes commands handled without a model.
    // Persist the read with completion so a restart can retry it independently
    // of model hooks, conversation history and incoming event traffic.
    state.complete(message.id, revision!, { failed, requestRead: readPolicy === "on_inbound" && ingress.ingress.admission === "dispatch",
      awaitHandoff: readPolicy === "on_model_dispatch" && !settled.runId });
    return failed ? "needs_review" : "completed";
  } catch {
    state.finish(message.id, revision);
    return state.isRecalled(message) ? "ignored" : "needs_review";
  }
}
const isExpiredCursor = (error: unknown) => error instanceof NativeApiError && error.code === "event_cursor_expired";
/**
 * Move a new or expired cursor to the first event of the catch-up window, never
 * before the stored cursor. The daemon keeps only its newest events, and older
 * messages are not answered, so nothing before the window needs reading.
 * Returns the new cursor, or undefined when the stored one already reaches the window.
 */
export async function resumeFromCatchUpWindow(client: NativeWeChatClient, accountId: string, state: NativeConsumerState, options: NativeIntakeOptions = {}): Promise<string | undefined> {
  const cutoff = (options.now ?? Date.now()) - NATIVE_CATCH_UP_WINDOW_MS;
  // True from the first cursor whose next event is recent. Event times rise with the cursor.
  const reachesWindow = async (after: bigint) => {
    try {
      const next = (await client.events(accountId, after.toString(), 1)).items[0];
      return !next || !(Date.parse(next.createdAt) < cutoff);
    } catch (error) { if (isExpiredCursor(error)) return false; throw error; }
  };
  const before = state.cursor;
  let low = BigInt(before);
  if (await reachesWindow(low)) return undefined;
  // Double the step until a cursor reaches the window, then narrow down to the first one.
  let step = 1n, high = low + step;
  while (!(await reachesWindow(high))) {
    if (step > 1n << 62n) throw Error("Native event log position could not be found");
    low = high; step *= 2n; high = low + step;
  }
  while (high - low > 1n) { const middle = (low + high) / 2n; if (await reachesWindow(middle)) high = middle; else low = middle; }
  state.advance(high.toString()); state.commit();
  return state.cursor === before ? undefined : state.cursor;
}
/**
 * Read the events missed since the cursor before offering stored input to a
 * model. Messages older than the catch-up window are skipped, and of the rest
 * only the newest {@link NATIVE_CATCH_UP_LIMIT} stay queued.
 */
export async function synchronizeNativeEvents(client: NativeWeChatClient, accountId: string, state: NativeConsumerState,
  options: NativeIntakeOptions & { onResume?: (from: string, to: string) => void;
    /** At startup, messages already queued count toward the limit together with those caught up. */
    includeQueued?: boolean } = {}): Promise<void> {
  const resume = async () => {
    const from = state.cursor, to = await resumeFromCatchUpWindow(client, accountId, state, options);
    if (to) options.onResume?.(from, to);
  };
  const queued: Array<{ id: string; revision: string }> = [];
  // Trimmed before each acknowledgement, so a crash cannot leave more than the limit queued.
  const trim = () => { state.skipped.limit += state.dropQueued(queued.splice(0, Math.max(0, queued.length - NATIVE_CATCH_UP_LIMIT))); };
  try {
    if (options.includeQueued) {
      const unanswered: Array<{ id: string; revision: string }> = [];
      for (const item of state.items().filter(value => value.state === "received")
        .sort((a, b) => BigInt(a.cursor) < BigInt(b.cursor) ? -1 : BigInt(a.cursor) > BigInt(b.cursor) ? 1 : 0)) {
        const entry = { id: item.id, revision: item.revision };
        // Stored work counts toward the limit only if the current policy would answer it,
        // including work an earlier release queued without that check.
        if (options.answerable) {
          let message: NativeMessage | undefined;
          try { message = await client.message(accountId, item.id); }
          catch (error) { if (error instanceof NativeApiError && error.status === 404) { unanswered.push(entry); continue; } if (!(error instanceof NativeApiError)) throw error; }
          if (message && !(await options.answerable(message))) { unanswered.push(entry); continue; }
        }
        queued.push(entry);
      }
      state.dropQueued(unanswered);
    }
    trim();
    // A new consumer does not replay the daemon's whole event history.
    if (state.cursor === "0") await resume();
    for (let index = 0; index < 100; index++) {
      let page;
      try { page = await client.events(accountId, state.cursor, 200); }
      catch (error) { if (!isExpiredCursor(error)) throw error; await resume(); continue; }
      if (!page.items.length) { state.commit(); return; }
      const before = state.cursor;
      for (const event of page.items) {
        const message = await acceptNativeEvent(client, accountId, state, event, options);
        const item = message && state.get(message.id);
        if (item?.cursor === event.cursor) queued.push({ id: item.id, revision: item.revision });
      }
      if (state.cursor === before) throw Error("Native recovery event cursor did not advance");
      trim(); state.commit();
      await client.ack(accountId, "openclaw", state.cursor);
      if (page.items.length < 200) return;
    }
    throw Error("Native recovery backlog remains; no model input was dispatched");
  } finally { trim(); }
}
/** Read recovery is independent of incoming SSE traffic and model execution. */
export function startNativeReadPump(task: () => Promise<void>, failed: () => void, intervalMs = 5000): () => void {
  let active = false, stopped = false;
  const timer = setInterval(() => {
    if (active || stopped) return;
    active = true;
    void task().catch(() => { if (!stopped) failed(); }).finally(() => { active = false; });
  }, intervalMs);
  timer.unref();
  return () => { stopped = true; clearInterval(timer); };
}
export async function startNativeMonitor(opts: WeChatMonitorOptions): Promise<void> {
  const native = opts.account.native; if (!native) throw new Error("Native configuration is missing");
  const host = await ensureNativeHost(native);
  const status = await host.client.status();
  if (typeof status.serverId !== "string") throw new Error("Native daemon identity is missing");
  const state = new NativeConsumerState(native.consumerStateDir, status.serverId, native.accountId);
  const core = getWeChatRuntime();
  const warn = (line: string) => { if (opts.log?.warn) opts.log.warn(line); else opts.log?.error?.(line); };
  if (state.heldOnOpen.length) warn(`${state.heldOnOpen.length} WeChat message(s) were not retried after restart: their model dispatch or reply could not be confirmed`);
  const reportSkipped = () => {
    const { old, limit, full } = state.skipped; state.skipped = { old: 0, limit: 0, full: 0 };
    if (old) opts.log?.info?.(`Skipped ${old} WeChat message(s) that arrived more than 24 hours ago`);
    if (limit) opts.log?.info?.(`Skipped ${limit} older missed WeChat message(s): catching up answers at most the newest ${NATIVE_CATCH_UP_LIMIT}`);
    if (full) warn(`${full} WeChat message(s) were not queued because the input queue is full`);
  };
  let backoff = 250;
  let readTask: Promise<void> | undefined;
  const readFailures = new Map<string, number>();
  // Only messages the bot would answer are queued, live and while catching up.
  const answerable = (message: NativeMessage) => {
    const cfg = core.config.current();
    return nativeMayAnswer(message, { account: resolveWeChatAccount(cfg as Record<string, unknown>, opts.account.accountId) ?? opts.account, cfg, core });
  };
  let startup = true;
  const flushRead = () => {
    if (native.readPolicy === "never") return Promise.resolve();
    return readTask ??= projectNativeRead(host.client, native.accountId, state).finally(() => { readTask = undefined; });
  };
  const process = async () => {
    const cfg = core.config.current();
    const account = resolveWeChatAccount(cfg as Record<string, unknown>, opts.account.accountId) ?? opts.account;
    if (account.backend !== "native" || JSON.stringify(account.native) !== JSON.stringify(native)) throw new Error("Native service configuration changed; restart required before processing");
    for (const item of state.items().filter(value => value.state === "received")) {
      // A recall, a replacement or the catch-up limit may have settled it since this pass began.
      if (state.get(item.id)?.revision !== item.revision || state.get(item.id)?.state !== "received") continue;
      // Queued work holds only the message's identity; read the message as it is now.
      let message: NativeMessage;
      try { message = await host.client.message(native.accountId, item.id); }
      catch (error) {
        // An unreachable daemon ends the pass; the message stays queued for the next one.
        if (!(error instanceof NativeApiError) && (error instanceof TypeError || (error as Error)?.name === "TimeoutError" || (error as Error)?.name === "AbortError")) throw error;
        // One message the daemon cannot return must not hold up the rest of the queue.
        const failures = (readFailures.get(item.id) ?? 0) + 1; readFailures.set(item.id, failures);
        if (error instanceof NativeApiError && error.status !== 404 && error.retryable && failures < 3) continue;
        readFailures.delete(item.id); state.finish(item.id, item.revision);
        if (!(error instanceof NativeApiError && error.status === 404)) warn(`WeChat message ${item.id} could not be read from the daemon and was not answered`);
        continue;
      }
      readFailures.delete(item.id);
      if (state.get(item.id)?.revision !== item.revision) continue;
      if (message.accountId !== native.accountId || message.threadId !== item.threadId || (message.serverId ?? null) !== item.serverId) {
        state.finish(item.id, item.revision); warn(`WeChat message ${item.id} changed before it was handled and was not answered`); continue;
      }
      // Work queued before a long stop follows the same 24-hour rule as messages caught up.
      if (message.createdAt && Date.parse(message.createdAt) < Date.now() - NATIVE_CATCH_UP_WINDOW_MS) {
        if (state.finish(item.id, item.revision)) state.skipped.old++; continue;
      }
      const outcome = await dispatchNativeMessage({ client: host.client, state, account, message, cfg, core, revision: item.revision,
        signal: opts.abortSignal, onDelivered: () => opts.setStatus({ accountId: account.accountId, lastOutboundAt: Date.now() }) });
      // Never retried: resending could reply twice. Logged once, not kept as a status warning.
      if (outcome === "needs_review") warn(`WeChat message ${item.id} was not retried: its model dispatch or reply could not be confirmed`);
    }
    reportSkipped();
    try { await flushRead(); }
    catch { opts.log?.error?.("Native read projection is pending; thread read receipt retained for retry"); }
    const handoffUnconfirmed = native.readPolicy === "on_model_dispatch" && state.items().some(value => value.handoffUnconfirmed);
    const accountStatus = await host.client.account(native.accountId);
    opts.setStatus({ accountId: account.accountId, running: true, connected: true,
      linked: accountStatus.authenticated === true && accountStatus.connection === "online", handoffUnconfirmed });
  };
  let processing: Promise<void> | undefined;
  let processAgain = false;
  let intakeReady = false;
  const scheduleProcess = () => {
    processAgain = true;
    if (processing || opts.abortSignal.aborted || !intakeReady) return;
    processing = (async () => {
      // Catching up holds back new passes, so work is not started before the catch-up limit applies.
      do { processAgain = false; await process(); } while (processAgain && intakeReady && !opts.abortSignal.aborted);
    })().catch(() => opts.log?.error?.("Native input processing was interrupted; durable custody was retained"))
      .finally(() => { processing = undefined; });
  };
  // Retry durable whole-thread reads while an otherwise quiet SSE stream
  // remains open, including late confirmations from the explicit model policy.
  const stopReadPump = startNativeReadPump(async () => {
    await flushRead();
    scheduleProcess();
    if (!opts.abortSignal.aborted) opts.setStatus({ accountId: opts.account.accountId,
      handoffUnconfirmed: native.readPolicy === "on_model_dispatch" && state.items().some(value => value.handoffUnconfirmed) });
  }, () => opts.log?.error?.("Native read projection remains pending; thread read receipts retained"));
  try { while (!opts.abortSignal.aborted) {
    try {
      // A prior ACK failure must not leave durable received items idle while an
      // otherwise quiet SSE connection waits for another message.
      if (state.cursor !== "0") { state.commit(); await host.client.ack(native.accountId, "openclaw", state.cursor); }
      // Drain durable events before resuming stored input, then keep intake
      // independent of slow media/model work so a recall can invalidate it.
      intakeReady = false;
      await synchronizeNativeEvents(host.client, native.accountId, state, { answerable, includeQueued: startup,
        onResume: (from, to) => warn(`WeChat event position ${from === "0" ? "was new" : `${from} had expired`}; resumed at ${to}, the start of the last 24 hours`) });
      startup = false;
      reportSkipped();
      intakeReady = true;
      scheduleProcess();
      for await (const event of host.client.watch(native.accountId, state.cursor, opts.abortSignal)) {
        const message = await acceptNativeEvent(host.client, native.accountId, state, event, { answerable });
        reportSkipped();
        if (message?.direction === "incoming" && event.data.origin === "live" && !nativeRecallInfo(message)) opts.setStatus({ accountId: opts.account.accountId, lastInboundAt: Date.now() });
        state.commit();
        await host.client.ack(native.accountId, "openclaw", state.cursor);
        scheduleProcess(); backoff = 250;
      }
      if (!opts.abortSignal.aborted) throw new Error("Native event stream closed");
    } catch (error) {
      if (opts.abortSignal.aborted) break;
      // An expired cursor is resumed from the catch-up window on the next pass.
      opts.setStatus({ accountId: opts.account.accountId, connected: false, running: true });
      opts.log?.error?.("Native event stream interrupted; retained cursor/custody will resume without replaying sends");
      await new Promise<void>(resolve => {
        const finish = () => { clearTimeout(timer); opts.abortSignal.removeEventListener("abort", finish); resolve(); };
        const timer = setTimeout(finish, backoff); opts.abortSignal.addEventListener("abort", finish, { once: true });
      });
      backoff = Math.min(backoff * 2, 30000);
    }
  }
  } finally { stopReadPump(); }
  opts.setStatus({ accountId: opts.account.accountId, running: false, connected: false });
}
