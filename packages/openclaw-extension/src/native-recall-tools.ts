import { createHash } from "node:crypto";
import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveStableChannelMessageIngress } from "openclaw/plugin-sdk/channel-ingress-runtime";
import { NativeApiError, type NativeOperation } from "@agent-wechat/shared";
import { resolveWeChatIngress, resolveWeChatPolicyContext } from "./access-control.ts";
import { ensureNativeHost } from "./native-host.ts";
import { getWeChatRuntime } from "./runtime.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { nativeRecallInfo } from "./native-recall-state.ts";

const names = ["wechat_recall"];
function text(value: unknown, label: string): string {
  if (typeof value !== "string" || !value.trim() || value.length > 4096 || /[\u0000-\u001f\u007f]/.test(value)) throw Error(`Invalid ${label}`);
  return value.trim();
}
function thread(value: unknown): string { return text(text(value, "conversation").replace(/^(?:agent-)?wechat:(?:group:)?/i, ""), "conversation"); }
export function nativeRecallKey(accountId: string, threadId: string, messageId: string, serverId: string): string {
  return `openclaw-recall-${createHash("sha256").update(JSON.stringify([accountId, threadId, messageId, serverId])).digest("hex")}`;
}
export function createNativeRecallTools(context: OpenClawPluginToolContext, dependencies: { host?: typeof ensureNativeHost; core?: any; waitMs?: number } = {}): ChannelAgentTool[] {
  const currentConfig = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  if (!currentConfig()) return [];
  const selected = resolveWeChatAccount(currentConfig() as Record<string, unknown>);
  if (!selected?.enabled || !selected.native || context.messageChannel !== "agent-wechat"
    || (context.agentAccountId && context.agentAccountId !== selected.accountId)
    || (context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId)) return [];
  let conversation: string, requester: string;
  try {
    const nativeThread = context.nativeChannelId ? thread(context.nativeChannelId) : undefined;
    const delivered = context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.to ? thread(context.deliveryContext.to) : undefined;
    if (nativeThread && delivered && nativeThread !== delivered) return [];
    conversation = text(nativeThread ?? delivered, "conversation");
    requester = text(context.requesterSenderId, "requester");
  } catch { return []; }
  const accountId = selected.native.accountId;
  const configurationKey = nativeConfigurationKey(selected.native);
  return [{ name: names[0], label: "WeChat Recall",
    description: "Recall one own sent message in the current WeChat conversation by its exact opaque message ID from a verified send receipt or wechat_history, or inspect a retained recalled placeholder without another native mutation. The daemon checks eligibility. Returns the durable operation and confirmed, rejected, pending or uncertain result. Never invent IDs or resubmit an uncertain action with a new key; prior model input and completed actions cannot be erased.",
    parameters: { type: "object", additionalProperties: false, properties: {
      messageId: { type: "string", minLength: 1, maxLength: 4096, description: "Exact stored message ID in this conversation; send/job/operation IDs are not message IDs." },
      serverId: { type: "string", pattern: "^[1-9][0-9]{0,19}$", description: "Exact decimal server ID from the same verified receipt or history entry. The stored local message ID alone may be reused." },
    }, required: ["messageId", "serverId"] } as ChannelAgentTool["parameters"],
    execute: async (_callId, input: unknown, signal?: AbortSignal) => {
      if (!input || typeof input !== "object" || Array.isArray(input) || Object.keys(input).some(key => !["messageId", "serverId"].includes(key))) throw Error("Recall is restricted to a message ID in the current conversation");
      const messageId = text((input as { messageId?: unknown }).messageId, "message ID");
      const serverId = text((input as { serverId?: unknown }).serverId, "server ID");
      if (!/^[1-9]\d{0,19}$/.test(serverId) || BigInt(serverId) > 18446744073709551615n) throw Error("Invalid exact server message ID");
      const guard = () => {
        signal?.throwIfAborted(); context.assertInvocationCurrent?.();
        const cfg = currentConfig(), account = resolveWeChatAccount(cfg as Record<string, unknown>);
        if (!account?.enabled || !account.native || account.accountId !== selected.accountId || account.native.accountId !== accountId
          || nativeConfigurationKey(account.native) !== configurationKey) throw Error("WeChat account configuration changed; start a new turn");
        return { cfg: cfg!, account, native: account.native };
      };
      const allowed = async () => {
        const { cfg, account } = guard(); const core = dependencies.core ?? getWeChatRuntime();
        const ingress = await resolveWeChatIngress({ resolve: core.channel.inbound?.ingress?.resolveStable ?? resolveStableChannelMessageIngress,
          cfg, accountId: account.accountId, policy: resolveWeChatPolicyContext({ account, cfg, chatId: conversation }),
          isGroup: conversation.endsWith("@chatroom"), chatId: conversation, senderId: requester,
          readStoreAllowFrom: async () => core.channel.pairing.readAllowFromStore({ channel: "agent-wechat", accountId: account.accountId, env: process.env }).catch(() => []) });
        guard(); if (ingress.ingress.admission !== "dispatch") throw Error("WeChat conversation access is no longer allowed");
      };
      await allowed(); const { native } = guard();
      const { client } = await (dependencies.host ?? ensureNativeHost)(native); guard();
      const message = await client.message(accountId, messageId); guard();
      const retainedRecall = !!nativeRecallInfo(message);
      if (message.id !== messageId || message.accountId !== accountId || message.threadId !== conversation || (message.direction !== "outgoing" && !retainedRecall)) throw Error("Recall target is outside this conversation or was not sent by this account");
      if (message.serverId !== serverId) throw Error("The exact server identity does not match this receipt; the local message identity may have been reused");
      await allowed(); guard();
      const key = nativeRecallKey(accountId, conversation, messageId, serverId);
      let operation: NativeOperation | undefined;
      let operationId: string | undefined;
      const result = (status: string, extra: Record<string, unknown> = {}) => {
        const details = { messageId, serverId, threadId: conversation, accountId, action: retainedRecall ? "inspect_recalled" : "recall", idempotencyKey: key, operationId,
          operationState: operation?.state, status, retryable: false, ...extra };
        return { content: [{ type: "text" as const, text: JSON.stringify(details) }], details };
      };
      try {
        // Cancellation after submission does not cancel or repeat the daemon-owned action.
        operation = await client.recallMessage(accountId, messageId, serverId, key, signal);
        const checkOperation = (candidate: NativeOperation) => {
          if (!candidate || typeof candidate.id !== "string" || !candidate.id || operationId && candidate.id !== operationId
            || candidate.accountId !== accountId || candidate.kind !== "message_recall") throw Error("Recall operation identity is invalid");
        };
        checkOperation(operation); operationId = operation.id;
        const deadline = Date.now() + Math.max(0, Math.min(dependencies.waitMs ?? 2000, 10000));
        while (["queued", "running", "submitting"].includes(operation.state) && Date.now() < deadline) {
          await new Promise(resolve => setTimeout(resolve, Math.min(250, deadline - Date.now())));
          guard(); const remaining = Math.max(1, deadline - Date.now());
          const next = await client.operation(accountId, operation.id, signal ? AbortSignal.any([signal, AbortSignal.timeout(remaining)]) : AbortSignal.timeout(remaining));
          checkOperation(next); operation = next;
        }
        guard();
      } catch (error) {
        const reason = error instanceof NativeApiError ? error.code : "recall_result_unavailable";
        return result("needs_review", { reason, note: "Inspect the existing operation or repeat only this same message with the same idempotency key. No automatic second submission was made." });
      }
      const receipt = operation.result && typeof operation.result === "object" ? operation.result as Record<string, unknown> : {};
      if (receipt.messageId !== undefined && receipt.messageId !== messageId || receipt.threadId !== undefined && receipt.threadId !== conversation
        || receipt.serverId !== undefined && receipt.serverId !== serverId) throw Error("Recall operation target identity is invalid");
      const exactReceipt = receipt.messageId === messageId && receipt.threadId === conversation && receipt.serverId === serverId;
      const status = operation.state === "succeeded" && exactReceipt && ["recalled", "already_recalled"].includes(String(receipt.status)) ? String(receipt.status)
        : ["queued", "running", "submitting"].includes(operation.state) ? "pending"
        : operation.state === "failed" && exactReceipt && ["not_self", "not_eligible", "unavailable", "not_found"].includes(String(receipt.status)) ? String(receipt.status) : "needs_review";
      const boundedState = (value: unknown) => typeof value === "string" && /^[a-z0-9_]{1,80}$/.test(value) ? value : undefined;
      return result(status, { remoteOutcome: boundedState(receipt.remoteOutcome), profileProjection: boundedState(receipt.profileProjection),
        note: status === "pending" || status === "needs_review" ? "Inspect this durable operation; do not automatically submit a replacement recall." : undefined });
    },
  }];
}
export function registerNativeRecallTools(api: OpenClawPluginApi): void {
  api.registerTool({ contextVersion: 2, create: context => createNativeRecallTools(context) }, { names });
}
