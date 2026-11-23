/**
 * Tool-layer foundations (M3) — DESIGN.md §4.
 *
 * Tools are thin, typed wrappers around the native client plus the trusted
 * tool context. They are declared as plain structural `ToolDefinition` objects
 * (no runtime import from `@deepseek-ai/dsh-tools`), which keeps the bundle free
 * of host internals while remaining registry-compatible.
 *
 * Every tool resolves its identity from `exec.agent.id`; model arguments never
 * carry account/owner facts.
 */

import {
  asAccountId,
  asChatJid,
  type AccountId,
  type ChatJid,
  type ToolPrincipal,
} from '../contracts.ts';
import type { TrustedToolContext } from '../tool-context.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { WechatTables } from '../storage.ts';

// ---------------------------------------------------------------------------
// Structural host shapes (no runtime host imports)
// ---------------------------------------------------------------------------

/** Execution context DSH hands to a tool body. */
export interface ToolRunContextLike {
  readonly agent?: { readonly id: string } | undefined;
  readonly signal: AbortSignal;
}

/** Structural subset of the shared `NativeWeChatClient` used by tools. */
export interface NativeToolsLike {
  threads(accountId: string, options?: string | Record<string, unknown>): Promise<any>;
  thread(accountId: string, threadId: string, signal?: AbortSignal): Promise<any>;
  messages(
    accountId: string,
    threadId: string,
    options?: string | Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<any>;
  message(accountId: string, messageId: string): Promise<any>;
  searchMessages(
    accountId: string,
    options: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<any>;
  mediaAssets(accountId: string, messageId: string, signal?: AbortSignal): Promise<any>;
  mediaContentStream(
    accountId: string,
    assetId: string,
    options?: Record<string, unknown>,
  ): Promise<any>;
  sendText(
    accountId: string,
    threadId: string,
    text: string,
    key: string,
    quoteOrOptions?: Record<string, unknown>,
  ): Promise<any>;
  sendShare(accountId: string, input: Record<string, unknown>, key: string, signal?: AbortSignal): Promise<any>;
  recallMessage(
    accountId: string,
    messageId: string,
    serverId: string,
    key: string,
    signal?: AbortSignal,
  ): Promise<any>;
  stickerCollections(accountId: string, signal?: AbortSignal): Promise<any>;
  customStickers(accountId: string, signal?: AbortSignal): Promise<any>;
  customSticker(accountId: string, stickerId: string, signal?: AbortSignal): Promise<any>;
  prepareSticker(accountId: string, input: Record<string, unknown>, key: string, signal?: AbortSignal): Promise<any>;
  addCustomSticker(accountId: string, input: Record<string, unknown>, key: string, signal?: AbortSignal): Promise<any>;
  moveCustomSticker(
    accountId: string,
    stickerId: string,
    input: Record<string, unknown>,
    revision: string,
    key: string,
    signal?: AbortSignal,
  ): Promise<any>;
  removeCustomSticker(
    accountId: string,
    stickerId: string,
    revision: string,
    key: string,
    signal?: AbortSignal,
  ): Promise<any>;
  login(accountId: string, options?: Record<string, unknown>, key?: string): Promise<any>;
  operation(accountId: string, operationId: string, signal?: AbortSignal): Promise<any>;
  disconnect(accountId: string): Promise<any>;
  logoutLocal(accountId: string): Promise<any>;
  forgetCredentials(accountId: string, idempotencyKey: string, confirm: true): Promise<any>;
  accounts(): Promise<{ items: Array<Record<string, unknown>> }>;
  contacts(accountId: string, options?: unknown, signal?: AbortSignal): Promise<any>;
  contact(accountId: string, username: string, signal?: AbortSignal): Promise<any>;
  lookupContacts(accountId: string, ids: string[], signal?: AbortSignal): Promise<any>;
  searchContacts(accountId: string, query: string, signal?: AbortSignal): Promise<any>;
  removeContact(accountId: string, username: string, key: string, signal?: AbortSignal): Promise<any>;
  contactRequests(accountId: string, options?: Record<string, unknown>, signal?: AbortSignal): Promise<any>;
  contactRequest(accountId: string, requestId: string, signal?: AbortSignal): Promise<any>;
  sendContactRequest(accountId: string, input: Record<string, unknown>, key: string, signal?: AbortSignal): Promise<any>;
  acceptContactRequest(accountId: string, requestId: string, input: unknown, key: string, signal?: AbortSignal): Promise<any>;
  deleteContactRequest(
    accountId: string,
    requestId: string,
    expectedVersion: string,
    key: string,
    signal?: AbortSignal,
  ): Promise<any>;
  group(accountId: string, roomId: string, signal?: AbortSignal): Promise<any>;
  groupMembers(accountId: string, roomId: string, options?: unknown, signal?: AbortSignal): Promise<any>;
}

/** Host attachment store subset used to surface media to the model. */
export interface MediaSinkLike {
  /** Store verified image bytes; returns a durable image reference. */
  saveImage(input: {
    readonly data: Uint8Array;
    readonly mimeType: string;
    readonly filename?: string;
  }): Promise<{ readonly ref: string }>;
  /** Store arbitrary file bytes (documents, voice, video). */
  saveFile(input: {
    readonly data: Uint8Array;
    readonly mimeType: string;
    readonly filename: string;
  }): Promise<{ readonly ref: string }>;
}

// ---------------------------------------------------------------------------
// Tool definition shape
// ---------------------------------------------------------------------------

export interface TextContentBlock {
  readonly type: 'text';
  readonly text: string;
}

export interface ImageContentBlock {
  readonly type: 'image';
  readonly attachment: { readonly ref: string };
}

export type ToolContentBlock = TextContentBlock | ImageContentBlock;

/** Registry-compatible tool definition (structural mirror of DSH ToolDefinition). */
export interface WechatToolDefinition {
  readonly name: string;
  readonly description: string;
  readonly parameters: Record<string, unknown>;
  readonly output: {
    readonly schema: Record<string, unknown>;
    render(args: unknown, value: unknown): ToolContentBlock[];
  };
  execute(args: unknown, exec: ToolRunContextLike): Promise<unknown>;
}

/** Structural view of the call coordinator the wechat_call tool drives. */
export interface CallCoordinatorLike {
  status(): {
    state: string;
    available: boolean;
    calls: Array<{ callId: string; groupId?: string; contactId: string; direction: string; toolPolicy: string }>;
  };
  dial(contactId: string): Promise<unknown>;
  conferenceStart(groupId: string, invitees: string[]): Promise<unknown>;
  invite(callId: string, contactIds: string[]): Promise<unknown>;
  hangupCall(callId?: string): Promise<boolean>;
}

/**
 * One-way notification sender behind `wechat_push`. Present only when
 * `push.enabled` is true with at least one configured target; the whitelist
 * IS the authorization — no route binding or owner flag is consulted, so the
 * tool works from any DSH session (user decision 2025-11-23).
 */
export interface PushSenderLike {
  /** Exact wxids / @chatroom ids the tool may deliver to. */
  readonly targets: readonly string[];
  /** Deliver text to one configured target through the durable outbox. */
  deliver(target: string, text: string): Promise<{ status: string; key: string }>;
}

export interface ToolDeps {
  readonly client: NativeToolsLike;
  readonly context: TrustedToolContext;
  readonly accountId: AccountId;
  readonly log: (level: 'info' | 'warn' | 'error', message: string) => void;
  readonly now: () => number;
  readonly media?: MediaSinkLike;
  /** Present when the calls feature is configured and enabled. */
  readonly calls?: CallCoordinatorLike;
  /** Exact contact IDs allowed to start calls through the tool. */
  readonly outboundToolAllowFrom?: readonly string[];
  /** Present when message push is enabled with at least one target. */
  readonly push?: PushSenderLike;
}

/** JSON text renderer used by every tool that returns data. */
export function renderJson(_args: unknown, value: unknown): ToolContentBlock[] {
  return [{ type: 'text', text: JSON.stringify(value, null, 2) }];
}

/**
 * Declare one tool with a JSON-object output schema and JSON text rendering.
 * `parameters` is the model-facing JSON Schema.
 */
export function defineWechatTool(spec: {
  readonly name: string;
  readonly description: string;
  readonly parameters: Record<string, unknown>;
  readonly schema?: Record<string, unknown>;
  readonly render?: (args: unknown, value: unknown) => ToolContentBlock[];
  readonly execute: (args: any, exec: ToolRunContextLike, deps: ToolDeps) => Promise<unknown>;
  readonly deps: ToolDeps;
}): WechatToolDefinition {
  return {
    name: spec.name,
    description: spec.description,
    parameters: spec.parameters,
    output: {
      schema: spec.schema ?? { type: 'object', additionalProperties: true },
      render: spec.render ?? renderJson,
    },
    execute: (args, exec) => spec.execute(args, exec, spec.deps),
  };
}

/** Resolve the trusted principal for a tool call; throws ToolAccessError. */
export async function requirePrincipal(
  deps: ToolDeps,
  exec: ToolRunContextLike,
  toolName: string,
): Promise<ToolPrincipal> {
  const agentId = exec.agent?.id;
  if (agentId === undefined) {
    throw new ToolAccessError(`${toolName}: no calling Agent`, toolName);
  }
  return deps.context.require(agentId, toolName);
}

/** Revalidate immediately before a mutation (async-gap safety). */
export async function revalidateForMutation(
  deps: ToolDeps,
  principal: ToolPrincipal,
  toolName: string,
): Promise<ToolPrincipal> {
  return deps.context.revalidate(principal, toolName);
}

export const asChat = (value: unknown): ChatJid => {
  if (typeof value !== 'string' || value.length === 0) {
    throw new Error('chat id is required');
  }
  return asChatJid(value);
};

export const asAccount = (value: unknown): AccountId => {
  if (typeof value !== 'string' || value.length === 0) throw new Error('account id is required');
  return asAccountId(value);
};

/** Coerce a native page into `{items, nextCursor}` without leaking tombstones. */
export function pageItems(page: unknown): { items: Array<Record<string, unknown>>; nextCursor?: string } {
  const raw = (page ?? {}) as { items?: unknown; nextCursor?: unknown };
  const items = Array.isArray(raw.items)
    ? (raw.items as Array<Record<string, unknown>>).filter(
        (item) => item !== null && typeof item === 'object' && item['deleted'] !== true,
      )
    : [];
  return {
    items,
    ...(typeof raw.nextCursor === 'string' ? { nextCursor: raw.nextCursor } : {}),
  };
}

/** Normalize a native message into the compact model-facing shape. */
export function compactMessage(raw: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {
    id: raw['id'],
    threadId: raw['threadId'],
    serverId: raw['serverId'] ?? null,
    senderId: raw['senderId'] ?? null,
    direction: raw['direction'] ?? null,
    type: raw['type'],
    body: raw['body'] ?? '',
    createdAt: raw['createdAt'] ?? null,
  };
  if (raw['appMessageType'] !== undefined) out['appMessageType'] = raw['appMessageType'];
  if (raw['quote'] !== undefined) out['quote'] = raw['quote'];
  if (raw['recall'] !== undefined) out['recalled'] = true;
  if (raw['mentions'] !== undefined) out['mentions'] = raw['mentions'];
  return out;
}

/** Stable idempotency key for a tool mutation. */
export function mutationKey(prefix: string, parts: readonly (string | number)[]): string {
  return `${prefix}:${parts.join(':')}`;
}

export { ToolAccessError };
