import type { NativeTextMentionOptions } from "@agent-wechat/shared";

// OpenClaw supplies recipient identities; the daemon supplies visible labels
// and mention metadata. Numeric editor positions are reserved for the UI.
export type WeChatTextMentions = Pick<NativeTextMentionOptions["mentions"], "userIds" | "all">;

export function parseWeChatTextMentions(value: unknown): WeChatTextMentions {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw Error("Invalid WeChat mentions");
  const mentions = value as Record<string, unknown>;
  const ids = mentions.userIds;
  if (Object.keys(mentions).some(key => key !== "userIds" && key !== "all")
    || (mentions.all !== undefined && mentions.all !== true)
    || (mentions.all === true ? ids !== undefined : !Array.isArray(ids) || !ids.length)
    || Array.isArray(ids) && (ids.length > 128 || new Set(ids).size !== ids.length
      || ids.some(id => typeof id !== "string" || !/^[A-Za-z0-9_@.-]{1,256}$/.test(id) || id === "notify@all"))) throw Error("Invalid WeChat mentions");
  return mentions.all === true ? { all: true } : { userIds: [...ids as string[]] };
}

export function validateWeChatMentionSend(mentions: WeChatTextMentions | undefined, input: {
  to: string; text?: string; mediaUrl?: string; mediaUrls?: string[]; audioAsVoice?: boolean;
  forceDocument?: boolean; videoQuality?: string; replyToId?: string | null;
}): void {
  if (!mentions) return;
  parseWeChatTextMentions(mentions);
  if (!input.to.endsWith("@chatroom")) throw Error("WeChat mentions require a group conversation");
  if (typeof input.text !== "string" || !input.text.trim()) throw Error("WeChat mentions require a text message");
  if (input.mediaUrl || input.mediaUrls?.length || input.audioAsVoice || input.forceDocument || input.videoQuality != null) {
    throw Error("WeChat mentions support group text only; do not combine with attachments, voice, document or video quality");
  }
}
