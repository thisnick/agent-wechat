import type { OpenClawConfig, PluginRuntime } from "openclaw/plugin-sdk/core";
import { WECHAT_INGRESS_IDENTITY } from "./access-control.ts";

/** Host-owned ingress carries the authenticated call principal into tool
 * creation. Plain JSON sender fields alone do not establish owner authority. */
export async function buildCallConsultContext(runtime: PluginRuntime, cfg: OpenClawConfig, params: {
  senderId: string; accountId: string; sessionKey: string; parentKey?: string; runId: string;
  question: string; body: string; conference?: { groupId: string; owner: boolean };
}): Promise<ReturnType<PluginRuntime["channel"]["inbound"]["buildContext"]>> {
  const inbound = runtime.channel?.inbound;
  if (typeof inbound?.ingress?.resolveStable !== "function" || typeof inbound.buildContext !== "function") throw Error("Calling host ingress unavailable");
  const agentId = /^agent:([^:]+):/.exec(params.sessionKey)?.[1];
  if (!agentId) throw Error("Invalid calling consultation session");
  const { senderId, accountId, sessionKey, parentKey, runId, conference } = params;
  const kind = conference ? "group" : "direct";
  const conversationId = conference?.groupId ?? senderId;
  const ingress = await inbound.ingress.resolveStable({
    channelId: "agent-wechat", accountId, cfg, identity: WECHAT_INGRESS_IDENTITY,
    subject: { stableId: senderId }, conversation: { kind, id: conversationId },
    contextBinding: { agentId, sessionKey, messageId: runId, nativeChannelId: conversationId, inboundEventKind: "user_request" },
    event: { kind: "message", authMode: "inbound", mayPair: false },
    dmPolicy: "allowlist", allowFrom: [senderId], groupPolicy: "allowlist", groupAllowFrom: [senderId],
    policy: { groupAllowFromFallbackToAllowFrom: false, activation: { requireMention: false, allowTextCommands: false } },
    command: false, useDefaultPairingStore: false,
  });
  if (ingress.ingress.admission !== "dispatch") throw Error("Calling consultation ingress denied");
  return inbound.buildContext({
    channel: "agent-wechat", accountId, provider: "agent-wechat", surface: "agent-wechat", messageId: runId,
    from: conference ? `wechat:group:${conversationId}` : `wechat:${senderId}`,
    sender: { id: senderId }, conversation: { kind, id: conversationId, nativeChannelId: conversationId },
    route: { agentId, accountId, routeSessionKey: sessionKey, dispatchSessionKey: sessionKey, ...(parentKey ? { parentSessionKey: parentKey } : {}) },
    reply: { to: `wechat:${conversationId}`, nativeChannelId: conversationId, sourceReplyDeliveryMode: "none" },
    message: { inboundEventKind: "user_request", rawBody: params.question, body: params.question, bodyForAgent: params.body },
    access: { commands: { authorized: false } }, channelIngress: ingress,
    extra: { CommandAuthorized: false, CommandInterpretationSuppressed: true, SuppressMessageReceivedHooks: true },
  });
}
