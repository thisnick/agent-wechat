/**
 * `wechat_push` — one-way notification delivery to pre-configured targets.
 *
 * Unlike every other WeChat tool, this tool does NOT resolve authority from a
 * chat route binding: the user explicitly chose "any DSH session" (including
 * non-WeChat agents such as CLI sessions). The authorization boundary is the
 * configuration itself:
 *
 * - the tool only registers when `push.enabled` is true AND at least one
 *   target is configured (fail-closed default);
 * - every send validates the target against the configured whitelist with an
 *   exact match — model-supplied ids never widen it;
 * - delivery goes through the durable outbox (at-most-once POST, canonical
 *   readback, `needs_review` on ambiguity — never an automatic resend).
 */

import { ToolAccessError, type WechatToolDefinition } from './shared.ts';
import { defineWechatTool, type ToolDeps } from './shared.ts';

/** Outcome note keyed by the outbox status, phrased for the model. */
function statusNote(status: string): string {
  if (status === 'sent') {
    return 'Delivered; confirmed by canonical readback.';
  }
  if (status === 'needs_review') {
    return 'Send outcome was uncertain; the message is held for review and never auto-retried.';
  }
  return 'Accepted into the outbound journal; the daemon readback is the receipt.';
}

/** The push tool; present in `wechatTools` only when a push sender is wired. */
export function wechatPushTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_push',
    description:
      'Send a one-way notification message to a pre-configured WeChat contact or group from any session. '
        + 'Targets are limited to the configured push whitelist; use action "list" to see it. '
        + 'Fire-and-forget: this is not a chat reply and does not create a conversation.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['send', 'list'],
          default: 'send',
          description: 'send delivers text to one configured target; list returns the configured targets.',
        },
        target: {
          type: 'string',
          description: 'Configured target id: a wxid for a direct message or a group id ending with @chatroom. Required for send.',
        },
        text: {
          type: 'string',
          minLength: 1,
          description: 'Message text delivered verbatim (whitespace-trimmed). Required for send.',
        },
      },
      additionalProperties: false,
    },
    schema: {
      type: 'object',
      properties: {
        status: { type: 'string' },
        target: { type: 'string' },
        key: { type: 'string' },
        note: { type: 'string' },
      },
      additionalProperties: true,
    },
    async execute(args, exec) {
      const push = deps.push;
      if (push === undefined) {
        throw new ToolAccessError('wechat_push: no push targets are configured', 'wechat_push');
      }
      if (exec.agent?.id === undefined) {
        throw new ToolAccessError('wechat_push: no calling Agent', 'wechat_push');
      }
      if (args.action === 'list') {
        return { targets: [...push.targets] };
      }
      const target = typeof args.target === 'string' ? args.target.trim() : '';
      if (target.length === 0) {
        throw new ToolAccessError('wechat_push: target is required for send', 'wechat_push');
      }
      if (!push.targets.includes(target)) {
        throw new ToolAccessError(
          `wechat_push: target "${target}" is not in the configured push targets (use action "list")`,
          'wechat_push',
        );
      }
      const text = typeof args.text === 'string' ? args.text.trim() : '';
      if (text.length === 0) {
        throw new ToolAccessError('wechat_push: text is required and must not be empty', 'wechat_push');
      }
      const delivered = await push.deliver(target, text);
      return {
        status: delivered.status,
        target,
        key: delivered.key,
        note: statusNote(delivered.status),
      };
    },
  });
}

export function pushTools(deps: ToolDeps): WechatToolDefinition[] {
  return [wechatPushTool(deps)];
}
