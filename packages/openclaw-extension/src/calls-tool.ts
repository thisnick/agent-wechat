import type { OpenClawPluginApi, OpenClawPluginToolContext } from "openclaw/plugin-sdk/core";
import { resolveWeChatAccount } from "./types.ts";
import { callsConfigurationKey } from "./calls-secrets.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import type { NativeCallService } from "./calls-service.ts";

export function registerNativeCallTool(api: OpenClawPluginApi, service: () => NativeCallService | undefined): void {
  api.registerTool({ contextVersion: 2, create: context => {
    const cfg = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
    const account = cfg && resolveWeChatAccount(cfg as unknown as Record<string, unknown>);
    if (!account?.enabled || !account.calls?.voice || !account.native) return [];
    const configurationKey = nativeConfigurationKey(account.native);
    const callConfiguration = callsConfigurationKey(account.calls);
    const check = () => assertCallingToolAccess(context, configurationKey, callConfiguration);
    return [{ name: "wechat_call", label: "WeChat Call", catalogMode: "direct-only" as const,
      description: "Check voice-call status, dial an authorized contact, start or join a configured voice conference, invite allowed contacts to its owned conference, or hang up. Join requires a current invitation and never creates a room. Conference invitations do not promote a direct call. Never retry an uncertain operation without the user's request. Calls require their own allowlists and provider readiness; this tool grants no caller owner privileges.",
      parameters: { type: "object", additionalProperties: false, required: ["action"], properties: {
        action: { type: "string", enum: ["status", "dial", "conference_start", "conference_join", "invite", "hangup"] }, contactId: { type: "string", minLength: 1, maxLength: 200 },
        groupId: { type: "string", minLength: 1, maxLength: 200 },
        contactIds: { type: "array", minItems: 1, maxItems: 100, uniqueItems: true, items: { type: "string", minLength: 1, maxLength: 200 } },
      } },
      execute: async (_id: string, args: unknown, signal?: AbortSignal) => {
        if (!args || typeof args !== "object" || Array.isArray(args)) throw Error("Invalid calling arguments");
        const value = args as Record<string, unknown>;
        const id = (item: unknown): item is string => typeof item === "string" && item.length > 0 && item.length <= 200 && !/[\u0000-\u001f\u007f]/.test(item);
        const contacts = Array.isArray(value.contactIds) && value.contactIds.length > 0 && value.contactIds.length <= 100 && value.contactIds.every(id) && new Set(value.contactIds).size === value.contactIds.length;
        if (Object.keys(value).some(key => !["action", "contactId", "groupId", "contactIds"].includes(key)) || !["status", "dial", "conference_start", "conference_join", "invite", "hangup"].includes(value.action as string)
          || (value.action === "dial" ? !id(value.contactId) : value.contactId !== undefined)
          || (["conference_start", "conference_join"].includes(String(value.action)) ? !id(value.groupId) : value.groupId !== undefined)
          || (["conference_start", "invite"].includes(String(value.action)) ? !contacts : value.contactIds !== undefined)) throw Error("Invalid calling arguments");
        signal?.throwIfAborted(); check();
        const current = service(); if (!current) throw Error("Calling service unavailable");
        const access = { signal, assertCurrent: check, parentSession: context.sessionKey ? { key: context.sessionKey, expectedSessionId: context.sessionId } : undefined };
        const result = value.action === "status" ? current.status() : value.action === "dial" ? await current.dial(value.contactId as string, access)
          : value.action === "conference_start" ? await current.startConference(value.groupId as string, value.contactIds as string[], access)
          : value.action === "conference_join" ? await current.joinConference(value.groupId as string, access)
          : value.action === "invite" ? await current.invite(value.contactIds as string[], access) : await current.end();
        return { content: [{ type: "text" as const, text: JSON.stringify(result ?? { requested: true }) }], details: result ?? { requested: true } };
      } }];
  } }, { names: ["wechat_call"], optional: true });
}

export function assertCallingToolAccess(context: OpenClawPluginToolContext, configurationKey: string, callConfiguration: string): void {
  if (!context.assertInvocationCurrent) throw Error("Calling tool invocation authority unavailable");
  context.assertInvocationCurrent();
  const cfg = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  const current = cfg && resolveWeChatAccount(cfg as unknown as Record<string, unknown>);
  if (!current?.enabled || !current.native || !current.calls?.voice || nativeConfigurationKey(current.native) !== configurationKey || callsConfigurationKey(current.calls) !== callConfiguration
    || context.senderIsOwner !== true || !context.requesterSenderId || !current.calls.voice.outboundToolAllowFrom.includes(context.requesterSenderId)) throw Error("Calling tool is not permitted for this requester");
}
