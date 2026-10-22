// Actual SDK dispatch against synthetic daemon/media and a local model only.
import base from "../../index.ts";
import { appendFileSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { dispatchNativeMessage, projectNativeRead } from "../../src/native-monitor.ts";
import { prepareNativeAttachment } from "../../src/native-media.ts";
import { NativeConsumerState } from "../../src/native-consumer-state.ts";
import { ensureNativeHost } from "../../src/native-host.ts";
import { resolveWeChatAccount } from "../../src/types.ts";
import type { NativeMessage } from "@agent-wechat/shared";
export default { ...base, register(api: any) {
  base.register(api);
  const directory = process.env.NATIVE_MODEL_FIXTURE_DIR;
  if (!directory) throw Error("Isolated fixture directory required");
  for (const kind of ["llm_input", "model_call_started", "agent_end"]) api.on(kind, (event: unknown, ctx: unknown) => appendFileSync(join(directory, "hooks.jsonl"), JSON.stringify({ kind, event, ctx }) + "\n"));
  api.registerService({ id: "native-media-model-validation", start: async () => { setTimeout(async () => {
    try {
      const cfg = api.runtime.config.current(), account = resolveWeChatAccount(cfg)!;
      const { client } = await ensureNativeHost(account.native!);
      const consumerDirectory = join(directory, "probe-consumer");
      let state = new NativeConsumerState(consumerDirectory, "fixture", "a"), sequence = 0;
      const outcomes: Record<string, string> = {};
      const core = api.runtime;
      const scenarios = JSON.parse(readFileSync(join(directory, "scenarios.json"), "utf8"));
      for (const { id, type, threadId = "u1", appMessageType, body = null, link } of scenarios) {
        const message: NativeMessage = { id, accountId: "a", threadId, senderId: "u1", serverId: String(9000 + sequence), type, body, appMessageType, link, direction: "incoming", createdAt: new Date().toISOString(), version: "v1", media: [], mentions: threadId.endsWith("@chatroom") ? { state: "known", userIds: ["self"], self: true, all: false, source: "msgsource.atuserlist" } : { state: "unknown", userIds: [], self: null, all: null, source: null } };
        state.accept(String(++sequence), message, "live");
        outcomes[id] = await dispatchNativeMessage({ client, state, account, message, cfg, core,
          prepareAttachment: params => prepareNativeAttachment({ ...params, waitMs: 0 }) });
        await new Promise(resolve => setTimeout(resolve, 200));
        if (id === "file") {
          // Reopen a committed handoff before projecting its read. A lost read
          // response retains the same exact IDs for another store reopen/retry.
          state = new NativeConsumerState(consumerDirectory, "fixture", "a");
          try { await projectNativeRead(client, "a", state); } catch { /* intentional lost response */ }
          state = new NativeConsumerState(consumerDirectory, "fixture", "a");
          outcomes["duplicate-file"] = await dispatchNativeMessage({ client, state, account, message, cfg, core });
        }
        await projectNativeRead(client, "a", state);
      }
      writeFileSync(join(directory, "result.json"), JSON.stringify({ outcomes, items: state.items().map(item => ({ id: item.message.id, state: item.state, runId: item.runId, readPending: item.readPending, handoffUnconfirmed: item.handoffUnconfirmed, manifest: item.dispatchManifest })) }));
    } catch (error) { writeFileSync(join(directory, "result.json"), JSON.stringify({ error: String(error) })); }
  }, 1000); }, stop: async () => {} });
} };
