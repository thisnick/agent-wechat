import { createHash } from "node:crypto";
import type { ChannelMessageActionContext } from "openclaw/plugin-sdk/channel-contract";
import { resolveWeChatAccount } from "./types.ts";
import { ensureNativeHost } from "./native-host.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { normalizeWeChatTarget } from "./native-target.ts";
import { nativeOutboundDelivery } from "./native-send.ts";

/** Send exactly one Saved reference through the host's standard sticker action.
 * The daemon owns recovery; pending jobs never become fabricated chat receipts. */
export async function handleNativeStickerAction(ctx: ChannelMessageActionContext, dependencies = { host: ensureNativeHost }) {
  if (ctx.action !== "sticker" || ctx.senderIsOwner !== true) throw Error("WeChat Saved sticker sends require the account owner");
  const ids=ctx.params.stickerId;
  if (!Array.isArray(ids) || ids.length!==1 || typeof ids[0]!=="string" || !/^[0-9a-f]{32}$/.test(ids[0])) throw Error("Provide exactly one Saved sticker identity");
  if (ctx.params.message || ctx.params.media || ctx.params.mediaUrl || ctx.params.replyTo || ctx.params.channelData) throw Error("Send a Saved sticker separately from text, media and quoted replies");
  const to=typeof ctx.params.to==="string" ? normalizeWeChatTarget(ctx.params.to) : undefined;
  if (!to || to.length>4096 || /[\u0000-\u001f\u007f]/.test(to)) throw Error("Invalid WeChat sticker destination");
  const account=resolveWeChatAccount(ctx.cfg,ctx.accountId ?? undefined);
  if (!account?.enabled || !account.native) throw Error("No native WeChat account configured");
  const binding=nativeConfigurationKey(account.native);
  // The host's message tool passes assertDirectAdapterHandoff on every turn but
  // onPlatformSendDispatch only on scheduled turns, as for ordinary sends.
  const guard=async()=> {
    if (ctx.senderIsOwner!==true || !ctx.assertDirectAdapterHandoff) throw Error("WeChat sticker send requires live host handoff authority");
    await ctx.onPlatformSendDispatch?.();ctx.assertDirectAdapterHandoff();
    const current=resolveWeChatAccount(ctx.cfg,ctx.accountId ?? undefined);
    if (!current?.enabled || !current.native || current.accountId!==account.accountId || nativeConfigurationKey(current.native)!==binding) throw Error("WeChat account configuration changed");
  };
  if (ctx.dryRun) {
    const details={dryRun:true,accountId:account.native.accountId,to,stickerId:ids[0]};
    return {content:[{type:"text" as const,text:JSON.stringify(details)}],details};
  }
  const supplied=ctx.params.idempotencyKey;
  if (typeof supplied!=="string" || !supplied || supplied.length>8192 || /[\u0000-\u001f\u007f]/.test(supplied)) throw Error("A stable sticker idempotency key is required");
  const key=createHash("sha256").update(JSON.stringify(["wechat-sticker",account.accountId,account.native.accountId,supplied])).digest("hex");
  await guard();
  const {client}=await dependencies.host(account.native);
  await guard();
  const job=await client.sendSticker(account.native.accountId,{kind:"sticker",threadId:to,source:{kind:"saved",stickerId:ids[0]}},key);
  await guard();
  if (job.accountId!==account.native.accountId || job.threadId!==to || job.kind!=="sticker_send") throw Error(`Sticker job identity mismatch; retain idempotency key ${key}`);
  let delivery;
  if (job.state==="available") {
    const result=job.result && typeof job.result==="object" && !Array.isArray(job.result) ? job.result as Record<string,unknown> : {};
    delivery=await nativeOutboundDelivery(client,account.native.accountId,to,[result.messageId],{jobId:job.id,state:job.state,serverIds:typeof result.serverMessageId==="string" ? [result.serverMessageId] : undefined});
    await guard();
  }
  const failed=["failed","cancelled","needs_review","unsupported","expired"].includes(job.state);
  const details={job:{id:job.id,accountId:job.accountId,threadId:job.threadId,kind:job.kind,state:job.state,phase:job.phase,reason:job.reason,retryable:job.retryable},idempotencyKey:key,status:delivery?.meta.outcome==="sent" ? "sent" : failed || delivery?.meta.outcome==="unknown" ? "needs_review" : "pending",...(delivery?.meta.outcome==="sent" && {delivery}),
    note:"Inspect the durable sticker job before sending again; reuse the same idempotency key."};
  return {content:[{type:"text" as const,text:JSON.stringify(details)}],details,...(failed && {isError:true})};
}
