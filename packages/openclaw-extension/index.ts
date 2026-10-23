import type { OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import type { OpenClawPluginConfigSchema } from "openclaw/plugin-sdk/core";
import { nativePluginConfigSchema } from "./src/native-config.js";
import { resolveWeChatAccount } from "./src/types.js";
import { ensureNativeHost, stopNativeHost } from "./src/native-host.js";
import { wechatPlugin } from "./src/channel.js";
import { setWeChatRuntime } from "./src/runtime.js";
import { registerNativeFacade } from "./src/native-facade.js";
import { registerNativePreparedBoundary } from "./src/native-prepared-boundary.js";
import { registerNativeHistoryTools } from "./src/native-history-tools.js";
import { registerNativeShareTools } from "./src/native-share-tool.js";
import { registerNativeRecallTools } from "./src/native-recall-tools.js";
import { registerNativeGroupTools } from "./src/native-group-tool.js";

const plugin: {
  id: string;
  name: string;
  description: string;
  configSchema: OpenClawPluginConfigSchema;
  register: (api: OpenClawPluginApi) => void;
} = {
  id: "agent-wechat",
  name: "WeChat",
  description: "WeChat channel for the standalone native daemon",
  configSchema: { jsonSchema: nativePluginConfigSchema, uiHints: { "native.token": { sensitive: true } } },
  register(api: OpenClawPluginApi) {
    setWeChatRuntime(api.runtime);
    const selected = resolveWeChatAccount(api.config as unknown as Record<string, unknown>);
    api.registerChannel({ plugin: wechatPlugin });
    registerNativeHistoryTools(api);
    registerNativeShareTools(api);
    registerNativeRecallTools(api);
    registerNativeGroupTools(api);
    const stopFacade = selected?.native ? registerNativeFacade(api, selected.native) : () => {};
    const stopPreparedBoundary = selected?.native?.readPolicy && selected.native.readPolicy !== "on_inbound"
      ? registerNativePreparedBoundary(api) : () => {};
    api.registerService({
      id: "agent-wechat-native",
      reload: { configPrefixes: ["plugins.entries.agent-wechat.config", "channels.agent-wechat", "secrets"] },
      start: async context => {
        const account = resolveWeChatAccount(context.config as unknown as Record<string, unknown>);
        if (account?.native && account.enabled) await ensureNativeHost(account.native);
      },
      stop: async () => { stopFacade(); stopPreparedBoundary(); await stopNativeHost(); },
    });
  },
};

export default plugin;
