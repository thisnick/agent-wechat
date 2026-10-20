import { createHash, randomUUID } from "node:crypto";
import type { OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import type { NativeMessage } from "@agent-wechat/shared";
import type { NativeDispatchManifest } from "./native-consumer-state.ts";

type Proof = { sessionId: string; provider: string; model: string };
type Pending = { manifest: NativeDispatchManifest; record: (runId: string) => unknown; expires: number; proof?: Proof };
const pending = new Map<string, Pending>();
let enabled = false;
const digest = (value: string) => createHash("sha256").update(value).digest("hex");
function prune(): void { for (const [id, item] of pending) if (item.expires < Date.now()) pending.delete(id); }

/** The caller supplies this immutable manifest before entering the SDK. */
export function nativeDispatchManifest(params: {
  message: NativeMessage; inputBody: string; sessionKey: string; agentId: string; channelAccountId: string;
}): NativeDispatchManifest {
  return Object.freeze({ schemaVersion: 1, runId: randomUUID(), accountId: params.message.accountId,
    messageIds: Object.freeze([params.message.id]), messageSid: `wechat-native:${params.message.accountId}:${params.message.id}`,
    threadId: params.message.threadId, sessionKey: params.sessionKey, agentId: params.agentId,
    channelAccountId: params.channelAccountId, inputBodySha256: digest(params.inputBody) });
}

/** Older released hosts can only prove the text boundary below. Media requires
 * the per-dispatch prepared hook until its exact model representation is proven. */
export function trackNativePreparedText(manifest: NativeDispatchManifest, record: (runId: string) => unknown): void {
  prune();
  if (!enabled || pending.size >= 1024) return;
  pending.set(manifest.runId, { manifest, record, expires: Date.now() + 15 * 60 * 1000 });
}

function promptMatches(prompt: unknown, expected: string): boolean {
  if (typeof prompt !== "string" || Buffer.byteLength(prompt) > 1024 * 1024) return false;
  // The pinned host may prepend one current-turn timestamp. No substring match
  // is accepted: merged, omitted or rewritten inputs remain unconfirmed.
  return digest(prompt) === expected || digest(prompt.replace(/^\[[^\]\r\n]{1,96}\] /, "")) === expected;
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

export function observeNativePreparedInput(event: any, ctx: any): void {
  prune();
  const item = pending.get(event?.runId); if (!item) return;
  item.proof = undefined;
  const m = item.manifest;
  if (ctx?.runId !== m.runId || ctx?.sessionKey !== m.sessionKey || ctx?.agentId !== m.agentId
    || ctx?.channel !== "agent-wechat" || ctx?.accountId !== m.channelAccountId
    || ctx?.chatId !== `wechat:${m.threadId}` || ctx?.sessionId !== event.sessionId
    || typeof event.sessionId !== "string" || !event.sessionId || typeof event.provider !== "string" || typeof event.model !== "string"
    || !promptMatches(event.prompt, m.inputBodySha256)) return;
  const facts = currentMessageFacts(event.historyMessages);
  if (facts.length !== 1 || facts[0].messageId !== m.messageSid || facts[0].chatId !== `wechat:${m.threadId}`) return;
  item.proof = { sessionId: event.sessionId, provider: event.provider, model: event.model };
}

export function observeNativeModelCall(event: any, ctx: any): void {
  prune();
  const item = pending.get(event?.runId); if (!item?.proof) return;
  const m = item.manifest, proof = item.proof;
  if (ctx?.runId !== m.runId || ctx?.agentId !== m.agentId || ctx?.sessionKey !== m.sessionKey
    || event.sessionKey !== m.sessionKey || event.sessionId !== proof.sessionId || ctx?.sessionId !== proof.sessionId
    || event.provider !== proof.provider || event.model !== proof.model || typeof event.callId !== "string" || !event.callId) return;
  item.record(m.runId);
  pending.delete(m.runId);
}

/** Explicit host conversation access is required. The adapter never grants it. */
export function registerNativePreparedBoundary(api: OpenClawPluginApi): () => void {
  const access = (api.config as any)?.plugins?.entries?.["agent-wechat"]?.hooks?.allowConversationAccess === true;
  enabled = access;
  if (access) { api.on("llm_input", observeNativePreparedInput); api.on("model_call_started", observeNativeModelCall); }
  return () => { enabled = false; pending.clear(); };
}
