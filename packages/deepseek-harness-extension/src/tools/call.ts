/**
 * wechat_call (DESIGN.md §6.4): place and manage WeChat voice calls. The tool
 * itself carries no dial authority — it is granted only through
 * tools.grants.call, and every mutating action additionally demands an owner
 * caller whose exact wxid is listed in calls.voice.outboundToolAllowFrom.
 * Inbound auto-answer never goes through this tool.
 */
import { ToolAccessError } from '../tool-context.ts';
import type { ToolPrincipal } from '../contracts.ts';
import { defineWechatTool, requirePrincipal, revalidateForMutation, renderJson, type ToolDeps } from './shared.ts';

/** Tool requesters need owner authority plus an explicit outbound allowlist hit. */
function requireToolCaller(deps: ToolDeps, principal: ToolPrincipal): void {
  {
  if (!principal.isOwner) throw new ToolAccessError('starting calls requires the owner caller', 'wechat_call');
  const peer = String(principal.chatJid);
  if (!deps.outboundToolAllowFrom?.includes(peer)) {
    throw new ToolAccessError(`calls from ${peer} are not listed in calls.voice.outboundToolAllowFrom`, 'wechat_call');
  }
  }
}

export function wechatCallTool(deps: ToolDeps) {
  return defineWechatTool({
    deps,
    name: 'wechat_call',
    description: 'Place and manage WeChat voice calls for the configured account. '
      + 'Actions: status (list the calls feature state and live calls), dial (start a direct call to a contact id), '
      + 'hangup (end a call by id, or the current one), conference_start (start a conference inside a group chat with invitee contact ids), '
      + 'invite (add contacts to a live call). Mutating actions require the owner caller '
      + '(calls.voice.outboundToolAllowFrom); inbound calls never come through this tool.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['status', 'dial', 'hangup', 'conference_start', 'invite'],
          description: 'Which call action to perform.',
        },
        contactId: { type: 'string', description: 'Contact id to call (dial). Must be listed in calls.voice.outboundAllowTo.' },
        callId: { type: 'string', description: 'Call id (hangup, invite); omit for hangup to end the current call.' },
        groupId: { type: 'string', description: 'Group chat id ending with @chatroom (conference_start).' },
        contactIds: {
          type: 'array',
          items: { type: 'string' },
          description: 'Contact ids to invite (conference_start invitees, invite members).',
        },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const input = args as { action: string; contactId?: string; callId?: string; groupId?: string; contactIds?: string[] };
      const calls = deps.calls;
      if (!calls) throw new ToolAccessError('calls are not configured for this deployment', 'wechat_call');
      if (input.action === 'status') {
        await requirePrincipal(deps, exec, 'wechat_call');
        return calls.status();
      }
      const principal = await requirePrincipal(deps, exec, 'wechat_call');
      await revalidateForMutation(deps, principal, 'wechat_call');
      requireToolCaller(deps, principal);
      if (input.action === 'dial') {
        if (!input.contactId) throw new Error('dial requires contactId');
        return calls.dial(input.contactId);
      }
      if (input.action === 'hangup') {
        const stopped = await calls.hangupCall(input.callId);
        return { status: stopped ? 'requested' : 'no_call', note: 'Hangup is a request; the daemon call state event is the receipt.' };
      }
      if (input.action === 'conference_start') {
        if (!input.groupId || !input.groupId.endsWith('@chatroom')) throw new Error('conference_start requires a group chat id ending with @chatroom');
        const invitees = input.contactIds ?? [];
        if (invitees.length === 0) throw new Error('conference_start requires at least one invitee contactId');
        return calls.conferenceStart(input.groupId, invitees);
      }
      if (input.action === 'invite') {
        if (!input.callId) throw new Error('invite requires callId');
        const contactIds = input.contactIds ?? [];
        if (contactIds.length === 0) throw new Error('invite requires at least one contactId');
        return calls.invite(input.callId, contactIds);
      }
      throw new Error(`unknown action: ${input.action}`);
    },
  });
}

export function callTools(deps: ToolDeps) {
  return [wechatCallTool(deps)];
}
