import type { PluginRuntime } from "openclaw/plugin-sdk/core";

let runtime: PluginRuntime | null = null;

export function setWeChatRuntime(next: PluginRuntime) {
  runtime = next;
}

export function getWeChatRuntime(): PluginRuntime {
  if (!runtime) {
    throw new Error("WeChat runtime not initialized");
  }
  return runtime;
}
