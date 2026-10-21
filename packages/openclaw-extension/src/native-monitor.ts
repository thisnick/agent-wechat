import { createHash } from "node:crypto";
import { isNativeTombstone, NativeApiError, type NativeEvent, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { createChannelMessageReplyPipeline } from "openclaw/plugin-sdk/channel-outbound";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { normalizeWeChatCommandBody, resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { nativeDispatchManifest, trackNativePreparedInput } from "./native-prepared-boundary.ts";
import { submitNativeText } from "./native-send.ts";
import { prepareNativeAttachment, sendNativeAttachment } from "./native-media.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount, type ResolvedWeChatAccount } from "./types.ts";
import type { WeChatMonitorOptions } from "./types.js";
export function nativeWasMentioned(message: NativeMessage): boolean {
  // Unknown/malformed direct metadata and displayed @names never satisfy a gate.
  return message.mentions?.state === "known" && message.mentions.self === true;
}
export async function projectNativeRead(client: NativeWeChatClient, accountId: string, state: NativeConsumerState): Promise<void> {
  for (const item of state.items().filter(value => value.readPending && value.runId)) {
    for (let attempt = 0; attempt < 2; attempt++) {
      let cursor: string | undefined; let thread;
      const seen = new Set<string>();
      for (let pageIndex = 0; pageIndex < 100; pageIndex++) {
        const page = await client.threads(accountId, cursor);
        thread = page.items.find((value): value is import("@agent-wechat/shared").NativeThread => !isNativeTombstone(value) && value.id === item.message.threadId);
        if (thread || !page.nextCursor) break;
        if (seen.has(page.nextCursor)) throw new Error("Native thread pagination did not advance");
        seen.add(page.nextCursor); cursor = page.nextCursor;
      }
      if (!thread) break;
      try {
        await client.markRead(accountId, thread.id, [item.message.id], thread.readStateVersion);
        state.update(item.message.id, { readPending: false }); break;
      } catch (error) {
        if (error instanceof NativeApiError && error.status === 409 && attempt === 0) continue;
        throw error;
      }
    }
  }
}
export async function dispatchNativeMessage(params: {
  client: NativeWeChatClient; state: NativeConsumerState; account: ResolvedWeChatAccount;
  message: NativeMessage; cfg: any; core: any;
  createPipeline?: typeof createChannelMessageReplyPipeline;
  prepareAttachment?: typeof prepareNativeAttachment;
}): Promise<"ignored" | "completed" | "needs_review" | "pending"> {
  const { client, state, account, message, cfg, core } = params;
  const custody = state.get(message.id);
  if (!custody || custody.state !== "received") return "needs_review";
  if (custody.origin !== "live" || message.direction !== "incoming" || ![1,3,34,43,47,49].includes(message.type) || (message.type === 1 && !message.body) || !message.senderId) {
    state.update(message.id, { state: "ignored" }); return "ignored";
  }
  const isGroup = message.threadId.endsWith("@chatroom");
  const wasMentioned = nativeWasMentioned(message);
  const policy = resolveWeChatPolicyContext({ account, cfg, chatId: message.threadId });
  const allowTextCommands = core.channel.commands.shouldHandleTextCommands({ cfg, surface: "agent-wechat" });
  const commandBody = normalizeWeChatCommandBody(message.type === 1 ? message.body ?? "" : "", { isGroup, wasMentioned });
  const ingress = await resolveWeChatIngress({
    resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
    cfg, accountId: account.accountId, policy, isGroup, chatId: message.threadId, senderId: message.senderId,
    readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []),
    command: { allowTextCommands, hasControlCommand: allowTextCommands && core.channel.commands.isControlCommandMessage(commandBody, cfg) },
    mention: { wasMentioned, allowTextCommands },
  });
  const unauthorizedDirectCommand = !isGroup && ingress.ingress.reasonCode === "control_command_unauthorized";
  if (ingress.ingress.admission !== "dispatch" && !unauthorizedDirectCommand) {
    state.update(message.id, { state: "ignored" }); return "ignored";
  }
  let preparedMedia;
  try { preparedMedia = await (params.prepareAttachment ?? prepareNativeAttachment)({ client, account, message, core, cfg }); }
  catch { state.update(message.id, { state: "needs_review" }); return "needs_review"; }
  if (preparedMedia.pending) return "pending";
  let inputBody = preparedMedia.body;
  if (preparedMedia.attachment?.kind === "audio" && preparedMedia.attachment.status === "ready" && preparedMedia.attachment.path) {
    try { const transcript = await core.mediaUnderstanding.transcribeAudioFile({ filePath: preparedMedia.attachment.path, cfg, mime: preparedMedia.attachment.mime }); if (transcript.text?.trim()) inputBody += `\n[Voice transcript: ${transcript.text.trim().slice(0, 10000)}]`; else inputBody += "\n[Voice transcription unavailable]"; }
    catch { inputBody += "\n[Voice transcription unavailable]"; }
  }
  const route = core.channel.routing.resolveAgentRoute({ cfg, channel: "agent-wechat", accountId: account.accountId,
    peer: { kind: isGroup ? "group" : "direct", id: isGroup ? message.threadId : message.senderId } });
  const storePath = core.channel.session.resolveStorePath(cfg.session?.store, { agentId: route.agentId });
  const body = core.channel.reply.formatAgentEnvelope({ channel: "WeChat", from: message.senderId,
    timestamp: message.createdAt ? Date.parse(message.createdAt) : undefined, envelope: core.channel.reply.resolveEnvelopeFormatOptions(cfg), body: inputBody });
  const ctx = core.channel.reply.finalizeInboundContext({ Body: body, BodyForAgent: inputBody, RawBody: inputBody,
    CommandBody: commandBody, From: isGroup ? `wechat:group:${message.threadId}` : `wechat:${message.senderId}`,
    To: `wechat:${message.threadId}`, SessionKey: route.sessionKey, AccountId: account.accountId,
    ChatType: isGroup ? "group" : "direct", SenderId: message.senderId, Provider: "agent-wechat", Surface: "agent-wechat",
    MessageSid: `wechat-native:${account.native!.accountId}:${message.id}`, WasMentioned: isGroup ? wasMentioned : undefined,
    CommandAuthorized: core.channel.commands.shouldComputeCommandAuthorized(commandBody, cfg) ? ingress.commandAccess.authorized : undefined,
    OriginatingChannel: "agent-wechat", OriginatingTo: `wechat:${message.threadId}`,
    ...(preparedMedia.mediaPath ? { MediaPath: preparedMedia.mediaPath, MediaUrl: preparedMedia.mediaPath, MediaType: preparedMedia.mediaMime } : {}),
  });
  await core.channel.session.recordInboundSession({ storePath, sessionKey: route.sessionKey, ctx });
  const pipeline = (params.createPipeline ?? createChannelMessageReplyPipeline)({ cfg, agentId: route.agentId, channel: "agent-wechat", accountId: account.accountId });
  const { onModelSelected, ...prefixOptions } = pipeline;
  // Persist before making a model-side effect. A crash in this window is uncertain,
  // and reopening holds this item for review rather than duplicating a model turn.
  const dispatchManifest = nativeDispatchManifest({ message, inputBody, sessionKey: route.sessionKey,
    agentId: route.agentId, channelAccountId: account.accountId, policy, assets: preparedMedia.suppliedAssets ?? [] });
  state.update(message.id, { state: "dispatching", dispatchManifest });
  const callback = message.type === 1 || (preparedMedia.attachment?.status === "ready" && preparedMedia.suppliedAssets?.length)
    ? state.preparedCallback([message.id], account.native?.readPolicy ?? "on_model_dispatch") : undefined;
  if (callback) trackNativePreparedInput(dispatchManifest, callback);
  let failed = false; let delivery = 0;
  try {
    await core.channel.reply.dispatchReplyWithBufferedBlockDispatcher({ ctx, cfg,
      dispatcherOptions: { ...prefixOptions,
        deliver: async (payload: any) => {
          const urls: string[] = payload.mediaUrls?.length ? payload.mediaUrls : payload.mediaUrl ? [payload.mediaUrl] : [];
          if (urls.length > 16) throw new Error("Native reply media count exceeds its bound");
          if (payload.audioAsVoice && !urls.length) throw new Error("Native voice reply requires an audio attachment");
          for (const url of urls) { const key = createHash("sha256").update(`${account.native!.accountId}\0${message.id}\0${delivery++}`).digest("hex"); await sendNativeAttachment(client, account, message.threadId, url, payload.audioAsVoice === true, key); }
          if (payload.text) { const key = createHash("sha256").update(`${account.native!.accountId}\0${message.id}\0${delivery++}`).digest("hex"); await submitNativeText(client, account.native!.accountId, message.threadId, payload.text, key); }
        }, onError: () => { failed = true; } },
      // This hook is intentionally separate from onModelSelected and old
      // onAgentRunStart (which can also report direct-tool execution). Released
      // hosts without it need the separately verified text input/model-call
      // and media-snapshot bridge; unsupported or ambiguous handoffs leave reads unchanged.
      replyOptions: { runId: dispatchManifest.runId, onModelSelected, onPreparedAgentRunStart: callback },
    });
    const prepared = Boolean(state.get(message.id)?.runId);
    state.update(message.id, { state: failed ? "needs_review" : "completed", handoffUnconfirmed: !prepared });
    return failed ? "needs_review" : "completed";
  } catch {
    state.update(message.id, { state: "needs_review" }); return "needs_review";
  }
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
  const flushRead = () => readTask ??= projectNativeRead(host.client, native.accountId, state).finally(() => { readTask = undefined; });
  const process = async () => {
    const cfg = core.config.current();
    const account = resolveWeChatAccount(cfg as Record<string, unknown>, opts.account.accountId) ?? opts.account;
    if (account.backend !== "native" || JSON.stringify(account.native) !== JSON.stringify(native)) throw new Error("Native service configuration changed; restart required before processing");
    for (const item of state.items().filter(value => value.state === "received")) {
      await dispatchNativeMessage({ client: host.client, state, account, message: item.message, cfg, core });
    }
    try { await flushRead(); }
    catch { opts.log?.error?.("Native read projection is pending; exact input IDs retained for retry"); }
    const needsReview = state.items().filter(value => value.state === "needs_review").length;
    const handoffUnconfirmed = native.readPolicy !== "never" && state.items().some(value => value.handoffUnconfirmed);
    const accountStatus = await host.client.account(native.accountId);
    opts.setStatus({ accountId: account.accountId, running: true, connected: true,
      linked: accountStatus.authenticated === true && accountStatus.connection === "online", needsReview, handoffUnconfirmed });
  };
  await process();
  // Media confirmation can arrive after the dispatcher settles. Retry only
  // durable exact-ID reads while an otherwise quiet SSE stream remains open.
  const stopReadPump = startNativeReadPump(async () => {
    await flushRead();
    if (!opts.abortSignal.aborted) opts.setStatus({ accountId: opts.account.accountId,
      handoffUnconfirmed: native.readPolicy !== "never" && state.items().some(value => value.handoffUnconfirmed) });
  }, () => opts.log?.error?.("Native read projection remains pending; exact input IDs retained"));
  try { while (!opts.abortSignal.aborted) {
    try {
      // A prior ACK failure must not leave durable received items idle while an
      // otherwise quiet SSE connection waits for another message.
      if (state.cursor !== "0") await host.client.ack(native.accountId, "openclaw", state.cursor);
      await process();
      for await (const event of host.client.watch(native.accountId, state.cursor, opts.abortSignal)) {
        let message: NativeMessage | undefined;
        if (event.kind === "message.committed") {
          if (typeof event.data.messageId !== "string") throw new Error("Native committed message identity is missing; cursor retained");
          message = await host.client.message(native.accountId, event.data.messageId);
          if (message.threadId !== event.data.threadId) throw new Error("Native committed message thread identity mismatch");
        }
        state.accept(event.cursor, message, typeof event.data.origin === "string" ? event.data.origin : "unknown");
        await host.client.ack(native.accountId, "openclaw", state.cursor);
        await process(); backoff = 250;
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
