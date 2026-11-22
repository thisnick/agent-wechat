/**
 * M4 owner + archive tools — DESIGN.md §4 rows 5–7.
 *
 * Owner tools (contacts, contact requests, group info) operate account-wide
 * but stay inside the account the owner is bound to. Archive tools cross chats
 * within that account and are granted explicitly (owner + archive grant).
 * Identity always comes from the trusted route binding; model arguments never
 * widen scope.
 */

import { ToolAccessError } from '../tool-context.ts';
import type { ToolPrincipal } from '../contracts.ts';
import {
  compactMessage,
  defineWechatTool,
  pageItems,
  requirePrincipal,
  revalidateForMutation,
  type ToolDeps,
  type WechatToolDefinition,
} from './shared.ts';

const requireId = (value: unknown, field: string): string => {
  if (typeof value !== 'string' || value.length === 0 || value.length > 4096) {
    throw new Error(`${field} is required`);
  }
  return value;
};

const clampLimit = (value: unknown, fallback: number, max: number): number => {
  if (value === undefined) return fallback;
  const n = Number(value);
  if (!Number.isInteger(n) || n < 1) throw new Error('limit must be a positive integer');
  return Math.min(n, max);
};

async function requireOwner(
  deps: ToolDeps,
  exec: { readonly agent?: { readonly id: string } | undefined; readonly signal: AbortSignal },
  toolName: string,
): Promise<ToolPrincipal> {
  const principal = await requirePrincipal(deps, exec, toolName);
  if (!principal.isOwner) {
    throw new ToolAccessError(`${toolName}: owner authority required`, toolName);
  }
  return principal;
}

const cursor = (args: Record<string, unknown>): Record<string, unknown> =>
  typeof args['cursor'] === 'string' && args['cursor'].length > 0 ? { cursor: args['cursor'] } : {};

// ---------------------------------------------------------------------------
// wechat_contacts
// ---------------------------------------------------------------------------

export function wechatContactsTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_contacts',
    description:
      'Owner address book: list, get, filter, exact-lookup or search contacts, and remove a contact (keeps chat history). Owner only; removal requires an idempotency key.',
    parameters: {
      type: 'object',
      properties: {
        action: { type: 'string', enum: ['list', 'get', 'find', 'lookup', 'search', 'remove'], description: 'Which contact operation to run.' },
        username: { type: 'string', description: 'Contact wxid.' },
        query: { type: 'string', description: 'Filter or search text (find/search).' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous list call.' },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'How many entries to return.' },
        idempotencyKey: { type: 'string', description: 'Required for remove.' },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_contacts');
      const account = String(principal.accountId);
      const action = String(args['action']);
      const limit = clampLimit(args['limit'], 20, 200);

      switch (action) {
        case 'list': {
          const page = await deps.client.contacts(
            account,
            { limit, ...cursor(args) },
            exec.signal,
          );
          return pageItems(page);
        }
        case 'get': {
          const username = requireId(args['username'], 'username');
          const contact = await deps.client.contact(account, username, exec.signal);
          return { contact };
        }
        case 'find': {
          const query = requireId(args['query'], 'query').toLowerCase();
          const page = await deps.client.contacts(account, { limit: 200 }, exec.signal);
          const { items } = pageItems(page);
          const matched = items.filter((item) =>
            JSON.stringify(item).toLowerCase().includes(query),
          );
          return { items: matched.slice(0, limit) };
        }
        case 'lookup': {
          const username = requireId(args['username'], 'username');
          const page = await deps.client.lookupContacts(account, [username], exec.signal);
          return pageItems(page);
        }
        case 'search': {
          const query = requireId(args['query'], 'query');
          const page = await deps.client.searchContacts(account, query, exec.signal);
          return pageItems(page);
        }
        case 'remove': {
          const username = requireId(args['username'], 'username');
          const key = requireId(args['idempotencyKey'], 'idempotencyKey');
          await revalidateForMutation(deps, principal, 'wechat_contacts');
          const operation = await deps.client.removeContact(account, username, key, exec.signal);
          return {
            status: 'submitted',
            operation: operation ?? null,
            note: 'Removal submitted; the contact is gone from the address book but chat history is kept.',
          };
        }
        default:
          throw new Error(`unknown action: ${action}`);
      }
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_contact_request
// ---------------------------------------------------------------------------

export function wechatContactRequestTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_contact_request',
    description:
      'Owner friend-request inbox: list/get requests, send a request, accept one, delete local history of one, or poll a submitted operation. Owner only; every mutation requires an idempotency key and accept/delete need the request version.',
    parameters: {
      type: 'object',
      properties: {
        action: {
          type: 'string',
          enum: ['list', 'get', 'send', 'accept', 'delete', 'status'],
          description: 'Which contact-request operation to run.',
        },
        requestId: { type: 'string', description: 'Contact-request id to inspect.' },
        expectedVersion: { type: 'string', description: 'Request version read before mutating.' },
        recipientRef: { type: 'string', description: 'Who to add (send).' },
        greeting: { type: 'string', description: 'Message sent with the request (send).' },
        operationId: { type: 'string', description: 'Operation id (status).' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous list call.' },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'How many requests to return.' },
        idempotencyKey: { type: 'string', description: 'Required for accept/delete; also dedupes send.' },
      },
      required: ['action'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_contact_request');
      const account = String(principal.accountId);
      const action = String(args['action']);
      const limit = clampLimit(args['limit'], 20, 200);
      const key = () => requireId(args['idempotencyKey'], 'idempotencyKey');
      const version = () => requireId(args['expectedVersion'], 'expectedVersion');

      switch (action) {
        case 'list': {
          const page = await deps.client.contactRequests(
            account,
            { limit, ...cursor(args) },
            exec.signal,
          );
          return pageItems(page);
        }
        case 'get': {
          const requestId = requireId(args['requestId'], 'requestId');
          const request = await deps.client.contactRequest(account, requestId, exec.signal);
          return { request };
        }
        case 'send': {
          const recipientRef = requireId(args['recipientRef'], 'recipientRef');
          await revalidateForMutation(deps, principal, 'wechat_contact_request');
          const input: Record<string, unknown> = { recipientRef };
          if (typeof args['greeting'] === 'string') input['greeting'] = args['greeting'];
          const operation = await deps.client.sendContactRequest(account, input, key(), exec.signal);
          return { status: 'submitted', operation: operation ?? null };
        }
        case 'accept': {
          const requestId = requireId(args['requestId'], 'requestId');
          await revalidateForMutation(deps, principal, 'wechat_contact_request');
          const input = { expectedVersion: version() };
          const operation = await deps.client.acceptContactRequest(
            account,
            requestId,
            input,
            key(),
            exec.signal,
          );
          return { status: 'submitted', operation: operation ?? null };
        }
        case 'delete': {
          const requestId = requireId(args['requestId'], 'requestId');
          await revalidateForMutation(deps, principal, 'wechat_contact_request');
          const operation = await deps.client.deleteContactRequest(
            account,
            requestId,
            version(),
            key(),
            exec.signal,
          );
          return {
            status: 'submitted',
            operation: operation ?? null,
            note: 'Local request history deleted; the sender is not notified.',
          };
        }
        case 'status': {
          const operationId = requireId(args['operationId'], 'operationId');
          const operation = await deps.client.operation(account, operationId, exec.signal);
          return { operation: operation ?? null };
        }
        default:
          throw new Error(`unknown action: ${action}`);
      }
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_group_info
// ---------------------------------------------------------------------------

export function wechatGroupInfoTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_group_info',
    description:
      'Owner view of one WeChat group: metadata plus a member page. Read-only; a remaining nextCursor means the member list is incomplete (absence from one page is not proof a member is gone).',
    parameters: {
      type: 'object',
      properties: {
        groupId: { type: 'string', description: 'Group chat id, ending with @chatroom.' },
        username: { type: 'string', description: 'Optional member filter.' },
        cursor: { type: 'string', description: 'Opaque member-page cursor from a previous call.' },
        limit: { type: 'integer', minimum: 1, maximum: 100, description: 'How many members to return.' },
      },
      required: ['groupId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_group_info');
      const account = String(principal.accountId);
      const groupId = requireId(args['groupId'], 'groupId');
      if (!groupId.endsWith('@chatroom')) {
        throw new Error('groupId must be a group chat id ending with @chatroom');
      }
      const limit = clampLimit(args['limit'], 50, 100);
      const group = await deps.client.group(account, groupId, exec.signal);
      const memberOptions: Record<string, unknown> = {
        limit,
        ...cursor(args),
        ...(typeof args['username'] === 'string' ? { username: args['username'] } : {}),
      };
      const members = await deps.client.groupMembers(account, groupId, memberOptions, exec.signal);
      const page = pageItems(members);
      return {
        group: group ?? null,
        members: page.items,
        unconfirmed: page.nextCursor !== undefined,
        ...(page.nextCursor === undefined ? {} : { nextCursor: page.nextCursor }),
      };
    },
  });
}

// ---------------------------------------------------------------------------
// Archive tools
// ---------------------------------------------------------------------------

export function wechatArchiveThreadsTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_archive_threads',
    description:
      'List stored chats across the whole account (archive scope): query by name, page with cursor. Read-only.',
    parameters: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Optional title filter for stored threads.' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous call.' },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'How many threads to return.' },
      },
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_archive_threads');
      const limit = clampLimit(args['limit'], 20, 200);
      const options: Record<string, unknown> = {
        limit,
        ...cursor(args),
        ...(typeof args['query'] === 'string' && args['query'].length > 0
          ? { query: args['query'] }
          : {}),
      };
      const page = await deps.client.threads(String(principal.accountId), options);
      return pageItems(page);
    },
  });
}

export function wechatArchiveSearchTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_archive_search',
    description:
      'Search stored messages across the whole account, optionally inside one chat (archive scope). Read-only.',
    parameters: {
      type: 'object',
      properties: {
        query: { type: 'string', minLength: 1, description: 'Text to look for across this account.' },
        threadId: { type: 'string', description: 'Restrict the search to one chat.' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous call.' },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'How many matches to return.' },
      },
      required: ['query'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_archive_search');
      const query = requireId(args['query'], 'query');
      const limit = clampLimit(args['limit'], 20, 200);
      const options: Record<string, unknown> = {
        query,
        limit,
        ...cursor(args),
        ...(typeof args['threadId'] === 'string' && args['threadId'].length > 0
          ? { threadId: args['threadId'] }
          : {}),
      };
      const page = await deps.client.searchMessages(String(principal.accountId), options, exec.signal);
      const result = pageItems(page);
      return { ...result, items: result.items.map(compactMessage) };
    },
  });
}

export function wechatArchiveHistoryTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_archive_history',
    description:
      'Stored messages of any chat in this account (archive scope), newest first. Read-only.',
    parameters: {
      type: 'object',
      properties: {
        threadId: { type: 'string', minLength: 1, description: 'Stored chat to read (any chat in this account).' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous call.' },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'How many messages to return.' },
      },
      required: ['threadId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_archive_history');
      const threadId = requireId(args['threadId'], 'threadId');
      const limit = clampLimit(args['limit'], 20, 200);
      const page = await deps.client.messages(
        String(principal.accountId),
        threadId,
        { limit, ...cursor(args) },
        exec.signal,
      );
      const result = pageItems(page);
      return { ...result, items: result.items.map(compactMessage) };
    },
  });
}

export function wechatArchiveMessageTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_archive_message',
    description: 'Read one stored message anywhere in this account (archive scope). Read-only.',
    parameters: {
      type: 'object',
      properties: { messageId: { type: 'string', minLength: 1, description: 'Stored message ID.' } },
      required: ['messageId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_archive_message');
      const messageId = requireId(args['messageId'], 'messageId');
      const message = await deps.client.message(String(principal.accountId), messageId);
      return { message: compactMessage(message) };
    },
  });
}

export function wechatArchiveMediaTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_archive_media',
    description:
      'Attachment metadata for one stored message anywhere in this account (archive scope). Reports state/contentType/size/checksum; never downloads bytes.',
    parameters: {
      type: 'object',
      properties: { messageId: { type: 'string', minLength: 1, description: 'Stored message ID.' } },
      required: ['messageId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requireOwner(deps, exec, 'wechat_archive_media');
      const messageId = requireId(args['messageId'], 'messageId');
      const page = await deps.client.mediaAssets(
        String(principal.accountId),
        messageId,
        exec.signal,
      );
      const { items } = pageItems(page);
      const assets =
        typeof args['assetId'] === 'string'
          ? items.filter((asset) => asset['id'] === args['assetId'])
          : items;
      return { messageId, assets };
    },
  });
}

/** All owner + archive tools, in registration order. */
export function ownerTools(deps: ToolDeps): WechatToolDefinition[] {
  return [
    wechatContactsTool(deps),
    wechatContactRequestTool(deps),
    wechatGroupInfoTool(deps),
    wechatArchiveThreadsTool(deps),
    wechatArchiveSearchTool(deps),
    wechatArchiveHistoryTool(deps),
    wechatArchiveMessageTool(deps),
    wechatArchiveMediaTool(deps),
  ];
}
