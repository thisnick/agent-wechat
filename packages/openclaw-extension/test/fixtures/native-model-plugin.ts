// Built only by native-model-smoke.mjs into an isolated test plugin.
import base from "../../index.ts";
import { appendFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { dispatchNativeMessage, projectNativeRead } from "../../src/native-monitor.ts";
import { NativeConsumerState } from "../../src/native-consumer-state.ts";
import { ensureNativeHost } from "../../src/native-host.ts";
import { resolveWeChatAccount } from "../../src/types.ts";
import type { NativeMessage } from "@agent-wechat/shared";

export default { ...base, register(api: any) {
  base.register(api);
  const directory = process.env.NATIVE_MODEL_FIXTURE_DIR;
  if (!directory) throw new Error("Model fixture requires isolated output storage");
  let nextStart: (() => void) | undefined;
  const record = (kind: string, event: unknown, ctx: unknown) => appendFileSync(join(directory, "hooks.jsonl"), JSON.stringify({ kind, event, ctx }) + "\n");
  api.on("llm_input", (event: unknown, ctx: unknown) => record("llm_input", event, ctx));
  api.on("model_call_started", (event: unknown, ctx: unknown) => { record("model_call_started", event, ctx); const resolve = nextStart; nextStart = undefined; resolve?.(); });
  api.registerService({ id: "native-model-validation", start: async () => {
    setTimeout(async () => {
      try {
        const cfg = api.runtime.config.current();
        const account = resolveWeChatAccount(cfg)!;
        const { client } = await ensureNativeHost(account.native!);
        const state = new NativeConsumerState(join(directory, "probe-consumer"), "fixture", "a");
        let sequence = 0;
        const message = (id: string, body: string): NativeMessage => ({ id, body, accountId: "a", threadId: "u1", senderId: "u1", serverId: String(9000 + sequence), type: 1,
          direction: "incoming", createdAt: new Date().toISOString(), version: "1", media: [], mentions: { state: "unknown", userIds: [], self: null, all: null, source: null } });
        const dispatch = (id: string, body: string) => {
          const input = message(id, body); state.accept(String(++sequence), input, "live");
          return dispatchNativeMessage({ client, state, account, message: input, cfg, core: api.runtime });
        };
        await dispatch("single", "native-model-single-exact-input");
        state.accept(String(++sequence), message("gap", "native-model-deferred-gap"), "live");
        await projectNativeRead(client, "a", state);
        const started = new Promise<void>(resolve => { nextStart = resolve; });
        const blocking = dispatch("blocking", "native-model-blocking-input");
        await Promise.race([started, new Promise((_, reject) => setTimeout(() => reject(Error("Model start deadline")), 10000))]);
        const queuedA = dispatch("queued-a", "native-model-queued-alpha");
        const queuedB = dispatch("queued-b", "native-model-queued-beta");
        await Promise.all([blocking, queuedA, queuedB]);
        // Deferred collect-mode work can outlive its dispatch promise.
        await new Promise(resolve => setTimeout(resolve, 3500));
        await projectNativeRead(client, "a", state);
        await dispatch("command", "/help");
        await projectNativeRead(client, "a", state);
        writeFileSync(join(directory, "result.json"), JSON.stringify({ items: state.items().map(item => ({ id: item.message.id, state: item.state,
          runId: item.runId, readPending: item.readPending, handoffUnconfirmed: item.handoffUnconfirmed, manifest: item.dispatchManifest })) }));
      } catch (error) { writeFileSync(join(directory, "result.json"), JSON.stringify({ error: String(error) })); }
    }, 1000);
  }, stop: async () => {} });
} };
