import { createHash } from "node:crypto";
import { isNativeTombstone, NativeApiError, type NativeEvent, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { createChannelMessageReplyPipeline } from "openclaw/plugin-sdk/channel-outbound";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { normalizeWeChatCommandBody, resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { NativeConsumerState } from "./native-consumer-state.ts";
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
  const pending = state.items().filter(value => value.readPending);
  const threads = [...new Set(pending.map(item => item.message.threadId))];
  for (const threadId of threads) {
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
        const receipt = await client.markRead(accountId, thread.id, thread.readStateVersion);
        const notification = (receipt as { phoneNotification?: { state?: string } })?.phoneNotification?.state;
        const phoneReadState = notification === "accepted" || notification === "rejected" || notification === "needs_review" ? notification : undefined;
        for (const item of pending.filter(item => item.message.threadId === threadId)) state.update(item.message.id, { readPending: false, phoneReadState }, item.revision);
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
};
class NativeInputInvalidated extends Error {}

export async function acceptNativeEvent(client: NativeWeChatClient, accountId: string, state: NativeConsumerState, event: NativeEvent): Promise<NativeMessage | undefined> {
  if (state.accountId !== accountId) throw Error("Native event crosses its custody account boundary");
  if (event.kind === "message.recalled") {
    const affected = state.acceptRecall(event.cursor, event.data as NativeRecallObservation);
    invalidateNativePreparedInputs(accountId, affected);
    return undefined;
  }
  let message: NativeMessage | undefined;
  if (event.kind === "message.committed") {
    if (typeof event.data.messageId !== "string") throw Error("Native committed message identity is missing; cursor retained");
    message = await client.message(accountId, event.data.messageId);
    if (message.id !== event.data.messageId || message.accountId !== accountId || message.threadId !== event.data.threadId) throw Error("Native committed message identity mismatch");
  }
  const replaced = state.accept(event.cursor, message, typeof event.data.origin === "string" ? event.data.origin : "unknown");
  if (replaced && message) invalidateNativePreparedInputs(accountId, [message.id]);
  return message;
}
async function recheckNativeInputs(client: NativeWeChatClient, state: NativeConsumerState, messages: NativeMessage[]): Promise<void> {
  for (const message of messages) {
    if (!state.isCurrent(message)) throw new NativeInputInvalidated();
    const current = await client.message(message.accountId, message.id);
    if (current.accountId !== message.accountId || current.threadId !== message.threadId || current.id !== message.id || current.serverId !== message.serverId) throw Error("Native input identity changed; delivery held for review");
    const recall = nativeRecallInfo(current);
    if (recall && current.serverId) {
      const affected = state.acceptRecall(state.cursor, { ...recall, messageId: current.id, threadId: current.threadId, serverId: current.serverId });
      invalidateNativePreparedInputs(message.accountId, [current.id, ...affected]);
      throw new NativeInputInvalidated();
    }
    if (message.quote) {
      const quote = current.quote;
      if (quote?.state === "recalled") {
        const affected = state.acceptRecall(state.cursor, { threadId: message.threadId, serverId: message.quote.serverId,
          observedAt: new Date().toISOString() });
        invalidateNativePreparedInputs(message.accountId, affected);
        throw new NativeInputInvalidated();
      }
      if (!quote || !sameEmbeddedQuote(message.quote, quote)) throw Error("Native embedded quote changed; delivery held for review");
    }
    if (message.type === 48 && (current.type !== 48 || current.body !== message.body || JSON.stringify(current.location) !== JSON.stringify(message.location))) {
      throw Error("Native location changed; delivery held for review");
    }
    if (!state.isCurrent(message)) throw new NativeInputInvalidated();
  }
}
export async function dispatchNativeMessage(params: NativeDispatchParams): Promise<"ignored" | "completed" | "needs_review" | "pending"> {
  const revision = params.state.get(params.message.id)?.revision;
  try { return await dispatchNativeMessageInput(params, revision); }
  catch (error) {
    const item = params.state.get(params.message.id);
    if (params.state.isRecalled(params.message)) return "ignored";
    if (error instanceof NativeInputInvalidated && item?.state === "received") return "pending";
    if (item) params.state.update(params.message.id, { state: "needs_review" }, revision);
    return "needs_review";
  }
}
async function dispatchNativeMessageInput(params: NativeDispatchParams, revision?: string): Promise<"ignored" | "completed" | "needs_review" | "pending"> {
  const { client, state, account, message, cfg, core } = params;
  const readPolicy = account.native?.readPolicy ?? "on_inbound";
  const custody = state.get(message.id);
  if (state.isRecalled(message)) return "ignored";
  if (!custody || custody.state !== "received" || !state.isCurrent(message, revision)) return "needs_review";
  // Upload notices precede a final attachment; they are not another model turn.
  if (message.type === 49 && message.appMessageType === 74) {
    state.update(message.id, { state: "ignored" }, revision); return "ignored";
  }
  if (custody.origin !== "live" || message.direction !== "incoming" || ![1,3,34,43,47,48,49].includes(message.type) || (message.type === 1 && !message.body) || (message.type === 48 && (!message.location || !message.body)) || !message.senderId) {
    state.update(message.id, { state: "ignored" }, revision); return "ignored";
  }
  const isGroup = message.threadId.endsWith("@chatroom");
  const wasMentioned = nativeWasMentioned(message);
  const policy = resolveWeChatPolicyContext({ account, cfg, chatId: message.threadId });
  const allowTextCommands = core.channel.commands.shouldHandleTextCommands({ cfg, surface: "agent-wechat" });
  const commandBody = normalizeWeChatCommandBody(message.type === 1 ? message.body ?? "" : "", { isGroup, wasMentioned });
  const hasControlCommand = allowTextCommands && core.channel.commands.isControlCommandMessage(commandBody, cfg);
  const resolve = core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress;
  const senderAdmission = (senderId: string) => resolveWeChatIngress({ resolve, cfg, accountId: account.accountId,
    policy, isGroup, chatId: message.threadId, senderId,
    readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
  const ingress = await resolveWeChatIngress({
    resolve,
    cfg, accountId: account.accountId, policy, isGroup, chatId: message.threadId, senderId: message.senderId,
    readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []),
    command: { allowTextCommands, hasControlCommand },
    mention: { wasMentioned, allowTextCommands },
  });
  const unauthorizedDirectCommand = !isGroup && ingress.ingress.reasonCode === "control_command_unauthorized";
  if (ingress.ingress.admission !== "dispatch" && !unauthorizedDirectCommand) {
    if (isGroup && policy.requireMention && !wasMentioned && !hasControlCommand
      && (await senderAdmission(message.senderId)).ingress.admission === "dispatch") {
      state.bufferGroupMessage(message.id, nativeGroupHistoryLimit(cfg), revision); return "ignored";
    }
    state.update(message.id, { state: "ignored" }, revision); return "ignored";
  }
  let preparedMedia;
  const allowQuotedMessage = async (original: Pick<NativeMessage, "senderId" | "direction">) => original.direction === "outgoing"
    || !!original.senderId && (await senderAdmission(original.senderId)).ingress.admission === "dispatch";
  const assertCurrent = () => { params.signal?.throwIfAborted(); if (!state.isCurrent(message, revision)) throw new NativeInputInvalidated(); };
  const recheck = async (inputs: NativeMessage[]) => { assertCurrent(); await recheckNativeInputs(client, state, inputs); assertCurrent(); };
  await recheck([message]);
  preparedMedia = await (params.prepareAttachment ?? prepareNativeAttachment)({ client, account, message, core, cfg, allowQuotedMessage, assertCurrent });
  if (preparedMedia.pending) return "pending";
  preparedMedia = await transcribeNativeAttachment(preparedMedia, core, cfg);
  await recheck([message]);
  const buffered = isGroup && !hasControlCommand ? state.groupHistory(message.threadId, message.id, nativeGroupHistoryLimit(cfg)) : [];
  const admitted: NativeMessage[] = [];
  for (const item of buffered) {
    // A sender removed since intake must not return through history/media.
    if (item.message.senderId && (await senderAdmission(item.message.senderId)).ingress.admission === "dispatch") admitted.push(item.message);
  }
  const history = await prepareNativeHistory(admitted, async historical => transcribeNativeAttachment(
    await (params.prepareAttachment ?? prepareNativeAttachment)({ client, account, message: historical, core, cfg, allowQuotedMessage,
      assertCurrent: () => { assertCurrent(); if (!state.isCurrent(historical)) throw new NativeInputInvalidated(); } }), core, cfg));
  await recheck([message, ...admitted]);
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
    ...(context.mediaPath ? { MediaPath: context.mediaPath, MediaUrl: context.mediaPath, MediaType: context.mediaMime } : {}),
  });
  await core.channel.session.recordInboundSession({ storePath, sessionKey: route.sessionKey, ctx });
  await recheck([message, ...admitted]);
  const pipeline = (params.createPipeline ?? createChannelMessageReplyPipeline)({ cfg, agentId: route.agentId, channel: "agent-wechat", accountId: account.accountId });
  const { onModelSelected, ...prefixOptions } = pipeline;
  // Persist before making a model-side effect. A crash in this window is uncertain,
  // and reopening holds this item for review rather than duplicating a model turn.
  const dispatchManifest = nativeDispatchManifest({ message, inputBody, sessionKey: route.sessionKey,
    agentId: route.agentId, channelAccountId: account.accountId, policy, assets: context.assets, history: admitted });
  assertCurrent(); state.beginDispatch(message.id, dispatchManifest, buffered.map(item => item.message.id));
  const callback = readPolicy !== "on_inbound"
    && (message.type === 1 || (message.type === 48 && !!message.location && !!message.body) || (message.type === 49 && [1, 17, 19, 87].includes(message.appMessageType ?? 0) && !!message.body)
      || (message.type === 49 && [3, 4, 5, 51].includes(message.appMessageType ?? 0) && !!message.link)
      || (preparedMedia.quote && [1, 49].includes(preparedMedia.quote.type ?? 0) && ["resolved", "embedded"].includes(preparedMedia.quote.state))
      || (preparedMedia.attachment?.status === "ready" && preparedMedia.suppliedAssets?.length))
    ? state.preparedCallback([message.id], readPolicy) : undefined;
  if (callback) trackNativePreparedInput(dispatchManifest, callback);
  const beforeSend = () => recheck([message, ...admitted]);
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
      // onAgentRunStart (which can also report direct-tool execution). Released
      // hosts without it need the separately verified text input/model-call
      // and media-snapshot bridge; unsupported or ambiguous handoffs leave reads unchanged.
      replyOptions: { runId: dispatchManifest.runId, onModelSelected, onPreparedAgentRunStart: callback },
    });
    params.signal?.throwIfAborted();
    if (state.get(message.id)?.revision !== revision || state.get(message.id)?.dispatchManifest?.runId !== dispatchManifest.runId) return state.isRecalled(message) ? "ignored" : "needs_review";
    if (state.get(message.id)?.invalidatedByRecall) return state.isRecalled(message) ? "ignored" : "needs_review";
    // Normal inbound completion includes commands handled without a model.
    // Persist the read with completion so a restart can retry it independently
    // of model hooks, conversation history and incoming event traffic.
    const readPending = readPolicy === "on_inbound" && !failed && ingress.ingress.admission === "dispatch"
      ? true : state.get(message.id)?.readPending;
    state.update(message.id, { state: failed ? "needs_review" : "completed", readPending,
      handoffUnconfirmed: readPolicy === "on_model_dispatch" && !state.get(message.id)?.runId }, revision);
    return failed ? "needs_review" : "completed";
  } catch {
    if (state.isRecalled(message)) return "ignored";
    state.update(message.id, { state: "needs_review" }, revision); return "needs_review";
  }
}
/** Replay committed custody changes before offering persisted input to a model. */
export async function synchronizeNativeEvents(client: NativeWeChatClient, accountId: string, state: NativeConsumerState): Promise<void> {
  for (let index = 0; index < 100; index++) {
    const page = await client.events(accountId, state.cursor, 200);
    if (!page.items.length) return;
    const before = state.cursor;
    for (const event of page.items) await acceptNativeEvent(client, accountId, state, event);
    if (state.cursor === before) throw Error("Native recovery event cursor did not advance");
    await client.ack(accountId, "openclaw", state.cursor);
    if (page.items.length < 200) return;
  }
  throw Error("Native recovery backlog remains; no model input was dispatched");
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
  let backoff = 250;
  let readTask: Promise<void> | undefined;
  const flushRead = () => {
    if (native.readPolicy === "never") return Promise.resolve();
    return readTask ??= projectNativeRead(host.client, native.accountId, state).finally(() => { readTask = undefined; });
  };
  const process = async () => {
    const cfg = core.config.current();
    const account = resolveWeChatAccount(cfg as Record<string, unknown>, opts.account.accountId) ?? opts.account;
    if (account.backend !== "native" || JSON.stringify(account.native) !== JSON.stringify(native)) throw new Error("Native service configuration changed; restart required before processing");
    for (const item of state.items().filter(value => value.state === "received")) {
      await dispatchNativeMessage({ client: host.client, state, account, message: item.message, cfg, core,
        signal: opts.abortSignal, onDelivered: () => opts.setStatus({ accountId: account.accountId, lastOutboundAt: Date.now() }) });
    }
    try { await flushRead(); }
    catch { opts.log?.error?.("Native read projection is pending; thread read receipt retained for retry"); }
    const needsReview = state.items().filter(value => value.state === "needs_review" || value.phoneReadState === "needs_review" || value.phoneReadState === "rejected").length;
    const handoffUnconfirmed = native.readPolicy === "on_model_dispatch" && state.items().some(value => value.handoffUnconfirmed);
    const accountStatus = await host.client.account(native.accountId);
    opts.setStatus({ accountId: account.accountId, running: true, connected: true,
      linked: accountStatus.authenticated === true && accountStatus.connection === "online", needsReview, handoffUnconfirmed });
  };
  let processing: Promise<void> | undefined;
  let processAgain = false;
  let intakeReady = false;
  const scheduleProcess = () => {
    processAgain = true;
    if (processing || opts.abortSignal.aborted || !intakeReady) return;
    processing = (async () => {
      do { processAgain = false; await process(); } while (processAgain && !opts.abortSignal.aborted);
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
      if (state.cursor !== "0") await host.client.ack(native.accountId, "openclaw", state.cursor);
      // Drain durable events before resuming stored input, then keep intake
      // independent of slow media/model work so a recall can invalidate it.
      intakeReady = false;
      await synchronizeNativeEvents(host.client, native.accountId, state);
      intakeReady = true;
      scheduleProcess();
      for await (const event of host.client.watch(native.accountId, state.cursor, opts.abortSignal)) {
        const message = await acceptNativeEvent(host.client, native.accountId, state, event);
        if (message?.direction === "incoming" && event.data.origin === "live" && !nativeRecallInfo(message)) opts.setStatus({ accountId: opts.account.accountId, lastInboundAt: Date.now() });
        await host.client.ack(native.accountId, "openclaw", state.cursor);
        scheduleProcess(); backoff = 250;
      }
      if (!opts.abortSignal.aborted) throw new Error("Native event stream closed");
    } catch (error) {
      if (opts.abortSignal.aborted) break;
      if (error instanceof NativeApiError && error.code === "event_cursor_expired") {
        opts.setStatus({ accountId: opts.account.accountId, connected: false, running: false, needsReview: 1, lastError: "event_cursor_expired" });
        throw new Error("Native journal cursor expired; explicit snapshot/replay is required before resuming");
      }
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
