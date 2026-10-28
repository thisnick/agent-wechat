import type { ChannelMessageDurableFinalAdapter } from "openclaw/plugin-sdk/channel-outbound";
import { createMessageReceiptFromOutboundResults } from "openclaw/plugin-sdk/channel-outbound";
import { ensureNativeHost } from "./native-host.ts";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { nativePayloadMediaOptions } from "./outbound-options.ts";
import { validateWeChatMentionSend } from "./outbound-mentions.ts";
import { nativeOutboundKey } from "./native-send-key.ts";
import { nativeOutboundDelivery } from "./native-send.ts";
import { nativeReplyMessageId, nativeReplyReference, parseWeChatReplyTo, validateWeChatReplySend } from "./outbound-reply.ts";

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
        // A plain text payload may have been split by the host. A receipt for
        // its first chunk must not acknowledge the complete logical message.
        if (!options.mentions) {
          const message = await client.message(account.native.accountId, intent.localMessageId);
          if (!current() || message.body !== payload.text) return unresolved;
          if (replyToId) {
            // Original recall may happen after an accepted reply. Read its
            // identity only; recovery never rebuilds or sends quoted content.
            const original = await client.message(account.native.accountId, nativeReplyMessageId(replyToId, account.native.accountId));
            const quote = nativeReplyReference(original, account.native.accountId, ctx.to, replyToId);
            if (!current() || message.type !== 49 || message.appMessageType !== 57 || message.quote?.serverId !== quote.serverId) return unresolved;
          } else if (message.type !== 1 || message.quote) return unresolved;
        }
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
