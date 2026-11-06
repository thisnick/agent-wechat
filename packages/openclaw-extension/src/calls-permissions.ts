import type { ResolvedWeChatConferenceGroupConfig, ResolvedWeChatVoiceCallsConfig } from "./calls-config.ts";

export type CallToolPolicy = "none" | "safe-read-only" | "owner";
export type CallPolicyReason = "group_default" | "owner_inviter" | "owner_grant_missing" | "permission_reduced";
export type CallConferenceContext = { groupId: string; inviterId?: string; policyAtAdmission?: CallToolPolicy };
const rank = { none: 0, "safe-read-only": 1, owner: 2 };
export function restrictCallPolicy(a: CallToolPolicy, b: CallToolPolicy): CallToolPolicy { return rank[a] <= rank[b] ? a : b; }
export function isExactCallOwner(id: string | undefined, entries: unknown): boolean {
  return !!id && !/[\s:*\u0000-\u001f\u007f]/.test(id) && !id.endsWith("@chatroom")
    && Array.isArray(entries) && entries.includes(`agent-wechat:${id}`);
}

/** The invitation selects policy; tools still run as the actual group principal.
 * A human owner match alone never supplies the host's group owner grant. */
export function conferenceCallPolicy(voice: ResolvedWeChatVoiceCallsConfig, group: ResolvedWeChatConferenceGroupConfig,
  context: CallConferenceContext, ownerAllowFrom: unknown): { toolPolicy: CallToolPolicy; reason: CallPolicyReason } {
  const override = group.ownerInviterToolPolicy !== undefined && isExactCallOwner(context.inviterId, ownerAllowFrom);
  let toolPolicy = restrictCallPolicy(voice.toolPolicy, override ? group.ownerInviterToolPolicy! : group.toolPolicy);
  let reason: CallPolicyReason = override ? "owner_inviter" : "group_default";
  if (!voice.allowConsultSessionPersistence) toolPolicy = "none";
  if (toolPolicy === "owner" && (!Array.isArray(ownerAllowFrom) || !ownerAllowFrom.includes(`agent-wechat:${context.groupId}`))) {
    toolPolicy = "safe-read-only"; reason = "owner_grant_missing";
  }
  if (context.policyAtAdmission) toolPolicy = restrictCallPolicy(toolPolicy, context.policyAtAdmission);
  return { toolPolicy, reason };
}
