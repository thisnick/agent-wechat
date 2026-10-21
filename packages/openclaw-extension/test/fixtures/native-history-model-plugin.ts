// Runs the actual SDK turn/tool pipeline with synthetic account data only.
import base from "../../index.ts";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { dispatchNativeMessage, projectNativeRead } from "../../src/native-monitor.ts";
import { NativeConsumerState } from "../../src/native-consumer-state.ts";
import { ensureNativeHost } from "../../src/native-host.ts";
import { resolveWeChatAccount } from "../../src/types.ts";
import { prepareNativeAttachment } from "../../src/native-media.ts";
import type { NativeMessage } from "@agent-wechat/shared";

export default { ...base, register(api: any) {
  base.register(api);
  const directory = process.env.NATIVE_HISTORY_FIXTURE_DIR;
  if (!directory) throw Error("Isolated history fixture directory required");
  api.registerService({ id: "native-history-model-validation", start: async () => { setTimeout(async () => {
    try {
      const cfg = api.runtime.config.current(), account = resolveWeChatAccount(cfg)!;
      const { client } = await ensureNativeHost(account.native!);
      const consumerDirectory = join(directory, "probe-consumer");
      let state = new NativeConsumerState(consumerDirectory, "fixture", "a"); let sequence = 0;
      const contexts: any[] = [], outcomes: Record<string, string> = {};
      const core = { ...api.runtime, channel: { ...api.runtime.channel, reply: { ...api.runtime.channel.reply,
        finalizeInboundContext: (ctx: any) => { contexts.push(structuredClone(ctx)); return api.runtime.channel.reply.finalizeInboundContext(ctx); } } } };
      const scenarios = JSON.parse(readFileSync(join(directory, "scenarios.json"), "utf8"));
      for (const { id, type, threadId = "room@chatroom", mention = false, body = null, appMessageType, quote } of scenarios) {
        const message: NativeMessage = { id, accountId: "a", threadId, senderId: "u1", serverId: String(9000 + sequence), type, body, appMessageType, quote,
          direction: "incoming", createdAt: new Date().toISOString(), version: "v1", media: [],
          mentions: { state: "known", userIds: mention ? ["self"] : [], self: mention, all: false, source: "msgsource.atuserlist" } };
        state.accept(String(++sequence), message, "live");
        outcomes[id] = await dispatchNativeMessage({ client, state, account, message, cfg, core,
          prepareAttachment: params => prepareNativeAttachment({ ...params, waitMs: 0 }) });
        // Restart before the activating mention; buffered context must survive.
        state = new NativeConsumerState(consumerDirectory, "fixture", "a");
        await projectNativeRead(client, "a", state);
      }
      writeFileSync(join(directory, "result.json"), JSON.stringify({ outcomes, contexts, items: state.items() }));
    } catch (error) { writeFileSync(join(directory, "result.json"), JSON.stringify({ error: String(error) })); }
  }, 1000); }, stop: async () => {} });
} };
