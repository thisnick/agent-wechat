import { createHash } from "node:crypto";
import { randomUUID } from "node:crypto";
import { resolveWeChatAccount } from "./types.ts";
import { ensureNativeHost } from "./native-host.ts";
import { sendNativeAttachment } from "./native-media.ts";
import { submitNativeText } from "./native-send.ts";

export async function sendWeChatMedia(
  cfg: unknown,
  to: string,
  text: string,
  mediaUrl: string | undefined,
  audioAsVoice = false,
  idempotencyKey?: string,
): Promise<string> {
  const account = resolveWeChatAccount(cfg as Record<string, unknown>);
  if (!account?.native) throw new Error("No native daemon configured");
  const { client } = await ensureNativeHost(account.native);
  const key = idempotencyKey ?? randomUUID();
  if (!mediaUrl) {
    if (audioAsVoice) throw new Error("Voice sending requires an audio attachment");
    const send = await submitNativeText(client, account.native.accountId, to, text, key);
    return `agent-wechat-native:${account.native.accountId}:${send.id}`;
  }
  const result = await sendNativeAttachment(client, account, to, mediaUrl, audioAsVoice, key);
  if (text) await submitNativeText(client, account.native.accountId, to, text, createHash("sha256").update(`${key}\0caption`).digest("hex"));
  return result;
}
