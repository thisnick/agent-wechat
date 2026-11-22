/**
 * Consultation session keys (DESIGN.md §6.3). Ported from the OpenClaw
 * adapter's `calls-session.ts`: per-contact keeps one session per contact so
 * the agent remembers earlier calls; read-only callers always get a separate
 * key from the owner's, so a later loss of owner status never exposes an
 * owner session. per-call or an unfit contact ID starts fresh every call.
 */
import { createHash, randomUUID } from 'node:crypto';
import type { CallSessionScope } from './config.ts';

const CONTACT_ID = /^[A-Za-z0-9_.@-]{1,200}$/;

export type ConferencePrincipal = { accountId: string; nativeAccountId?: string; groupId: string };

export function callSessionKey(
  agentId: string,
  scope: CallSessionScope,
  contactId: string,
  owner: boolean,
  conference?: ConferencePrincipal,
): string {
  if (conference) {
    const principal = createHash('sha256').update(JSON.stringify([conference.accountId, conference.nativeAccountId ?? '', conference.groupId])).digest('hex');
    return `agent:${agentId}:wechat-conference:${principal}:${owner ? 'owner' : 'read-only'}${scope === 'per-call' ? `:${randomUUID()}` : ''}`;
  }
  if (scope === 'per-contact' && CONTACT_ID.test(contactId) && !contactId.endsWith('@chatroom')) {
    return `agent:${agentId}:wechat-call:${contactId.toLowerCase()}${owner ? '' : ':read-only'}`;
  }
  return `agent:${agentId}:wechat-call:${randomUUID()}`;
}
