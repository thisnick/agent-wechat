import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveWeChatAccount } from "./types.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import type { NativeCallService } from "./calls-service.ts";

export function registerNativeCallTool(api: OpenClawPluginApi, service: () => NativeCallService | undefined): void {
  api.registerTool({ contextVersion: 2, create: context => {
    const cfg = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
    const account = cfg && resolveWeChatAccount(cfg as unknown as Record<string, unknown>);
    if (!account?.enabled || !account.calls || !account.native) return [];
    const configurationKey = nativeConfigurationKey(account.native);
    const callConfiguration = JSON.stringify(account.calls);
    const check = () => assertCallingToolAccess(context, configurationKey, callConfiguration);
    return [{ name: "wechat_call", label: "WeChat Call", catalogMode: "direct-only" as const,
      description: "Check this plugin's voice-call status, initiate one authorized direct voice call, or end its owned call. Calls require separate calling allowlists and provider readiness. Never retry an uncertain dial or call without the user's request. This tool does not open a computer microphone, record calls, or grant caller owner privileges.",
      parameters: { type: "object", additionalProperties: false, required: ["action"], properties: {
        action: { type: "string", enum: ["status", "dial", "hangup"] }, contactId: { type: "string", minLength: 1, maxLength: 200 },
      } },
      execute: async (_id: string, args: unknown, signal?: AbortSignal) => {
        if (!args || typeof args !== "object" || Array.isArray(args)) throw Error("Invalid calling arguments");
        const value = args as Record<string, unknown>;
        if (Object.keys(value).some(key => !["action", "contactId"].includes(key)) || !["status", "dial", "hangup"].includes(value.action as string)
          || (value.action === "dial" ? typeof value.contactId !== "string" || !value.contactId || value.contactId.length > 200 : value.contactId !== undefined)) throw Error("Invalid calling arguments");
        signal?.throwIfAborted(); check();
        const current = service(); if (!current) throw Error("Calling service unavailable");
        const result = value.action === "status" ? current.status() : value.action === "dial"
          ? await current.dial(value.contactId as string, { signal, assertCurrent: check }) : await current.end();
        return { content: [{ type: "text" as const, text: JSON.stringify(result ?? { requested: true }) }], details: result ?? { requested: true } };
      } }];
  } }, { names: ["wechat_call"], optional: true });
}

export function assertCallingToolAccess(context: OpenClawPluginToolContext, configurationKey: string, callConfiguration: string): void {
  if (!context.assertInvocationCurrent) throw Error("Calling tool invocation authority unavailable");
  context.assertInvocationCurrent();
  const cfg = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  const current = cfg && resolveWeChatAccount(cfg as unknown as Record<string, unknown>);
  if (!current?.enabled || !current.native || !current.calls || nativeConfigurationKey(current.native) !== configurationKey || JSON.stringify(current.calls) !== callConfiguration
    || context.agentId !== current.calls.agentId || context.senderIsOwner !== true || !context.requesterSenderId || !current.calls.outboundToolAllowFrom.includes(context.requesterSenderId)) throw Error("Calling tool is not permitted for this requester");
}
