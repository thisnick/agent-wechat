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
    const capturedService = service();
    const consultation = capturedService?.bindOwnerConsultationTool(context);
    const check = () => assertCallingToolAccess(context, configurationKey, callConfiguration, consultation);
    return [{ name: "wechat_call", label: "WeChat Call", catalogMode: "direct-only" as const,
      description: "Check voice-call status, dial an authorized contact, start or join a configured voice conference, invite allowed contacts to its owned conference, or hang up. Use action hangup when asked to leave the current call; in a conference it removes only this agent, leaving the room open for others. Owner-authorized call consultations can control their call without the separate chat-requester allowlist. Destination and group allowlists, host tool policy and approvals still apply. Join requires a current invitation and never creates a room. Invitations do not promote a direct call. Never retry an uncertain operation without the user's request. Do not claim an action was attempted before executing it.",
      parameters: { type: "object", additionalProperties: false, required: ["action"], properties: {
        action: { type: "string", enum: ["status", "dial", "conference_start", "conference_join", "invite", "hangup"], description: "Call operation to run; the tool description explains each one." }, contactId: { type: "string", minLength: 1, maxLength: 200, description: "Contact to dial. Required for dial." },
        groupId: { type: "string", minLength: 1, maxLength: 200, description: "Group of the configured voice conference. Required for conference_start and conference_join." },
        contactIds: { type: "array", minItems: 1, maxItems: 100, uniqueItems: true, items: { type: "string", minLength: 1, maxLength: 200 }, description: "Contacts to invite. Required for conference_start and invite." },
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
        if (consultation && current !== capturedService) throw Error("Calling consultation service changed");
        const access = { signal, assertCurrent: check, parentSession: context.sessionKey ? { key: context.sessionKey, expectedSessionId: context.sessionId } : undefined };
        const result = value.action === "status" ? current.status() : value.action === "dial" ? await current.dial(value.contactId as string, access)
          : value.action === "conference_start" ? await current.startConference(value.groupId as string, value.contactIds as string[], access)
          : value.action === "conference_join" ? await current.joinConference(value.groupId as string, access)
          : value.action === "invite" ? await current.invite(value.contactIds as string[], access) : await current.end();
        return { content: [{ type: "text" as const, text: JSON.stringify(result ?? { requested: true }) }], details: result ?? { requested: true } };
      } }];
  } }, { names: ["wechat_call"], optional: true });
}

export function assertCallingToolAccess(context: OpenClawPluginToolContext, configurationKey: string, callConfiguration: string, consultation?: () => void): void {
  if (!context.assertInvocationCurrent) throw Error("Calling tool invocation authority unavailable");
  context.assertInvocationCurrent();
  const cfg = context.getRuntimeConfig?.() ?? context.runtimeConfig ?? context.config;
  const current = cfg && resolveWeChatAccount(cfg as unknown as Record<string, unknown>);
  if (!current?.enabled || !current.native || !current.calls?.voice || nativeConfigurationKey(current.native) !== configurationKey || callsConfigurationKey(current.calls) !== callConfiguration
    || context.senderIsOwner !== true || !context.requesterSenderId) throw Error("Calling tool is not permitted for this requester");
  if (consultation) consultation();
  else if (context.sessionKey && /:wechat-(?:call|conference):/.test(context.sessionKey)) throw Error("Calling consultation authority unavailable");
  else if (!current.calls.voice.outboundToolAllowFrom.includes(context.requesterSenderId)) throw Error("Calling tool is not permitted for this requester");
}
