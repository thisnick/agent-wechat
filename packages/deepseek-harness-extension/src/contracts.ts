/**
 * Shared domain contracts for the DeepSeek Harness WeChat extension.
 *
 * These types are the vocabulary shared by all 8 ports in `ports.ts`. They
 * deliberately avoid importing host (`@deepseek-ai/*`) or daemon
 * (`@agent-wechat/shared`) types so ports stay testable with pure fakes.
 * Adapters bind them to concrete SDK types in later milestones.
 *
 * Security invariants encoded here (see DESIGN.md §0):
 *  - identity is Always host-derived, never model-supplied;
 *  - every mutable record carries a generation/revision for async revalidation;
 *  - outbound results are not 'verified' until canonical readback matches.
 */

declare const brand: unique symbol;
type Branded<T extends string> = string & { readonly [brand]: T };

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

export type AccountId = Branded<'AccountId'>;
export type ChatJid = Branded<'ChatJid'>;
export type SenderId = Branded<'SenderId'>;
export type SessionId = Branded<'SessionId'>;
export type ServerId = Branded<'ServerId'>;
export type RouteKey = Branded<'RouteKey'>;

export const asAccountId = (value: string): AccountId => value as AccountId;
export const asChatJid = (value: string): ChatJid => value as ChatJid;
export const asSenderId = (value: string): SenderId => value as SenderId;
export const asSessionId = (value: string): SessionId => value as SessionId;
export const asServerId = (value: string): ServerId => value as ServerId;
export const asRouteKey = (value: string): RouteKey => value as RouteKey;

/** `routeKey = <accountId>:<chatJid>` (DESIGN.md §3.2). */
export const makeRouteKey = (accountId: AccountId, chatJid: ChatJid): RouteKey =>
  asRouteKey(`${accountId}:${chatJid}`);

/** Split on the first colon; chat JIDs never contain a colon. */
export const parseRouteKey = (key: RouteKey): { accountId: AccountId; chatJid: ChatJid } => {
  const at = key.indexOf(':');
  if (at <= 0 || at === key.length - 1) throw new Error('Invalid route key');
  return { accountId: asAccountId(key.slice(0, at)), chatJid: asChatJid(key.slice(at + 1)) };
};

/** FNV-1a 32-bit hex — deterministic, dependency-free, safe for IDs. */
export const stableHash32 = (input: string): string => {
  let hash = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    hash ^= input.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
};

/** Stable per-message dispatch UserMessage id (DESIGN.md §3.1). */
export const wechatNativeMessageId = (
  accountId: AccountId,
  serverId: ServerId,
  chatJid: ChatJid,
  serverMessageId: string,
): string =>
  `wechat-native:${accountId}:${serverId}:${stableHash32(chatJid)}:${stableHash32(serverMessageId)}`;

// ---------------------------------------------------------------------------
// Inbound
// ---------------------------------------------------------------------------

export type MessageKind = 'text' | 'image' | 'voice' | 'file' | 'sticker' | 'share' | 'system';
export type MediaKind = 'image' | 'voice' | 'file' | 'sticker';

export interface InboundRef {
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly serverId: ServerId;
  readonly senderId?: SenderId;
}

export interface InboundMedia {
  readonly assetId: string;
  readonly mimeType: string;
  readonly kind: MediaKind;
}

export interface InboundMessage {
  /** Stable dispatch id from `wechatNativeMessageId`. */
  readonly id: string;
  readonly ref: InboundRef;
  readonly serverMessageId: string;
  readonly kind: MessageKind;
  readonly text: string;
  readonly media: readonly InboundMedia[];
  readonly mentioned: boolean;
  readonly createdAt: string;
}

export type AdmissionReason =
  | 'admitted'
  | 'disabled'
  | 'account-not-allowed'
  | 'sender-not-allowed'
  | 'group-not-allowed'
  | 'missing-mention'
  | 'tombstone';

export interface AdmissionDecision {
  readonly admitted: boolean;
  readonly reason: AdmissionReason;
  /** Fingerprint of the policy version that produced this decision. */
  readonly policyFingerprint: string;
}

// ---------------------------------------------------------------------------
// Trusted tool context
// ---------------------------------------------------------------------------

export type ToolScope = 'current-chat' | 'account' | 'owner' | 'archive' | 'call';

export interface ToolGrant {
  readonly scope: ToolScope;
  readonly tools: readonly string[];
}

/** Host-derived principal for one Agent. Never populated from model args. */
export interface ToolPrincipal {
  readonly agentId: string;
  readonly routeKey: RouteKey;
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly senderId?: SenderId;
  readonly isOwner: boolean;
  readonly grants: readonly ToolGrant[];
  /** Config generation at resolution time; revalidate before every mutation. */
  readonly generation: number;
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

export interface RouteBinding {
  readonly routeKey: RouteKey;
  readonly sessionId: SessionId;
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly policyFingerprint: string;
  /** CAS token for resume/dispose races. */
  readonly revision: number;
  readonly lastActivityAt: string;
}

/** Minimal message shape the router injects; adapters map to DSH UserMessage. */
export interface FollowupMessage {
  readonly id: string;
  readonly text: string;
  readonly source?: string;
}

// ---------------------------------------------------------------------------
// Outbound
// ---------------------------------------------------------------------------

export type OutboundStatus = 'pending' | 'sent' | 'needs_review' | 'failed';
export type OutboundKind = 'text' | 'media';

export interface OutboundRecord {
  /** Stable idempotency key — reused verbatim for `sendByKey` recovery. */
  readonly key: string;
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly kind: OutboundKind;
  readonly payload: string;
  readonly status: OutboundStatus;
  readonly serverId?: ServerId;
  readonly serverMessageId?: string;
  readonly reason?: string;
  readonly createdAt: string;
  readonly updatedAt: string;
}

// ---------------------------------------------------------------------------
// Prepared run (native dispatch proof)
// ---------------------------------------------------------------------------

export type PreparedRunStatus = 'prepared' | 'claimed' | 'consumed' | 'released';

export interface PreparedRun {
  readonly runId: string;
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly messageIds: readonly string[];
  /** Context text (history/catch-up) carried with the run. */
  readonly contextText: string;
  readonly status: PreparedRunStatus;
  readonly claimedBy?: string;
  readonly createdAt: string;
}

// ---------------------------------------------------------------------------
// Media / transcription
// ---------------------------------------------------------------------------

export interface SaveImageInput {
  readonly data: Uint8Array;
  readonly mimeType: string;
  readonly filename?: string;
}

export interface SaveFileInput {
  readonly data: Uint8Array;
  readonly mimeType: string;
  readonly filename: string;
}

export interface StoredImageData {
  readonly ref: string;
  readonly bytes: Uint8Array;
  readonly mimeType: string;
}

export interface AdmittedPart {
  readonly type: 'text' | 'image' | 'file';
  readonly text?: string;
  readonly ref?: string;
  readonly mimeType?: string;
}

export interface TranscribeInput {
  readonly bytes: Uint8Array;
  readonly mimeType: string;
  readonly signal?: AbortSignal;
}

export interface Transcript {
  readonly text: string;
  readonly language?: string;
  readonly durationMs?: number;
}

// ---------------------------------------------------------------------------
// Fail-closed error
// ---------------------------------------------------------------------------

/** Raised when an optional capability (speech, facade, daemon) is unavailable. */
export class UnavailableError extends Error {
  readonly capability: string;
  constructor(message: string, capability: string) {
    super(message);
    this.name = 'UnavailableError';
    this.capability = capability;
  }
}
