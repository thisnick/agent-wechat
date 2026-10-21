// Exercises the actual Gateway tool policies with synthetic account data.
import base from "../../index.ts";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import { dispatchNativeMessage } from "../../src/native-monitor.ts";
import { NativeConsumerState } from "../../src/native-consumer-state.ts";
import { ensureNativeHost } from "../../src/native-host.ts";
import { resolveWeChatAccount } from "../../src/types.ts";

export default { ...base, register(api: any) {
  base.register(api);
  const directory = process.env.NATIVE_ARCHIVE_FIXTURE_DIR;
  if (!directory) throw Error("Isolated archive fixture directory required");
  api.registerService({ id: "native-archive-policy-validation", start: async () => {
    setTimeout(async () => {
      try {
        const cfg = api.runtime.config.current(), account = resolveWeChatAccount(cfg)!;
        const { client } = await ensureNativeHost(account.native!);
        const state = new NativeConsumerState(join(directory, "probe-consumer"), "fixture", "a");
        const outcomes: Record<string, string> = {};
        let sequence = 0;
        for (const role of ["ungranted", "ordinary", "privileged"]) {
          const message = { id: `trigger-${role}`, accountId: "a", threadId: `${role}-peer`, senderId: `${role}-peer`, serverId: String(++sequence),
            type: 1, body: `archive-case:${role}`, direction: "incoming" as const, createdAt: new Date().toISOString(), version: "v1", media: [] };
          state.accept(String(sequence), message, "live");
          outcomes[role] = await dispatchNativeMessage({ client, state, account, message, cfg, core: api.runtime });
        }
        writeFileSync(join(directory, "result.json"), JSON.stringify({ outcomes }));
      } catch (error) { writeFileSync(join(directory, "result.json"), JSON.stringify({ error: String(error) })); }
    }, 1000);
  }, stop: async () => {} });
} };
