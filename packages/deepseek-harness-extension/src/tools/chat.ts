/**
 * M3 current-chat tools — DESIGN.md §4 rows 1–2.
 *
 * Every tool is scoped to the thread the calling Agent is bound to: the
 * account and chat come from the trusted route binding, never from model
 * arguments. Cross-chat IDs are rejected after a native read.
 *
 * Tools: wechat_history, wechat_search, wechat_message, wechat_media,
 * wechat_recall, wechat_send_share.
 */

import { asServerId, type ToolPrincipal } from '../contracts.ts';
import {
  asChat,
  compactMessage,
  defineWechatTool,
  mutationKey,
  pageItems,
  requirePrincipal,
  revalidateForMutation,
  type ToolContentBlock,
  type ToolDeps,
  type WechatToolDefinition,
} from './shared.ts';

const MAX_PAGE = 200;

/** Reject an ID that is not a plain non-empty string. */
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

/** Read one message and prove it belongs to the principal's chat. */
async function readScopedMessage(
  deps: ToolDeps,
  principal: ToolPrincipal,
  messageId: string,
): Promise<Record<string, unknown>> {
  const message = (await deps.client.message(String(principal.accountId), messageId)) as Record<
    string,
    unknown
  >;
  if (message.threadId !== String(principal.chatJid)) {
    throw new Error('message does not belong to the current chat');
  }
  return message;
}

// ---------------------------------------------------------------------------
// wechat_history
// ---------------------------------------------------------------------------

export function wechatHistoryTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_history',
    description:
      'Earlier messages in the current WeChat chat, newest first. Read-only local history; does not fetch remote history or mark the chat read.',
    parameters: {
      type: 'object',
      properties: {
        limit: { type: 'integer', minimum: 1, maximum: 50, default: 20, description: 'How many messages to return.' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous call.' },
      },
      additionalProperties: false,
    },
    schema: {
      type: 'object',
      properties: {
        items: { type: 'array', items: { type: 'object', additionalProperties: true } },
        nextCursor: { type: 'string' },
      },
      additionalProperties: true,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_history');
      const limit = clampLimit(args.limit, 20, 50);
      const page = await deps.client.messages(
        String(principal.accountId),
        String(principal.chatJid),
        { limit, ...(typeof args.cursor === 'string' ? { cursor: args.cursor } : {}) },
        exec.signal,
      );
      const { items, nextCursor } = pageItems(page);
      const compact = items
        .map(compactMessage)
        .sort((a, b) => String(b['createdAt'] ?? '').localeCompare(String(a['createdAt'] ?? '')))
        .slice(0, limit);
      return { items: compact, ...(nextCursor === undefined ? {} : { nextCursor }) };
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_search
// ---------------------------------------------------------------------------

export function wechatSearchTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_search',
    description:
      'Search locally stored messages in the current WeChat chat for text. Read-only; never searches other chats or fetches remote history.',
    parameters: {
      type: 'object',
      properties: {
        query: { type: 'string', minLength: 1, description: 'Text to look for.' },
        limit: { type: 'integer', minimum: 1, maximum: 50, default: 20, description: 'How many matches to return.' },
        cursor: { type: 'string', description: 'Opaque page cursor from a previous call.' },
      },
      required: ['query'],
      additionalProperties: false,
    },
    schema: {
      type: 'object',
      properties: {
        items: { type: 'array', items: { type: 'object', additionalProperties: true } },
        nextCursor: { type: 'string' },
      },
      additionalProperties: true,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_search');
      const query = requireId(args.query, 'query');
      const limit = clampLimit(args.limit, 20, 50);
      // The daemon's local search is account-wide; enforce the chat scope here
      // and keep paging only while the in-chat result set is still short.
      const found: Array<Record<string, unknown>> = [];
      let cursor = typeof args.cursor === 'string' ? args.cursor : undefined;
      for (let page = 0; page < 5 && found.length < limit; page += 1) {
        const result = await deps.client.searchMessages(
          String(principal.accountId),
          {
            query,
            limit: MAX_PAGE,
            ...(cursor === undefined ? {} : { cursor }),
          },
          exec.signal,
        );
        const { items, nextCursor } = pageItems(result);
        for (const item of items) {
          if (item['threadId'] !== String(principal.chatJid)) continue;
          found.push(compactMessage(item));
          if (found.length >= limit) break;
        }
        if (nextCursor === undefined) {
          cursor = undefined;
          break;
        }
        cursor = nextCursor;
      }
      return {
        items: found,
        ...(cursor === undefined ? {} : { nextCursor: cursor }),
      };
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_message
// ---------------------------------------------------------------------------

export function wechatMessageTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_message',
    description:
      'Read one message by its stored message ID from the current WeChat chat. Read-only; rejects IDs from other chats.',
    parameters: {
      type: 'object',
      properties: {
        messageId: { type: 'string', minLength: 1, description: 'Stored message ID.' },
      },
      required: ['messageId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_message');
      const messageId = requireId(args.messageId, 'messageId');
      const message = await readScopedMessage(deps, principal, messageId);
      return { message: compactMessage(message) };
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_media
// ---------------------------------------------------------------------------

export function wechatMediaTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_media',
    description:
      'Download an attachment from a message in the current WeChat chat. Images are also returned as model-visible image content; files, voice and video are staged for the agent. Pending downloads are reported as pending and can be retried.',
    parameters: {
      type: 'object',
      properties: {
        messageId: { type: 'string', minLength: 1, description: 'Stored message ID.' },
        assetId: {
          type: 'string',
          description: 'Specific attachment id; omit to use the message attachments.',
        },
      },
      required: ['messageId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_media');
      const messageId = requireId(args.messageId, 'messageId');
      // Access scope is verified before any bytes are downloaded.
      await readScopedMessage(deps, principal, messageId);
      const account = String(principal.accountId);
      const assetsPage = await deps.client.mediaAssets(account, messageId, exec.signal);
      const assets: Array<Record<string, unknown>> = Array.isArray(assetsPage?.items)
        ? assetsPage.items
        : [];
      const wanted =
        typeof args.assetId === 'string'
          ? assets.filter((asset) => asset['id'] === args.assetId)
          : assets;
      const results: Array<Record<string, unknown>> = [];
      const blocks: ToolContentBlock[] = [];

      for (const asset of wanted) {
        const assetId = String(asset['id'] ?? '');
        const base = {
          assetId,
          messageId,
          kind: asset['kind'] ?? null,
          state: asset['state'] ?? null,
          contentType: asset['contentType'] ?? null,
          byteLength: asset['byteLength'] ?? null,
          filename: asset['filename'] ?? null,
          sha256: asset['sha256'] ?? null,
        };
        if (asset['state'] !== 'available') {
          // Never fabricate bytes: pending/expired/unsupported are reported as-is.
          results.push({ ...base, status: asset['state'] ?? 'unavailable' });
          continue;
        }
        try {
          const stream = await deps.client.mediaContentStream(account, assetId, {});
          const chunks: Uint8Array[] = [];
          for await (const chunk of stream.stream as AsyncIterable<Uint8Array>) {
            chunks.push(chunk);
          }
          const total = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
          const bytes = new Uint8Array(total);
          let offset = 0;
          for (const chunk of chunks) {
            bytes.set(chunk, offset);
            offset += chunk.byteLength;
          }
          const contentType = String(stream.contentType ?? base.contentType ?? 'application/octet-stream');
          const filename = String(base.filename ?? `${assetId}`);
          const kind = String(base.kind ?? 'file');
          if (kind === 'image' && deps.media !== undefined) {
            const saved = await deps.media.saveImage({
              data: bytes,
              mimeType: contentType,
              ...(filename.length > 0 ? { filename } : {}),
            });
            blocks.push({ type: 'image', attachment: { ref: saved.ref } });
            results.push({ ...base, status: 'staged', ref: saved.ref, mediaType: contentType });
          } else if (deps.media !== undefined) {
            const saved = await deps.media.saveFile({ data: bytes, mimeType: contentType, filename });
            results.push({ ...base, status: 'staged', ref: saved.ref, mediaType: contentType });
          } else {
            results.push({ ...base, status: 'downloaded', bytes: total, mediaType: contentType });
          }
        } catch (error) {
          results.push({
            ...base,
            status: 'error',
            error: error instanceof Error ? error.message : String(error),
          });
        }
      }

      const value = { messageId, assets: results };
      if (blocks.length === 0) return value;
      return {
        ...value,
        __blocks: blocks,
      };
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_recall
// ---------------------------------------------------------------------------

export function wechatRecallTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_recall',
    description:
      "Recall (unsend) a message this account sent in the current WeChat chat. Requires the stored message ID and its server ID; refuses messages that are not your own in this chat.",
    parameters: {
      type: 'object',
      properties: {
        messageId: { type: 'string', minLength: 1, description: 'Stored message ID.' },
        serverId: { type: 'string', minLength: 1, description: "WeChat's message server ID." },
      },
      required: ['messageId', 'serverId'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_recall');
      const messageId = requireId(args.messageId, 'messageId');
      const serverId = requireId(args.serverId, 'serverId');
      const message = await readScopedMessage(deps, principal, messageId);
      if (message.direction !== 'outgoing') {
        throw new Error('only messages sent by this account can be recalled');
      }
      if (message.serverId !== serverId) {
        throw new Error('serverId does not match the stored message');
      }
      // Revalidate right before the mutation.
      await revalidateForMutation(deps, principal, 'wechat_recall');
      const key = mutationKey('recall', [String(principal.accountId), messageId, serverId]);
      const operation = await deps.client.recallMessage(
        String(principal.accountId),
        messageId,
        serverId,
        key,
        exec.signal,
      );
      return {
        status: 'requested',
        operation: operation ?? null,
        // A requested recall is not a delivered result.
        note: 'Recall was submitted; verify the chat before assuming it succeeded.',
      };
    },
  });
}

// ---------------------------------------------------------------------------
// wechat_send_share
// ---------------------------------------------------------------------------

export function wechatSendShareTool(deps: ToolDeps): WechatToolDefinition {
  return defineWechatTool({
    deps,
    name: 'wechat_send_share',
    description:
      'Send a link card (title, URL, optional description and thumbnail) to the current WeChat chat. Requires an explicit idempotency key; an accepted send is not proof of delivery.',
    parameters: {
      type: 'object',
      properties: {
        title: { type: 'string', minLength: 1, description: 'Share card title.' },
        url: { type: 'string', minLength: 1, description: 'http(s) address the card opens.' },
        description: { type: 'string', description: 'Share card body text.' },
        thumbnailUrl: { type: 'string', description: 'http(s) preview image address.' },
        idempotencyKey: {
          type: 'string',
          minLength: 1,
          description: 'Stable key; a retry with the same key never sends twice.',
        },
      },
      required: ['title', 'url', 'idempotencyKey'],
      additionalProperties: false,
    },
    async execute(args, exec) {
      const principal = await requirePrincipal(deps, exec, 'wechat_send_share');
      const title = requireId(args.title, 'title');
      const url = requireId(args.url, 'url');
      const key = requireId(args.idempotencyKey, 'idempotencyKey');
      if (!/^https?:\/\//i.test(url)) throw new Error('url must be an http(s) address');
      if (typeof args.thumbnailUrl === 'string' && !/^https?:\/\//i.test(args.thumbnailUrl)) {
        throw new Error('thumbnailUrl must be an http(s) address');
      }
      // Revalidate right before the mutation.
      await revalidateForMutation(deps, principal, 'wechat_send_share');
      const input = {
        kind: 'share',
        threadId: String(principal.chatJid),
        title,
        url,
        ...(typeof args.description === 'string' ? { description: args.description } : {}),
        ...(typeof args.thumbnailUrl === 'string' ? { thumbnailUrl: args.thumbnailUrl } : {}),
      };
      const intent = await deps.client.sendShare(String(principal.accountId), input, key, exec.signal);
      return {
        status: 'accepted',
        send: {
          id: intent?.id ?? null,
          state: intent?.state ?? null,
          serverId: intent?.serverId ?? null,
        },
        note: 'Accepted by the daemon; this is not a delivery receipt.',
      };
    },
  });
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/** All current-chat tools, in registration order. */
export function currentChatTools(deps: ToolDeps): WechatToolDefinition[] {
  return [
    wechatHistoryTool(deps),
    wechatSearchTool(deps),
    wechatMessageTool(deps),
    wechatMediaTool(deps),
    wechatRecallTool(deps),
    wechatSendShareTool(deps),
  ];
}

// Re-export helpers used by sibling tool modules.
export { requireId, clampLimit, readScopedMessage };
