/**
 * Call authority and tool policy (DESIGN.md §6.3). Direct port of the OpenClaw
 * adapter's `calls-permissions.ts`: permissions only ever decrease during a
 * call, the invitation selects conference policy, and an owner match alone
 * never supplies the host's group owner grant.
 */
import type { CallToolPolicy, ResolvedWeChatConferenceGroupConfig, ResolvedWeChatVoiceCallsConfig } from './config.ts';

export type CallPolicyReason = 'group_default' | 'owner_inviter' | 'owner_grant_missing' | 'permission_reduced';
export type CallConferenceContext = { groupId: string; inviterId?: string; policyAtAdmission?: CallToolPolicy };

const rank: Record<CallToolPolicy, number> = { none: 0, 'safe-read-only': 1, owner: 2 };
const CONTACT_ID_FORBIDDEN = new RegExp('[\\s:*\\u0000-\\u001f\\u007f]');

export function restrictCallPolicy(a: CallToolPolicy, b: CallToolPolicy): CallToolPolicy {
  return rank[a] <= rank[b] ? a : b;
}

/** Exact `agent-wechat:<id>` entry in the harness owner allowlist; contacts only. */
export function isExactCallOwner(id: string | undefined, entries: unknown): boolean {
  return !!id && !CONTACT_ID_FORBIDDEN.test(id) && !id.endsWith('@chatroom')
    && Array.isArray(entries) && entries.includes(`agent-wechat:${id}`);
}

export function directCallPolicy(
  voice: ResolvedWeChatVoiceCallsConfig,
  peer: string,
  ownerAllowFrom: unknown,
): CallToolPolicy {
  if (!voice.allowConsultSessionPersistence || voice.toolPolicy === 'none') return 'none';
  return voice.toolPolicy === 'owner' && isExactCallOwner(peer, ownerAllowFrom) ? 'owner' : 'safe-read-only';
}

/**
 * The invitation selects policy; tools still run as the actual group principal.
 * A human owner match alone never supplies the host's group owner grant.
 */
export function conferenceCallPolicy(
  voice: ResolvedWeChatVoiceCallsConfig,
  group: ResolvedWeChatConferenceGroupConfig,
  context: CallConferenceContext,
  ownerAllowFrom: unknown,
): { toolPolicy: CallToolPolicy; reason: CallPolicyReason } {
  const override = group.ownerInviterToolPolicy !== undefined && isExactCallOwner(context.inviterId, ownerAllowFrom);
  let toolPolicy = restrictCallPolicy(voice.toolPolicy, override ? group.ownerInviterToolPolicy! : group.toolPolicy);
  let reason: CallPolicyReason = override ? 'owner_inviter' : 'group_default';
  if (!voice.allowConsultSessionPersistence) toolPolicy = 'none';
  if (toolPolicy === 'owner' && (!Array.isArray(ownerAllowFrom) || !ownerAllowFrom.includes(`agent-wechat:${context.groupId}`))) {
    toolPolicy = 'safe-read-only';
    reason = 'owner_grant_missing';
  }
  if (context.policyAtAdmission) {
    const reduced = restrictCallPolicy(toolPolicy, context.policyAtAdmission);
    if (rank[reduced] < rank[context.policyAtAdmission]) reason = 'permission_reduced';
    toolPolicy = reduced;
  }
  return { toolPolicy, reason };
}
