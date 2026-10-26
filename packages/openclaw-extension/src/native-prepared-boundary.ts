import { createHash, randomUUID } from "node:crypto";
import { basename, isAbsolute } from "node:path";
import type { OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import { NATIVE_MAX_SOURCE_BYTES, type NativeMessage } from "@agent-wechat/shared";
import type { NativeDispatchManifest, NativeSuppliedAsset } from "./native-consumer-state.ts";

type Proof = { sessionId: string; provider: string; model: string };
type Pending = { manifest: NativeDispatchManifest; record: (runId: string) => unknown; expires: number; proof?: Proof; started?: boolean };
const pending = new Map<string, Pending>();
let enabled = false;
const digest = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
function prune(): void { for (const [id, item] of pending) if (item.expires < Date.now()) pending.delete(id); }
/** Source-local IDs can be reused; host input deduplication needs the exact target. */
export function nativeSourceMessageSid(message: NativeMessage): string {
  return `wechat-native:${message.accountId}:${message.id}:${digest(JSON.stringify([message.threadId, message.serverId ?? null]))}`;
}

/** Persist this immutable selection before entering the SDK. Content stays in
 * the private custody store; this record binds versions, roles and representations. */
export function nativeDispatchManifest(params: {
  message: NativeMessage; inputBody: string; sessionKey: string; agentId: string; channelAccountId: string;
  policy?: unknown; assets?: readonly NativeSuppliedAsset[];
  history?: readonly NativeMessage[];
}): NativeDispatchManifest {
  const history = params.history ?? [];
  if (history.length > 200 || history.some(message => message.accountId !== params.message.accountId || message.threadId !== params.message.threadId || message.id === params.message.id)) throw Error("Invalid native history manifest");
  const assets = Object.freeze((params.assets ?? []).map(asset => {
    if (typeof asset.id !== "string" || !asset.id || asset.id.length > 2048 || !/^[a-f0-9]{64}$/.test(asset.sha256) || !Number.isSafeInteger(asset.byteLength) || asset.byteLength < 1
      || asset.byteLength > (asset.kind === "file" ? NATIVE_MAX_SOURCE_BYTES : 128 * 1024 * 1024) || typeof asset.path !== "string" || !isAbsolute(asset.path) || asset.path.length > 4096
      || typeof asset.mime !== "string" || asset.mime.length > 128 || !/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(asset.mime)
      || !["image", "audio", "video", "file"].includes(asset.kind) || !["model_image", "file_reference"].includes(asset.role)
      || (asset.role === "model_image" && (asset.kind !== "image" || !asset.mime.startsWith("image/") || !asset.runtimeMedia))
      || !["trigger", "history"].includes(asset.contextRole)
      || (asset.sourceMessageId != null && (typeof asset.sourceMessageId !== "string" || !asset.sourceMessageId || asset.sourceMessageId.length > 8192
        || ![params.message, ...history].some(message => message.id === asset.messageId && message.type === 49 && message.appMessageType === 57 && message.quote?.type === 3)))
      || (asset.contextRole === "history" && !history.some(message => message.id === asset.messageId))) throw Error("Invalid native supplied asset manifest");
    return Object.freeze({ ...asset });
  }));
  const messageSid = nativeSourceMessageSid(params.message);
  const manifest = { schemaVersion: 2 as const, runId: randomUUID(), accountId: params.message.accountId,
    messageIds: Object.freeze([params.message.id]), messageSid,
    threadId: params.message.threadId, senderId: params.message.senderId ?? "", sessionKey: params.sessionKey, agentId: params.agentId,
    channelAccountId: params.channelAccountId, inputBodySha256: digest(params.inputBody),
    newInputs: Object.freeze([Object.freeze({ id: params.message.id, version: params.message.version ?? "",
      contentSha256: digest(JSON.stringify([params.message.id, params.message.version, params.message.senderId, params.message.type,
        params.message.body, params.message.mentions, params.message.serverId, params.message.appMessageType, params.message.quote, params.message.link])), role: "trigger" as const })]),
    ...(history.length ? { historyInputs: Object.freeze(history.map(message => Object.freeze({ id: message.id, version: message.version ?? "",
      contentSha256: digest(JSON.stringify([message.id, message.version, message.senderId, message.type, message.body, message.mentions, message.serverId, message.appMessageType, message.quote, message.link])), role: "history" as const }))) } : {}),
    assets, policySha256: digest(JSON.stringify(params.policy ?? null)),
    // Exact source-turn identity of OpenClaw 2026.10.2 through 2026.10.7. Requiring it in the later
    // snapshot prevents an identical older image/body from proving this input.
    hostSourceTurnId: `channel-user:v1:${digest(JSON.stringify(["agent-wechat", params.channelAccountId, `wechat:${params.message.threadId}`, messageSid]))}` };
  return Object.freeze({ ...manifest, manifestSha256: digest(JSON.stringify(manifest)) });
}

export function trackNativePreparedInput(manifest: NativeDispatchManifest, record: (runId: string) => unknown): void {
  prune(); if (!enabled || pending.size >= 1024) return;
  pending.set(manifest.runId, { manifest, record, expires: Date.now() + 15 * 60 * 1000 });
}
export const trackNativePreparedText = trackNativePreparedInput;
/** Forget pending proofs after custody invalidation, including quoted assets. */
export function invalidateNativePreparedInputs(accountId: string, messageIds: readonly string[]): void {
  const ids = new Set(messageIds);
  for (const [runId, item] of pending) {
    if (item.manifest.accountId === accountId && [...item.manifest.messageIds,
      ...(item.manifest.historyInputs ?? []).map(input => input.id),
      ...item.manifest.assets.flatMap(asset => asset.sourceMessageId ? [asset.sourceMessageId] : [])].some(id => ids.has(id))) pending.delete(runId);
  }
}

function promptMatches(prompt: unknown, manifest: NativeDispatchManifest): boolean {
  if (typeof prompt !== "string" || Buffer.byteLength(prompt) > 1024 * 1024) return false;
  const text = prompt.replace(/^\[[^\]\r\n]{1,96}\] /, "");
  const candidates = [text];
  // The pinned group llm_input prepends host-generated sender context; the
  // final current-user snapshot omits it. Only this exact sender may be stripped.
  const sender = `Conversation info: ⟦openclaw:ctx⟧\n\`\`\`json\n${JSON.stringify({ sender: { id: manifest.senderId } })}\n\`\`\`\n\n`;
  if (manifest.threadId.endsWith("@chatroom") && text.startsWith(sender)) candidates.push(text.slice(sender.length));
  return candidates.some(text => normalizedPromptMatches(text, manifest));
}
function normalizedPromptMatches(text: string, manifest: NativeDispatchManifest): boolean {
  const runtimeAssets = manifest.assets.filter(asset => asset.runtimeMedia);
  if (runtimeAssets.length) {
    if (runtimeAssets.length !== 1) return false;
    const asset = runtimeAssets[0];
    const header = /^\[media attached: ([^\r\n]+) \(([^)\r\n]+)\) \| (media:\/\/inbound\/[^\]\r\n]+)\]\n/.exec(text);
    if (!header || !isAbsolute(header[1]) || basename(header[1]) !== `input-${basename(asset.path)}`
      || header[2] !== asset.mime || header[3] !== `media://inbound/${basename(asset.path)}`) return false;
    text = text.slice(header[0].length);
    // This pinned, explicit status is supplemental host context. Arbitrary
    // descriptions, transformed/merged input and unknown additions fail closed.
    if (asset.kind === "video") text = text.replace(/\n\n\[Video attachment not analyzed: video understanding is disabled\]$/, "");
  }
  return digest(text) === manifest.inputBodySha256;
}
function currentMessageFacts(messages: unknown): Array<{ messageId: string; chatId: string }> {
  if (!Array.isArray(messages) || messages.length > 1000) return [];
  const facts: Array<{ messageId: string; chatId: string }> = [];
  for (const message of messages) {
    if (message?.role !== "custom" || message.customType !== "openclaw.runtime-context"
      || message.details?.source !== "openclaw-runtime-context" || message.details?.runtimeContextCarrier !== true) continue;
    const fragments = message.details?.fragments;
    if (!Array.isArray(fragments) || fragments.length > 64) return [];
    for (const fragment of fragments) {
      if (fragment?.kind !== "conversation-data" || typeof fragment.text !== "string" || fragment.text.length > 65536) continue;
      const match = /^Conversation info: ⟦openclaw:ctx⟧\n```json\n([^\n]+)\n```$/.exec(fragment.text);
      if (!match) continue;
      try { const value = JSON.parse(match[1]); if (typeof value.message_id === "string" && typeof value.chat_id === "string") facts.push({ messageId: value.message_id, chatId: value.chat_id }); } catch { /* No evidence from malformed metadata. */ }
    }
  }
  return facts;
}
function matchingContext(ctx: any, manifest: NativeDispatchManifest): boolean {
  return ctx?.runId === manifest.runId && ctx?.sessionKey === manifest.sessionKey && ctx?.agentId === manifest.agentId;
}
export function observeNativePreparedInput(event: any, ctx: any): void {
  prune(); const item = pending.get(event?.runId); if (!item) return;
  item.proof = undefined; item.started = false; const m = item.manifest;
  if (!matchingContext(ctx, m) || ctx?.channel !== "agent-wechat" || ctx?.accountId !== m.channelAccountId
    || ctx?.chatId !== (m.threadId.endsWith("@chatroom") ? m.threadId : `wechat:${m.threadId}`) || ctx?.sessionId !== event.sessionId
    || typeof event.sessionId !== "string" || !event.sessionId || typeof event.provider !== "string" || typeof event.model !== "string"
    || !promptMatches(event.prompt, m)) return;
  const facts = currentMessageFacts(event.historyMessages);
  if (facts.length !== 1 || facts[0].messageId !== m.messageSid || facts[0].chatId !== `wechat:${m.threadId}`) return;
  if (m.assets.length && event.imagesCount !== m.assets.filter(asset => asset.role === "model_image").length) return;
  item.proof = { sessionId: event.sessionId, provider: event.provider, model: event.model };
}
export function observeNativeModelCall(event: any, ctx: any): void {
  prune(); const item = pending.get(event?.runId); if (!item?.proof) return;
  const m = item.manifest, proof = item.proof;
  if (!matchingContext(ctx, m) || event.sessionKey !== m.sessionKey || event.sessionId !== proof.sessionId || ctx?.sessionId !== proof.sessionId
    || event.provider !== proof.provider || event.model !== proof.model || typeof event.callId !== "string" || !event.callId) return;
  item.started = true;
  // The input hook is before current images are prepared. A later exact asset
  // snapshot is mandatory for media; a count never establishes identity.
  if (!m.assets.length) { item.record(m.runId); pending.delete(m.runId); }
}
export function observeNativeAgentEnd(event: any, ctx: any): void {
  prune(); const item = pending.get(event?.runId); if (!item?.proof || !item.started || !item.manifest.assets.length) return;
  const m = item.manifest, proof = item.proof;
  if (!matchingContext(ctx, m) || ctx?.sessionId !== proof.sessionId || ctx?.channel !== "agent-wechat" || ctx?.accountId !== m.channelAccountId
    || ctx?.chatId !== (m.threadId.endsWith("@chatroom") ? m.threadId : `wechat:${m.threadId}`) || ctx?.modelProviderId !== proof.provider || ctx?.modelId !== proof.model
    || !Array.isArray(event.messages) || event.messages.length > 1000) return;
  const user = event.messages.findLast((message: any) => message?.role === "user");
  if (user?.idempotencyKey !== m.hostSourceTurnId) return;
  const blocks = typeof user.content === "string" ? [{ type: "text", text: user.content }] : user.content;
  if (!Array.isArray(blocks) || blocks.length > 32) return;
  if (blocks.some((block: any) => !["text", "image"].includes(block?.type) || (block.type === "text" && typeof block.text !== "string"))) return;
  const text = blocks.filter((block: any) => block?.type === "text").map((block: any) => block.text).join("\n");
  if (!promptMatches(text, m)) return;
  const images = blocks.filter((block: any) => block?.type === "image");
  const expectedImages = m.assets.filter(asset => asset.role === "model_image");
  if (images.length !== expectedImages.length) return;
  for (const [index, asset] of expectedImages.entries()) {
    const image = images[index];
    if (image.mimeType !== asset.mime || typeof image.data !== "string" || image.data.length > 8 * 1024 * 1024
      || image.data.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(image.data)) return;
    const bytes = Buffer.from(image.data, "base64");
    if (bytes.length !== asset.byteLength || digest(bytes) !== asset.sha256) return;
  }
  if (m.assets.some(asset => asset.role === "file_reference" && !text.includes(asset.path))) return;
  item.record(m.runId); pending.delete(m.runId);
}

/** Explicit host conversation access is required. No permission is enabled by
 * the plugin, and no private/internal diagnostic payload is acquired. */
export function registerNativePreparedBoundary(api: OpenClawPluginApi): () => void {
  const access = (api.config as any)?.plugins?.entries?.["agent-wechat"]?.hooks?.allowConversationAccess === true;
  enabled = access;
  if (access) { api.on("llm_input", observeNativePreparedInput); api.on("model_call_started", observeNativeModelCall); api.on("agent_end", observeNativeAgentEnd); }
  return () => { enabled = false; pending.clear(); };
}
