import { createHash } from "node:crypto";
import { posix } from "node:path";
import type { ChannelAgentTool } from "openclaw/plugin-sdk/channel-contract";
import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import type { MediaAsset, PreparedSticker } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { resolveWeChatAccount } from "./types.ts";
import { withStickerFiles } from "./sticker-files.ts";
const name = "wechat_stickers";
function text(value: unknown, label: string): string {
  if (typeof value !== "string" || !value || value.length > 4096 || /[\u0000-\u001f\u007f]/.test(value)) throw Error(`Invalid ${label}`);
  return value;
}
/** The one original sticker attachment of a received message; the daemon only adds that copy. */
async function receivedStickerOriginal(client: { mediaAssets(accountId: string, messageId: string, signal?: AbortSignal): Promise<{ items: MediaAsset[] }> },
  accountId: string, messageId: string, signal?: AbortSignal): Promise<string> {
  const { items } = await client.mediaAssets(accountId, messageId, signal);
  const originals = (Array.isArray(items) ? items : []).filter(item => item.accountId === accountId && item.messageId === messageId
    && item.kind === "sticker" && item.variant === "original");
  if (originals.length !== 1) throw Error(originals.length ? "The message has more than one original sticker" : "The message has no received sticker to add");
  return text(originals[0].id, "asset identity");
}
/** Owner-only custom sticker management and preparation. Files belong to the active execution
 * workspace; tool arguments cannot select a different account, node or SSH host. */
export function createNativeStickerTools(context: OpenClawPluginToolContext,
  dependencies = { host: ensureNativeHost, files: withStickerFiles }): ChannelAgentTool[] {
  if (context.senderIsOwner !== true || !context.assertInvocationCurrent) return [];
  const config = () => context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  const selected = resolveWeChatAccount(config() as Record<string,unknown>);
  if (!selected?.enabled || !selected.native || context.agentAccountId && context.agentAccountId !== selected.accountId
    || context.deliveryContext?.channel === "agent-wechat" && context.deliveryContext.accountId && context.deliveryContext.accountId !== selected.accountId) return [];
  const accountId = selected.native.accountId;
  const binding = nativeConfigurationKey(selected.native);
  return [{ name, label: "WeChat stickers", ...{catalogMode:"direct-only" as const},
    description: "Manage the configured account's custom Saved stickers and prepare image/GIF/MOV/MP4 files from the active execution workspace. Preparation does not save or send. Preview returns actual image content to model vision: sampled GIF frames or a representative still when animation decoding is unavailable. Download stages verified original bytes into the execution workspace. Mutations and missing-original acquisitions require a stable idempotency key; reuse it after timeouts. Official packs are outside this tool.",
    parameters: { type:"object", additionalProperties:false, required:["action"], properties: {
      action:{ type:"string", enum:["list","get","prepare","preview","download","add","move","remove"], description:"List/get Saved, preview/download one Saved or prepared item, prepare a source, add, move, or remove membership." },
      stickerId:{type:"string",description:"Stable Saved sticker identity for get, preview, download, move, or remove."},
      revision:{type:"string",description:"Collection revision returned by list; required for move/remove."},
      afterId:{type:["string","null"],description:"Required for move: place after this Saved identity, or null for first."},
      messageId:{type:"string",description:"Received message identity when adding a received original instead of a preparation."},
      assetId:{type:"string",description:"Verified original sticker asset belonging to messageId for add."},
      preparationId:{type:"string",description:"Preparation job identity for preview/download; returned by prepare."},
      file:{type:"string",description:"Workspace-relative source file for prepare; resolved on the session execution host."},
      sourceId:{type:"string",description:"Existing daemon-owned upload instead of a workspace file."},
      output:{type:"string",description:"New workspace-relative output for download, using the original format extension; existing files are never replaced."},
      fit:{type:"boolean",description:"Permit proportional resizing to the sticker output limit."},
      startMs:{type:"integer",minimum:0,maximum:4294967295,description:"Explicit segment start in milliseconds; requires durationMs."},
      durationMs:{type:"integer",minimum:1,maximum:10000,description:"Explicit retained duration in milliseconds; omitted means preserve duration."},
      idempotencyKey:{type:"string",minLength:1,maxLength:200,description:"Required stable key for prepare/add/move/remove and downloads that need acquisition. Reuse the key and identical intent after uncertain responses."},
    } } as ChannelAgentTool["parameters"],
    execute: async (_id,input:unknown,signal?:AbortSignal) => {
      if (!input || typeof input !== "object" || Array.isArray(input)) throw Error("Invalid sticker arguments");
      const args = input as Record<string,unknown>;
      const fields = ["action","stickerId","preparationId","file","sourceId","output","fit","startMs","durationMs","idempotencyKey","revision","afterId","messageId","assetId"];
      if (Object.keys(args).some(key=>!fields.includes(key))) throw Error("Unknown sticker argument");
      const guard = () => {
        signal?.throwIfAborted(); context.assertInvocationCurrent!();
        const account = resolveWeChatAccount(config() as Record<string,unknown>);
        if (context.senderIsOwner !== true || !account?.enabled || !account.native || account.accountId !== selected.accountId
          || account.native.accountId !== accountId || nativeConfigurationKey(account.native) !== binding) throw Error("WeChat account configuration changed; start a new turn");
      };
      guard();
      const {client} = await dependencies.host(selected.native!); guard();
      const result = (details: unknown) => ({content:[{type:"text" as const,text:JSON.stringify(details)}],details});
      if (args.action === "list") { const saved=await client.customStickers(accountId,signal); guard(); return result(saved); }
      if (args.action === "get") { const saved=await client.customSticker(accountId,text(args.stickerId,"sticker identity"),signal); guard(); return result(saved); }
      if (["add","move","remove"].includes(String(args.action))) {
        const key=text(args.idempotencyKey,"idempotency key"); if(key.length>200) throw Error("Invalid idempotency key");
        guard(); context.assertInputCommitAllowed?.();
        let job;
        if(args.action === "add") {
          if ((args.preparationId != null) === (args.messageId != null || args.assetId != null)) throw Error("Provide a preparation or a received sticker message");
          let source;
          if (args.preparationId != null) source={kind:"prepared" as const,preparationId:text(args.preparationId,"preparation identity")};
          else {
            const messageId=text(args.messageId,"message identity");
            // Received messages do not show asset identities to the model, so find the message's original sticker.
            const assetId=args.assetId != null ? text(args.assetId,"asset identity") : await receivedStickerOriginal(client,accountId,messageId,signal);
            guard(); context.assertInputCommitAllowed?.();
            source={kind:"message" as const,messageId,assetId};
          }
          job=await client.addCustomSticker(accountId,{source},key,signal);
        } else {
          const stickerId=text(args.stickerId,"sticker identity"),revision=text(args.revision,"collection revision");
          if(args.action === "remove") job=await client.removeCustomSticker(accountId,stickerId,revision,key,signal);
          else {
            if(!Object.hasOwn(args,"afterId")) throw Error("Provide afterId, or null for first");
            const afterId=args.afterId === null ? null : text(args.afterId,"placement anchor");
            job=await client.moveCustomSticker(accountId,stickerId,{position:{afterId}},revision,key,signal);
          }
        }
        guard(); return result({...job,idempotencyKey:key});
      }
      if (args.action === "prepare") {
        const key=text(args.idempotencyKey,"idempotency key");
        if (key.length > 200 || args.fit != null && typeof args.fit !== "boolean") throw Error("Invalid sticker preparation options");
        if (args.startMs != null && typeof args.startMs !== "number" || args.durationMs != null && typeof args.durationMs !== "number") throw Error("Invalid sticker segment");
        const trim = args.durationMs == null ? null : {startMs:Number(args.startMs ?? 0),durationMs:Number(args.durationMs)};
        if (args.startMs != null && !trim || trim && (!Number.isInteger(trim.startMs) || trim.startMs<0 || trim.startMs>4294967295
          || !Number.isInteger(trim.durationMs) || trim.durationMs<1 || trim.durationMs>10000)) throw Error("Invalid sticker segment");
        if ((args.file == null) === (args.sourceId == null)) throw Error("Provide one workspace file or sourceId");
        let sourceId:string;
        if (args.file != null) {
          const file=text(args.file,"source file");
          sourceId=await dependencies.files(context,guard,async files=>{
            const bytes=await files.read(file,64*1024*1024); guard(); context.assertInputCommitAllowed?.();
            const source=await client.upload(accountId,bytes,posix.basename(file),"application/octet-stream",signal); guard(); return source.id;
          },signal);
        } else sourceId=text(args.sourceId,"source identity");
        guard(); context.assertInputCommitAllowed?.();
        const job=await client.prepareSticker(accountId,{sourceId,fit:args.fit===true,trim},key,signal); guard();
        return result({...job,idempotencyKey:key});
      }
      if (args.action !== "preview" && args.action !== "download") throw Error("Unknown sticker action");
      if(args.stickerId != null) {
        if(args.preparationId != null) throw Error("Choose one Saved or prepared item");
        const stickerId=text(args.stickerId,"sticker identity");
        const item=await client.customSticker(accountId,stickerId,signal);guard();
        if(item.id !== stickerId || !item.original || item.original.accountId !== accountId) throw Error("Saved sticker original is unavailable");
        const asset=item.original;
        if(asset.state !== "available") {
          if(args.action === "preview") throw Error("Saved visual preview is unavailable; download the original first");
          const key=text(args.idempotencyKey,"idempotency key");guard();context.assertInputCommitAllowed?.();
          const job=await client.downloadSticker(accountId,{stickerId},key,signal);guard();
          return result({...job,idempotencyKey:key,staged:false});
        }
        const visual=args.action === "preview";
        if(visual && !asset.preview) throw Error("Saved visual preview delivery is unsupported for this original");
        const described=visual ? asset.preview! : asset;
        const media=await client.mediaContent(accountId,asset.id,{maxBytes:16*1024*1024,representation:visual?"preview":"original",signal});guard();
        const bytes=Buffer.from(media.bytes),hash=createHash("sha256").update(bytes).digest("hex"),mime=media.contentType.split(";")[0];
        if(hash !== described.sha256 || bytes.length !== described.byteLength || mime !== described.contentType) throw Error("Saved sticker media integrity failed");
        if(visual) {
          if(!["image/png","image/jpeg"].includes(mime)) throw Error("Saved visual image delivery is unsupported");
          const details={stickerId,state:"available",source:asset.contentType === "image/gif"?"sampled_frames":"representative_still",originalContentType:asset.contentType};
          return {content:[{type:"text" as const,text:JSON.stringify(details)},{type:"image" as const,data:bytes.toString("base64"),mimeType:mime}],details};
        }
        const output=text(args.output,"output file");
        const extensions:Record<string,string[]>={"image/gif":[".gif"],"image/png":[".png"],"image/jpeg":[".jpg",".jpeg"],"image/webp":[".webp"],"image/x-wxgf":[".wxgf"]};
        if(!extensions[mime]?.includes(posix.extname(output).toLowerCase())) throw Error("Output extension must match the original sticker format");
        await dependencies.files(context,guard,async files=>{context.assertInputCommitAllowed?.();await files.create(output,bytes);},signal);guard();
        return result({stickerId,state:"available",path:output,contentType:mime,byteLength:bytes.length,sha256:hash});
      }
      const preparationId=text(args.preparationId,"preparation identity");
      const job=await client.stickerPreparation(accountId,preparationId,signal); guard();
      if (job.accountId !== accountId || job.id !== preparationId || job.kind !== "sticker_prepare") throw Error("Sticker preparation identity mismatch");
      if (job.state !== "available") return result(job);
      const prepared=job.result as PreparedSticker;
      if (!prepared || prepared.id !== preparationId) throw Error("Invalid prepared sticker result");
      const asset:MediaAsset = args.action === "download" ? prepared.original : prepared.contactSheet;
      if (!asset || asset.accountId !== accountId || asset.jobId !== preparationId || asset.state !== "available" || !asset.sha256) throw Error("Prepared sticker media identity mismatch");
      const media=await client.mediaContent(accountId,asset.id,{maxBytes:16*1024*1024,signal}); guard();
      const hash=createHash("sha256").update(media.bytes).digest("hex");
      if (hash !== asset.sha256 || media.bytes.length !== asset.byteLength) throw Error("Prepared sticker media integrity failed");
      const bytes=Buffer.from(media.bytes);
      if (args.action === "preview") {
        if (media.contentType.split(";")[0] !== "image/png" || !bytes.subarray(0,8).equals(Buffer.from("89504e470d0a1a0a","hex"))) throw Error("Sticker visual preview is unavailable");
        const details={preparationId,source:"sampled_frames",width:prepared.width,height:prepared.height,durationMs:prepared.durationMs,transformations:prepared.transformations};
        return {content:[{type:"text" as const,text:JSON.stringify(details)},{type:"image" as const,data:bytes.toString("base64"),mimeType:"image/png"}],details};
      }
      const output=text(args.output,"output file");
      if (!output.toLowerCase().endsWith(".gif") || media.contentType.split(";")[0] !== "image/gif" || !["GIF87a","GIF89a"].includes(bytes.subarray(0,6).toString("ascii"))) throw Error("Prepared original must be exported as a GIF");
      await dependencies.files(context,guard,async files=>{context.assertInputCommitAllowed?.();await files.create(output,bytes);},signal); guard();
      return result({state:"available",path:output,contentType:"image/gif",byteLength:bytes.length,sha256:hash,preparationId});
    },
  }];
}
export function registerNativeStickerTools(api:OpenClawPluginApi):void {
  api.registerTool({contextVersion:2,create:context=>createNativeStickerTools(context)},{names:[name]});
}
