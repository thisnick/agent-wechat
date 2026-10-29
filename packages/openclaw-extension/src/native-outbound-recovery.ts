import type { ChannelMessageDurableFinalAdapter } from "openclaw/plugin-sdk/channel-outbound";
import type { NativeMessage } from "@agent-wechat/shared";
import { createMessageReceiptFromOutboundResults } from "openclaw/plugin-sdk/channel-outbound";
import { ensureNativeHost } from "./native-host.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { nativePayloadMediaOptions } from "./outbound-options.ts";
import { validateWeChatMentionSend, type WeChatTextMentions } from "./outbound-mentions.ts";
import { nativeOutboundKey } from "./native-send-key.ts";
import { nativeOutboundDelivery } from "./native-send.ts";
import { nativeReplyMessageId, nativeReplyReference, parseWeChatReplyTo, validateWeChatReplySend } from "./outbound-reply.ts";

function recoveredTextMatches(message: NativeMessage, text: string, mentions?: WeChatTextMentions): boolean {
  if (typeof message.body !== "string") return false;
  if (!mentions) return message.body === text;
  const expectedIds = mentions.userIds ?? [];
  const actual = message.mentions;
  if (actual?.state !== "known" || actual.all !== (mentions.all === true)
    || !Array.isArray(actual.userIds) || actual.userIds.length !== expectedIds.length
    || actual.userIds.some((id, index) => id !== expectedIds[index])) return false;
  // OpenClaw submits recipient IDs without editor positions. The stored body
  // has exactly one leading token per recipient, followed by the original text.
  // Keep the saved labels; today's group names cannot establish an old send.
  let offset = 0;
  for (let index = 0; index < (mentions.all ? 1 : expectedIds.length); index++) {
    if (message.body[offset] !== "@") return false;
    const end = message.body.indexOf("\u2005", offset + 1);
    if (end < 0) return false;
    const label = message.body.slice(offset + 1, end);
    if (!label.trim() || new TextEncoder().encode(label).length > 1024
      || /[\u0000-\u001f\u007f-\u009f\ud800-\udfff]/u.test(label)) return false;
    offset = end + 1;
  }
  return message.body.slice(offset) === text;
}

/** Queue recovery reads the original send only. It never submits another one. */
export function createWeChatDurableFinal(host: typeof ensureNativeHost = ensureNativeHost): ChannelMessageDurableFinalAdapter {
  return {
    capabilities: { text: true, media: true, payload: true, replyTo: true, messageSendingHooks: true, reconcileUnknownSend: true },
    automaticUnknownSendReconciliation: true,
    reconcileUnknownSendKinds: { text: true, payload: true },
    reconcileUnknownSend: async ctx => {
      const unresolved = { status: "unresolved" as const, retryable: false,
        error: "Inspect the original WeChat send receipt; do not send again with a new key." };
      try {
        if (ctx.channel !== "agent-wechat" || !ctx.queueId || ctx.payloads.length !== 1) return unresolved;
        const payload = ctx.payloads[0];
        const options = nativePayloadMediaOptions(payload);
        const replyToId = parseWeChatReplyTo(ctx.effectiveReplyToId ?? ctx.replyToId ?? payload.replyToId);
        if (payload.mediaUrl || payload.mediaUrls?.length || payload.audioAsVoice || payload.videoAsNote || payload.location
          || payload.presentation || payload.interactive || options.videoQuality != null
          || typeof payload.text !== "string" || !payload.text.trim()) return unresolved;
        validateWeChatMentionSend(options.mentions, { ...payload, to: ctx.to, replyToId });
        validateWeChatReplySend(replyToId, payload);
        const account = resolveWeChatAccount(ctx.cfg as Record<string, unknown>);
        if (!account?.enabled || !account.native || ctx.accountId && ctx.accountId !== account.accountId) return unresolved;
        const configurationKey = nativeConfigurationKey(account.native);
        const current = () => {
          const selected = resolveWeChatAccount(ctx.cfg as Record<string, unknown>);
          return selected?.enabled && selected.native && nativeConfigurationKey(selected.native) === configurationKey;
        };
        const { client } = await host(account.native);
        if (!current()) return unresolved;
        const intent = await client.sendByKey(account.native.accountId, nativeOutboundKey(ctx.queueId));
        if (!current() || !("destination" in intent) || intent.accountId !== account.native.accountId || intent.destination !== ctx.to
          || !["accepted", "sent", "server_accepted"].includes(intent.state) || !intent.localMessageId || !intent.serverId) return unresolved;
        // A receipt for one text chunk cannot acknowledge the complete payload,
        // including when the daemon added mention labels before its text.
        const message = await client.message(account.native.accountId, intent.localMessageId);
        if (!current() || message.id !== intent.localMessageId || message.accountId !== account.native.accountId
          || message.threadId !== ctx.to || message.direction !== "outgoing" || message.serverId !== intent.serverId || message.recall
          || !recoveredTextMatches(message, payload.text, options.mentions)) return unresolved;
        if (replyToId) {
          // Original recall may happen after an accepted reply. Read its
          // identity only; recovery never rebuilds or sends quoted content.
          const original = await client.message(account.native.accountId, nativeReplyMessageId(replyToId, account.native.accountId));
          const quote = nativeReplyReference(original, account.native.accountId, ctx.to, replyToId);
          if (!current() || message.type !== 49 || message.appMessageType !== 57 || message.quote?.serverId !== quote.serverId) return unresolved;
        } else if (message.type !== 1 || message.quote) return unresolved;
        const delivery = await nativeOutboundDelivery(client, account.native.accountId, ctx.to, [intent.localMessageId],
          { sendId: intent.id, state: intent.state, serverIds: [intent.serverId] });
        if (!current() || delivery.meta.outcome !== "sent" || !delivery.messageId) return unresolved;
        const receipt = createMessageReceiptFromOutboundResults({ results: [{ channel: "agent-wechat", ...delivery }], kind: "text",
          threadId: ctx.threadId == null ? undefined : String(ctx.threadId) });
        return { status: "sent", receipt, messageId: delivery.messageId };
      } catch { return unresolved; }
    },
  };
}
