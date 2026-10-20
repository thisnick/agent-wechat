import type { OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import type { OpenClawPluginConfigSchema } from "openclaw/plugin-sdk/core";
import { nativePluginConfigSchema } from "./src/native-config.js";
import { resolveWeChatAccount } from "./src/types.js";
import { ensureNativeHost, stopNativeHost } from "./src/native-host.js";
import { wechatPlugin } from "./src/channel.js";
import { setWeChatRuntime } from "./src/runtime.js";
import { registerNativeFacade } from "./src/native-facade.js";
import { registerNativePreparedBoundary } from "./src/native-prepared-boundary.js";

const plugin: {
  id: string;
  name: string;
  description: string;
  configSchema: OpenClawPluginConfigSchema;
  register: (api: OpenClawPluginApi) => void;
} = {
  id: "agent-wechat",
  name: "WeChat",
  description: "WeChat channel with explicit native and legacy container backends",
  configSchema: { jsonSchema: nativePluginConfigSchema },
  register(api: OpenClawPluginApi) {
    setWeChatRuntime(api.runtime);
    const selected = resolveWeChatAccount(api.config as unknown as Record<string, unknown>);
    // Capabilities are channel-global: this increment selects one backend per
    // registered channel, rather than mixing native and legacy account support.
    const channel = selected?.backend === "native" ? {
      ...wechatPlugin,
      capabilities: { ...wechatPlugin.capabilities, media: true },
      message: { ...(wechatPlugin.message as any), capabilities: { ...(wechatPlugin.message as any)?.capabilities, media: true } },
    } : wechatPlugin;
    api.registerChannel({ plugin: channel });
    const stopFacade = selected?.backend === "native" && selected.native ? registerNativeFacade(api, selected.native) : () => {};
    const stopPreparedBoundary = selected?.backend === "native" ? registerNativePreparedBoundary(api) : () => {};
    api.registerService({
      id: "agent-wechat-native",
      reload: { configPrefixes: ["plugins.entries.agent-wechat.config", "channels.agent-wechat"] },
      start: async context => {
        const account = resolveWeChatAccount(context.config as unknown as Record<string, unknown>);
        if (account?.backend === "native" && account.native && account.enabled) await ensureNativeHost(account.native);
      },
      stop: async () => { stopFacade(); stopPreparedBoundary(); await stopNativeHost(); },
    });
  },
};

export default plugin;
