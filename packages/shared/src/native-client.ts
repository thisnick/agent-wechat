/** Native v1 HTTP transport. This does not start a daemon or use the legacy API. */
import type { TextMentions } from "./native.generated.js";
export type NativeTextMentionOptions = { mentions: TextMentions };
import type { AccountSettings, NativeAccountRemoval, OfficialProfileRequest, OfficialProfileSelection, LoginRequest, QrRefresh, NativeTombstone, NativeMessage, QuoteSendReference, NativeThread, NativeContact, NativeGroup, NativeGroupMember, NativePage, NativeOperation, NativeSendIntent, ShareSendRequest, NativeEvent, NativeIdentity, NativeStatus, MediaAsset, MediaJob, NativeMediaJobRetryRequest, MediaTransferRequest, MediaSendRequest, OwnedMediaSource } from "./native.generated.js";
export type * from "./native.generated.js";
export { NATIVE_MAX_SOURCE_BYTES, NATIVE_MAX_ORDINARY_SOURCE_BYTES, NATIVE_MAX_VIDEO_SOURCE_BYTES } from "./native.generated.ts";
import { NATIVE_MAX_SOURCE_BYTES } from "./native.generated.ts";
import type { NativeGroupCreateRequest, NativeGroupMemberLookup } from "./native.generated.js";
export type NativeListOptions = { cursor?: string; limit?: number; query?: string; kind?: string };
export function isNativeTombstone(value: unknown): value is NativeTombstone { const item = value as Partial<NativeTombstone> | null; return item?.deleted === true && typeof item.id === "string" && typeof item.version === "string"; }
/** Validate a derived official notice without interpreting it as identity metadata. */
export function validateNativeRecallMetadata(value: unknown): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid native recall metadata");
  const recall = value as { replacementText?: unknown };
  if (recall.replacementText != null && (typeof recall.replacementText !== "string" || !recall.replacementText
    || recall.replacementText.length > 16384 || new TextEncoder().encode(recall.replacementText).length > 16384
    || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\ufffe\uffff]/.test(recall.replacementText))) throw new Error("Invalid native recall metadata");
}
function validateMessageRecall(message: NativeMessage): void { if (message.recall != null) validateNativeRecallMetadata(message.recall); }
function listQuery(input?: string | NativeListOptions): string { const options = typeof input === "string" ? { cursor: input } : input; if (!options) return ""; if (options.limit != null && (!Number.isInteger(options.limit) || options.limit < 1 || options.limit > 200)) throw new Error("Invalid native page limit"); const query = new URLSearchParams(); for (const [key, value] of Object.entries(options)) if (value != null) query.set(key, String(value)); return query.size ? `?${query}` : ""; }
export type NativeLoginOptions = Partial<Omit<LoginRequest, "qrRefresh">> & { qrRefresh?: Partial<QrRefresh> };
export type NativeClientOptions = { baseUrl: string; token: string; fetch?: typeof fetch; timeoutMs?: number; mediaTimeoutMs?: number; streamIdleTimeoutMs?: number };
export type NativeMediaContentOptions = { maxBytes?: number; range?: string; representation?: "original" | "preview"; signal?: AbortSignal };
export type NativeMediaContentMetadata = { contentType: string; etag: string | null; contentRange: string | null; sourceSha256: string | null; status: number };
export type NativeMediaContentStream = NativeMediaContentMetadata & { stream: AsyncIterable<Uint8Array>; cancel: () => Promise<void> };
export class NativeApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryable: boolean;
  constructor(status: number, code: string, retryable = false) {
    super(`Native API request failed (${status}, ${code})`);
    this.name = "NativeApiError"; this.status = status; this.code = code; this.retryable = retryable;
  }
}
export function validateNativeBaseUrl(input: string): URL {
  const url = new URL(input);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
  if (url.username || url.password || url.search || url.hash || !["http:", "https:"].includes(url.protocol)
    || (url.protocol === "http:" && !loopback)) throw new Error("Native API requires verified HTTPS or a loopback HTTP endpoint, without embedded credentials");
  if (!url.pathname.endsWith("/")) url.pathname += "/";
  return url;
}
const id = (value: string): string => {
  if (typeof value !== "string" || !value || value === "." || value === ".." || value.length > 4096 || /[\u0000-\u001f\u007f]/.test(value)) throw new Error("Invalid native resource ID");
  return encodeURIComponent(value);
};
const decimal = (value: string): string => {
  if (typeof value !== "string" || !/^\d+$/.test(value)) throw new Error("Native cursors must be decimal strings");
  return value;
};
export class NativeWeChatClient {
  private mediaBatchSupported = true;
  readonly baseUrl: URL;
  private readonly token: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;
  private readonly mediaTimeoutMs: number;
  private readonly streamIdleTimeoutMs: number;
  constructor(options: NativeClientOptions) {
    this.baseUrl = validateNativeBaseUrl(options.baseUrl);
    const nodeEnvironment = (globalThis as typeof globalThis & { process?: { env?: Record<string, string | undefined> } }).process?.env;
    if (this.baseUrl.protocol === "https:" && nodeEnvironment?.NODE_TLS_REJECT_UNAUTHORIZED === "0") throw new Error("Native API refuses HTTPS with TLS verification disabled");
    if (typeof options.token !== "string" || !options.token || /[\r\n]/.test(options.token)) throw new Error("Native API bearer token is required");
    this.token = options.token; this.fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis); this.timeoutMs = options.timeoutMs ?? 30000;
    this.mediaTimeoutMs = options.mediaTimeoutMs ?? 60 * 60000;
    if (!Number.isInteger(this.mediaTimeoutMs) || this.mediaTimeoutMs < 1 || this.mediaTimeoutMs > 60 * 60000) throw new Error("Invalid native media timeout");
    this.streamIdleTimeoutMs = options.streamIdleTimeoutMs ?? 45000;
    if (!Number.isInteger(this.streamIdleTimeoutMs) || this.streamIdleTimeoutMs < 1 || this.streamIdleTimeoutMs > 300000) throw new Error("Invalid native stream idle timeout");
  }
  private async response(path: string, init: RequestInit = {}, stream = false, timeoutMs = this.timeoutMs): Promise<Response> {
    const headers = new Headers(init.headers); headers.set("Authorization", `Bearer ${this.token}`);
    if (init.body != null && !(init.body instanceof FormData)) headers.set("Content-Type", "application/json");
    const timeout = stream ? undefined : AbortSignal.timeout(timeoutMs);
    const signal = init.signal && timeout ? AbortSignal.any([init.signal, timeout]) : init.signal ?? timeout;
    const response = await this.fetchImpl(new URL(path, this.baseUrl), { ...init, headers, redirect: "error", signal });
    if (!response.ok) {
      let code = "request_failed"; let retryable = response.status >= 500;
      try {
        const bytes = await this.readBounded(response, 64 * 1024);
        const body = JSON.parse(new TextDecoder().decode(bytes));
        if (typeof body.error?.code === "string" && /^[a-z0-9_]{1,80}$/.test(body.error.code)) code = body.error.code;
        if (typeof body.error?.retryable === "boolean") retryable = body.error.retryable;
      } catch { /* Do not expose response content or token-bearing URLs. */ }
      throw new NativeApiError(response.status, code, retryable);
    }
    return response;
  }
  private async readBounded(response: Response, max = 16 * 1024 * 1024): Promise<Uint8Array> {
    const reader = response.body?.getReader();
    if (!reader) return new Uint8Array();
    const chunks: Uint8Array[] = []; let size = 0;
    try {
      while (true) {
        const { value, done } = await reader.read(); if (done) break;
        size += value.byteLength;
        if (size > max) throw new Error("Native API response exceeds its bound");
        chunks.push(value);
      }
    } catch (error) { await reader.cancel().catch(() => {}); throw error; }
    finally { reader.releaseLock(); }
    const result = new Uint8Array(size); let offset = 0;
    for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.byteLength; }
    return result;
  }
  async request<T>(path: string, init: RequestInit = {}): Promise<T> {
    // Paths are constructed by these methods, never supplied as caller URLs.
    if (!path.startsWith("v1/") || path.includes("://")) throw new Error("Invalid native API path");
    const response = await this.response(path, init);
    if (response.status === 204) return undefined as T;
    return JSON.parse(new TextDecoder().decode(await this.readBounded(response))) as T;
  }
  status(): Promise<NativeStatus> { return this.request("v1/status"); }
  accounts(): Promise<{ items: Record<string, unknown>[] }> { return this.request("v1/accounts"); }
  account(accountId: string): Promise<Record<string, unknown>> { return this.request(`v1/accounts/${id(accountId)}`); }
  createAccount(input: string | { id?: string; alias?: string }): Promise<Record<string, unknown>> { const body = typeof input === "string" ? { id: input } : input; if (!body.id && !body.alias) throw new Error("Account ID or alias is required"); return this.request("v1/accounts", { method: "POST", body: JSON.stringify(body) }); }
  removeAccount(accountId: string, confirmAccountId: string): Promise<NativeAccountRemoval> { if (accountId !== confirmAccountId) throw new Error("Account confirmation does not match"); return this.request(`v1/accounts/${id(accountId)}`, { method: "DELETE", body: JSON.stringify({ confirmAccountId }) }); }
  updateAccount(accountId: string, options: { alias?: string; autoConnect?: boolean }): Promise<Record<string, unknown>> { return this.request(`v1/accounts/${id(accountId)}`, { method: "PATCH", body: JSON.stringify(options) }); }
  accountSettings(accountId: string, signal?: AbortSignal): Promise<AccountSettings> { return this.request(`v1/accounts/${id(accountId)}/settings`, { signal }); }
  updateAccountSettings(accountId: string, settings: AccountSettings, signal?: AbortSignal): Promise<AccountSettings> {
    if (!Number.isSafeInteger(settings.autoDownloadMaxBytes) || settings.autoDownloadMaxBytes < 0 || settings.autoDownloadMaxBytes > NATIVE_MAX_SOURCE_BYTES) throw new Error("Invalid automatic download limit");
    return this.request(`v1/accounts/${id(accountId)}/settings`, { method: "PATCH", body: JSON.stringify(settings), signal });
  }
  capabilities(accountId: string): Promise<Record<string, unknown>> { return this.request(`v1/accounts/${id(accountId)}/capabilities`); }
  connect(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/connect`, { method: "POST", body: "{}" }); }
  sync(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/sync`, { method: "POST", body: "{}" }); }
  /** Attach stopped official files on the daemon host; requires global admin. */
  prepareOfficialProfile(options: Partial<OfficialProfileRequest> = {}): Promise<OfficialProfileSelection> {
    return this.request("v1/profiles/official", { method: "POST", body: JSON.stringify(options) });
  }
  login(accountId: string, options: NativeLoginOptions = {}, key?: string): Promise<NativeOperation> {
    if (key != null && (!key || key.length > 200 || /[\r\n]/.test(key))) throw new Error("Invalid login idempotency key");
    return this.request(`v1/accounts/${id(accountId)}/auth/login`, { method: "POST", headers: key ? { "Idempotency-Key": key } : undefined, body: JSON.stringify(options) });
  }
  operation(accountId: string, operationId: string, signal?: AbortSignal): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/operations/${id(operationId)}`, { signal }); }
  /** Submit once with a stable key; inspect the returned operation after uncertainty. */
  async recallMessage(accountId: string, messageId: string, serverId: string, idempotencyKey: string, signal?: AbortSignal): Promise<NativeOperation> {
    if (typeof serverId !== "string" || !/^[1-9]\d{0,19}$/.test(serverId) || BigInt(serverId) > 18446744073709551615n) throw new Error("Invalid exact recall server ID");
    if (typeof idempotencyKey !== "string" || !idempotencyKey || idempotencyKey.length > 200 || /[\r\n]/.test(idempotencyKey)) throw new Error("Invalid recall idempotency key");
    const operation = await this.request<NativeOperation>(`v1/accounts/${id(accountId)}/messages/${id(messageId)}/recall`, {
      method: "POST", headers: { "Idempotency-Key": idempotencyKey }, body: JSON.stringify({ serverId }), signal,
    });
    if (!operation || operation.accountId !== accountId || operation.kind !== "message_recall" || typeof operation.id !== "string" || !operation.id || typeof operation.state !== "string") throw new Error("Native recall operation identity is invalid");
    return operation;
  }
  cancelOperation(accountId: string, operationId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/operations/${id(operationId)}`, { method: "DELETE" }); }
  async qr(accountId: string, operationId: string, generation?: number): Promise<Uint8Array> {
    if (generation != null && (!Number.isSafeInteger(generation) || generation < 1)) throw new Error("Invalid QR generation");
    const response = await this.response(`v1/accounts/${id(accountId)}/operations/${id(operationId)}/qr${generation == null ? "" : `?generation=${generation}`}`);
    if (!response.headers.get("content-type")?.startsWith("image/png")) throw new Error("Native login QR response is not PNG");
    return this.readBounded(response, 1024 * 1024);
  }
  disconnect(accountId: string): Promise<unknown> { return this.request(`v1/accounts/${id(accountId)}/auth/disconnect`, { method: "POST", body: "{}" }); }
  logoutLocal(accountId: string): Promise<unknown> { return this.request(`v1/accounts/${id(accountId)}/auth/logout-local`, { method: "POST", body: "{}" }); }
  logout(accountId: string, idempotencyKey: string): Promise<NativeOperation> {
    if (!idempotencyKey || /[\r\n]/.test(idempotencyKey)) throw new Error("An idempotency key is required");
    return this.request(`v1/accounts/${id(accountId)}/logout`, { method: "POST", headers: { "Idempotency-Key": idempotencyKey }, body: "{}" });
  }
  forgetCredentials(accountId: string, idempotencyKey: string, confirm: true): Promise<NativeOperation> {
    if (confirm !== true || !idempotencyKey || /[\r\n]/.test(idempotencyKey)) throw new Error("Explicit confirmation and an idempotency key are required");
    return this.request(`v1/accounts/${id(accountId)}/forget-credentials`, { method: "POST", headers: { "Idempotency-Key": idempotencyKey }, body: JSON.stringify({ confirm: true }) });
  }
  threads(accountId: string, options?: string | NativeListOptions): Promise<NativePage<NativeThread>> { return this.request(`v1/accounts/${id(accountId)}/threads${listQuery(options)}`); }
  contacts(accountId: string, options?: string | NativeListOptions): Promise<NativePage<NativeContact>> { return this.request(`v1/accounts/${id(accountId)}/contacts${listQuery(options)}`); }
  groups(accountId: string, options?: string | NativeListOptions): Promise<NativePage<NativeGroup>> { return this.request(`v1/accounts/${id(accountId)}/groups${listQuery(options)}`); }
  thread(accountId: string, threadId: string, signal?: AbortSignal): Promise<NativeThread> { return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}`, { signal }); }
  contact(accountId: string, username: string): Promise<NativeContact> { return this.request(`v1/accounts/${id(accountId)}/contacts/${id(username)}`); }
  group(accountId: string, roomId: string, signal?: AbortSignal): Promise<NativeGroup> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}`, { signal }); }
  groupMembers(accountId: string, roomId: string, options?: string | NativeListOptions, signal?: AbortSignal): Promise<NativePage<NativeGroupMember>> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/members${listQuery(options)}`, { signal }); }
  groupMember(accountId: string, roomId: string, username: string, signal?: AbortSignal): Promise<NativeGroupMemberLookup> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/members/${id(username)}`, { signal }); }
  private async groupMutation(accountId: string, path: string, method: string, body: unknown, key: string, action: string, target: string | null, signal?: AbortSignal): Promise<NativeOperation> {
    if (typeof key !== "string" || !key || key.length > 200 || /[\u0000-\u001f\u007f]/.test(key)) throw new Error("A stable group idempotency key is required");
    const operation = await this.request<NativeOperation>(`v1/accounts/${id(accountId)}/${path}`, { method, body: JSON.stringify(body), headers: { "Idempotency-Key": key }, signal });
    const result = operation?.result as Record<string, unknown> | undefined;
    if (!operation || operation.accountId !== accountId || operation.kind !== "group_mutation" || typeof operation.id !== "string" || !operation.id
      || !result || result.action !== action || result.targetId !== target) throw new Error("Native group operation identity is invalid");
    return operation;
  }
  /** A repeated key returns the same operation. Uncertain mutations must be inspected, never repeated with a new key. */
  createGroup(accountId: string, input: NativeGroupCreateRequest, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, "groups", "POST", input, key, "create", null, signal); }
  renameGroup(accountId: string, groupId: string, name: string, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}`, "PATCH", { name }, key, "rename", groupId, signal); }
  leaveGroup(accountId: string, groupId: string, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}/leave`, "POST", {}, key, "leave", groupId, signal); }
  addGroupMembers(accountId: string, groupId: string, members: string[], key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}/members`, "POST", { members }, key, "add_members", groupId, signal); }
  removeGroupMembers(accountId: string, groupId: string, members: string[], key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}/members`, "DELETE", { members }, key, "remove_members", groupId, signal); }
  setMyGroupName(accountId: string, groupId: string, displayName: string, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}/my-name`, "PUT", { displayName }, key, "my_name", groupId, signal); }
  clearGroupAnnouncement(accountId: string, groupId: string, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `groups/${id(groupId)}/announcement`, "DELETE", {}, key, "clear_announcement", groupId, signal); }
  setThreadMuted(accountId: string, threadId: string, muted: boolean, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.groupMutation(accountId, `threads/${id(threadId)}/notifications`, "PUT", { muted }, key, "mute", threadId, signal); }
  setThreadPinned(accountId: string, threadId: string, pinned: boolean, key: string, signal?: AbortSignal): Promise<NativeOperation> {
    if (typeof pinned !== "boolean") throw new Error("Thread pinned state must be a boolean");
    return this.groupMutation(accountId, `threads/${id(threadId)}/pin`, "PUT", { pinned }, key, "pin", threadId, signal);
  }
  groupMutationByKey(accountId: string, key: string, signal?: AbortSignal): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/group-mutations/by-idempotency-key?${new URLSearchParams({ key })}`, { signal }); }
  refreshAvatar(accountId: string, username: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/avatars/${id(username)}/refresh`, { method: "POST", body: "{}" }); }
  async avatar(accountId: string, username: string, signal?: AbortSignal): Promise<{ bytes: Uint8Array; contentType: string; etag: string | null }> { const response = await this.response(`v1/accounts/${id(accountId)}/avatars/${id(username)}`, { signal }); const type = response.headers.get("content-type") ?? ""; if (!type.startsWith("image/")) { await response.body?.cancel(); throw new Error("Cached avatar is not an image"); } return { bytes: await this.readBounded(response, 16 * 1024 * 1024), contentType: type, etag: response.headers.get("etag") }; }
  refreshContact(accountId: string, username: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/contacts/${id(username)}/refresh`, { method: "POST", body: "{}" }); }
  refreshGroup(accountId: string, roomId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/refresh`, { method: "POST", body: "{}" }); }
  refreshGroupMembers(accountId: string, roomId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/members/refresh`, { method: "POST", body: "{}" }); }
  syncContacts(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/contact-sync`, { method: "POST", body: "{}" }); }
  syncGroups(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/group-sync`, { method: "POST", body: "{}" }); }
  async messages(accountId: string, threadId: string, options?: string | NativeListOptions, signal?: AbortSignal): Promise<NativePage<NativeMessage>> {
    const result = await this.request<NativePage<NativeMessage>>(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/messages${listQuery(options)}`, { signal });
    for (const msg of result.items) {
      if (isNativeTombstone(msg)) continue;
      if (typeof msg.id !== "string" || msg.accountId !== accountId || msg.threadId !== threadId
        || (msg.serverId != null && (typeof msg.serverId !== "string" || !/^\d+$/.test(msg.serverId)))) throw new Error("Invalid native message identity");
      validateMessageRecall(msg);
    }
    return result;
  }
  async message(accountId: string, messageId: string): Promise<NativeMessage> {
    const message = await this.request<NativeMessage>(`v1/accounts/${id(accountId)}/messages/${id(messageId)}`);
    if (message.id !== messageId || message.accountId !== accountId || (message.serverId != null && (typeof message.serverId !== "string" || !/^\d+$/.test(message.serverId)))) throw new Error("Invalid native message identity");
    validateMessageRecall(message);
    return message;
  }
  /** Account-wide local search, with the daemon's query-bound opaque cursor. */
  async searchMessages(accountId: string, options: NativeListOptions): Promise<NativePage<NativeMessage>> {
    const result = await this.request<NativePage<NativeMessage>>(`v1/accounts/${id(accountId)}/messages${listQuery(options)}`);
    for (const message of result.items) {
      if (isNativeTombstone(message)) continue;
      if (typeof message.id !== "string" || message.accountId !== accountId || typeof message.threadId !== "string" || !message.threadId
        || (message.serverId != null && (typeof message.serverId !== "string" || !/^\d+$/.test(message.serverId)))) throw new Error("Invalid native archive message identity");
      validateMessageRecall(message);
    }
    return result;
  }
  /** Send text with optional group mentions, or quote a locally stored message in the same chat.
   * Quoted-reply live acceptance and recipient rendering remain unverified; unsupported targets are rejected.
   */
  sendText(accountId: string, threadId: string, text: string, idempotencyKey: string, quoteOrOptions?: QuoteSendReference | NativeTextMentionOptions): Promise<NativeSendIntent> {
    if (!idempotencyKey || /[\r\n]/.test(idempotencyKey)) throw new Error("An idempotency key is required");
    const options = quoteOrOptions && typeof quoteOrOptions === "object" && "mentions" in quoteOrOptions ? quoteOrOptions as NativeTextMentionOptions : undefined;
    const quote = options === undefined ? quoteOrOptions as QuoteSendReference | undefined : undefined;
    if (options !== undefined) {
      const mentions = options.mentions;
      const ids = mentions?.userIds ?? [];
      if (Object.keys(options).some(key => key !== "mentions") || !threadId.endsWith("@chatroom")
        || !mentions || typeof mentions !== "object" || Array.isArray(mentions)
        || Object.keys(mentions).some(key => key !== "userIds" && key !== "all" && key !== "positions")
        || (mentions.userIds !== undefined && !Array.isArray(mentions.userIds))
        || (mentions.all !== undefined && typeof mentions.all !== "boolean")
        || !Array.isArray(ids) || ids.length > 128 || new Set(ids).size !== ids.length
        || (mentions.all === true) === (ids.length > 0)
        || ids.some(id => typeof id !== "string" || !/^[A-Za-z0-9_@.-]{1,256}$/.test(id) || id === "notify@all")) throw new Error("Invalid text mentions");
      const positions = mentions.positions ?? [];
      if (mentions.positions !== undefined && !Array.isArray(mentions.positions) || positions.length > 256) throw new Error("Invalid text mentions");
      if (positions.length) {
        const boundary = (offset: number) => !(offset > 0 && offset < text.length
          && text.charCodeAt(offset - 1) >= 0xd800 && text.charCodeAt(offset - 1) <= 0xdbff
          && text.charCodeAt(offset) >= 0xdc00 && text.charCodeAt(offset) <= 0xdfff);
        const covered = new Set<number>();
        const recipients = mentions.all === true ? 1 : ids.length;
        let previous = 0;
        if (!text || text.includes("\0") || new TextEncoder().encode(text).length > 65536) throw new Error("Invalid text mentions");
        for (const position of positions) {
          if (!position || typeof position !== "object" || Array.isArray(position)
            || Object.keys(position).some(key => !["start", "end", "index"].includes(key))
            || !Number.isSafeInteger(position.start) || !Number.isSafeInteger(position.end) || !Number.isSafeInteger(position.index)
            || position.start < previous || position.start >= position.end || position.end > text.length
            || position.index < 0 || position.index >= recipients || !boundary(position.start) || !boundary(position.end)) throw new Error("Invalid text mentions");
          const span = text.slice(position.start, position.end);
          const label = span.slice(1, -1);
          if (!span.startsWith("@") || !span.endsWith("\u2005") || !label.trim()
            || new TextEncoder().encode(label).length > 1024
            || /[\u0000-\u001f\u007f-\u009f\u2005\ud800-\udfff]/u.test(label)) throw new Error("Invalid text mentions");
          covered.add(position.index); previous = position.end;
        }
        if (covered.size !== recipients) throw new Error("Invalid text mentions");
      }
    }
    if (quote !== undefined && (!quote || typeof quote !== "object" || Array.isArray(quote)
      || Object.keys(quote).some(key => key !== "messageId" && key !== "serverId")
      || typeof quote.messageId !== "string" || !quote.messageId || quote.messageId.length > 8192
      || typeof quote.serverId !== "string" || !/^[1-9]\d{0,19}$/.test(quote.serverId)
      || BigInt(quote.serverId) > 18446744073709551615n)) throw new Error("Invalid quote message identity");
    const input = options !== undefined ? { kind: "text", threadId, text, mentions: options.mentions }
      : quote === undefined ? { threadId, text } : { kind: "text", threadId, text, quote };
    return this.request(`v1/accounts/${id(accountId)}/sends`, { method: "POST", headers: { "Idempotency-Key": idempotencyKey }, body: JSON.stringify(input) });
  }
  send(accountId: string, sendId: string, signal?: AbortSignal): Promise<NativeSendIntent> { return this.request(`v1/accounts/${id(accountId)}/sends/${id(sendId)}`, { signal }); }
  sendShare(accountId: string, input: ShareSendRequest, key: string, signal?: AbortSignal): Promise<NativeSendIntent> {
    if (!key || key.length > 200 || /[\u0000-\u001f\u007f]/.test(key)) throw new Error("An idempotency key is required");
    return this.request(`v1/accounts/${id(accountId)}/sends`, { method: "POST", headers: { "Idempotency-Key": key }, body: JSON.stringify(input), signal });
  }
  sendByKey(accountId: string, key: string): Promise<NativeSendIntent | MediaJob> { if (!key || key.length > 200) throw new Error("Invalid idempotency key"); return this.request(`v1/accounts/${id(accountId)}/sends/by-idempotency-key?key=${encodeURIComponent(key)}`); }
  cancelSend(accountId: string, sendId: string): Promise<NativeSendIntent | MediaJob> { return this.request(`v1/accounts/${id(accountId)}/sends/${id(sendId)}/cancel`, { method: "POST", body: "{}" }); }
  /** A disk-backed Blob avoids holding the source in memory before staging. */
  async upload(accountId: string, bytes: Uint8Array | Blob, name: string, contentType = "application/octet-stream", signal?: AbortSignal): Promise<OwnedMediaSource> {
    const size = bytes instanceof Blob ? bytes.size : bytes.byteLength;
    if (!size || size > NATIVE_MAX_SOURCE_BYTES || !name || name.length > 255 || /[\r\n/\\]/.test(name)) throw new Error("Invalid bounded media upload");
    const part = bytes instanceof Blob ? bytes.slice(0, bytes.size, contentType) : new Blob([bytes.buffer instanceof ArrayBuffer ? new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength) : new Uint8Array(bytes)], { type: contentType });
    const form = new FormData(); form.set("file", part, name);
    const response = await this.response(`v1/accounts/${id(accountId)}/uploads`, { method: "POST", body: form, signal }, false, this.mediaTimeoutMs);
    return JSON.parse(new TextDecoder().decode(await this.readBounded(response))) as OwnedMediaSource;
  }
  sendMedia(accountId: string, input: MediaSendRequest, key: string): Promise<MediaJob> { if (!key || /[\r\n]/.test(key)) throw new Error("An idempotency key is required"); return this.request(`v1/accounts/${id(accountId)}/sends`, { method: "POST", headers: { "Idempotency-Key": key }, body: JSON.stringify(input) }); }
  mediaAssets(accountId: string, messageId: string, signal?: AbortSignal): Promise<{ items: MediaAsset[] }> { return this.request(`v1/accounts/${id(accountId)}/messages/${id(messageId)}/media`, { signal }); }
  async mediaAssetsBatch(accountId: string, messageIds: string[], signal?: AbortSignal): Promise<import("./native.generated.js").NativeMediaBatch> {
    if (!messageIds.length || messageIds.length > 40 || messageIds.some((id) => !id || id.length > 8192) || new Set(messageIds).size !== messageIds.length) throw new Error("Invalid media lookup batch");
    if (this.mediaBatchSupported) {
      try {
        const batch = await this.request<import("./native.generated.js").NativeMediaBatch>(`v1/accounts/${id(accountId)}/media/lookup`, { method: "POST", body: JSON.stringify({ messageIds }), signal });
        const expected = new Set(messageIds);
        if (!Array.isArray(batch.items) || batch.items.length !== expected.size) throw new Error("Invalid media lookup response");
        for (const item of batch.items) {
          if (!expected.delete(item.messageId) || !Array.isArray(item.assets) || (item.error !== null && typeof item.error !== "string") || item.assets.some((asset) => asset.accountId !== accountId || asset.messageId !== item.messageId)) throw new Error("Invalid media lookup identity");
        }
        return batch;
      } catch (error) {
        if (!(error instanceof NativeApiError) || ![404, 405].includes(error.status)) throw error;
        this.mediaBatchSupported = false;
      }
    }
    return { items: await Promise.all(messageIds.map(async (messageId) => {
      try { return { messageId, assets: (await this.mediaAssets(accountId, messageId, signal)).items, error: null }; }
      catch (error) { if (!(error instanceof NativeApiError)) throw error; return { messageId, assets: [], error: error.code }; }
    })) };
  }
  transferMedia(accountId: string, input: MediaTransferRequest): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/messages/${id(input.messageId)}/media-transfers`, { method: "POST", body: JSON.stringify(input) }); }
  mediaAsset(accountId: string, assetId: string): Promise<MediaAsset> { return this.request(`v1/accounts/${id(accountId)}/media/${id(assetId)}`); }
  job(accountId: string, jobId: string, signal?: AbortSignal): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}`, { signal }); }
  cancelJob(accountId: string, jobId: string): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}/cancel`, { method: "POST", body: "{}" }); }
  retryJob(accountId: string, jobId: string, options: NativeMediaJobRetryRequest = {}): Promise<MediaJob> {
    if (!options || typeof options !== "object" || Array.isArray(options) || Object.keys(options).some(key => key !== "reuseCompletedUpload")) throw new Error("Invalid media retry options");
    const reuse = options.reuseCompletedUpload;
    if (reuse !== undefined && typeof reuse !== "boolean") throw new Error("Invalid media retry options");
    return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}/${reuse === true ? "retry-completed-upload" : "retry"}`, { method: "POST", body: JSON.stringify(reuse === undefined ? {} : { reuseCompletedUpload: reuse }) });
  }
  repairMediaWriteback(accountId: string, limit = 25): Promise<NativeOperation> {
    if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new Error("Invalid media writeback limit");
    return this.request(`v1/accounts/${id(accountId)}/media-writeback/repair`, { method: "POST", body: JSON.stringify({ limit }) });
  }
  /** Consume the bounded stream once, or cancel it; this never starts a CDN transfer. */
  async mediaContentStream(accountId: string, assetId: string, options: NativeMediaContentOptions = {}): Promise<NativeMediaContentStream> {
    const max = options.maxBytes ?? 128 * 1024 * 1024;
    if (!Number.isInteger(max) || max < 1 || max > Math.max(128 * 1024 * 1024, NATIVE_MAX_SOURCE_BYTES) || (options.range && !/^bytes=(?:\d+-\d*|-\d+)$/.test(options.range)) || (options.representation != null && !["original", "preview"].includes(options.representation))) throw new Error("Invalid bounded content request");
    const query = options.representation === "preview" ? "?representation=preview" : "";
    const cancellation = new AbortController();
    const response = await this.response(`v1/accounts/${id(accountId)}/media/${id(assetId)}/content${query}`, { signal: options.signal ? AbortSignal.any([options.signal, cancellation.signal]) : cancellation.signal, headers: options.range ? { Range: options.range } : {} }, false, this.mediaTimeoutMs);
    const length = response.headers.get("content-length"); if (length && (!/^\d+$/.test(length) || Number(length) > max)) { await response.body?.cancel(); throw new Error("Native media response exceeds its bound"); }
    if (!response.body && length && Number(length) > 0) throw new Error("Native media response length mismatch");
    let activeReader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const iterate = async function* () {
      cancellation.signal.throwIfAborted(); options.signal?.throwIfAborted();
      const reader = response.body?.getReader(); if (!reader) return; activeReader = reader;
      let size = 0;
      try {
        while (true) { cancellation.signal.throwIfAborted(); options.signal?.throwIfAborted(); const { value, done } = await reader.read(); cancellation.signal.throwIfAborted(); options.signal?.throwIfAborted(); if (done) break; size += value.byteLength; if (size > max) throw new Error("Native media response exceeds its bound"); yield value; }
        if (length && size !== Number(length)) throw new Error("Native media response length mismatch");
      } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); activeReader = undefined; }
    };
    let claimed = false;
    const stream: AsyncIterable<Uint8Array> = { [Symbol.asyncIterator]() {
      if (claimed) throw new Error("Native media stream was already consumed");
      claimed = true; return iterate();
    } };
    return { stream, cancel: async () => { cancellation.abort(); await (activeReader ? activeReader.cancel() : response.body?.cancel())?.catch(() => {}); }, contentType: response.headers.get("content-type") ?? "application/octet-stream", etag: response.headers.get("etag"), contentRange: response.headers.get("content-range"), sourceSha256: response.headers.get("x-source-sha256"), status: response.status };
  }
  async mediaContent(accountId: string, assetId: string, options: NativeMediaContentOptions = {}): Promise<NativeMediaContentMetadata & { bytes: Uint8Array }> {
    const { stream, cancel: _cancel, ...metadata } = await this.mediaContentStream(accountId, assetId, options);
    const chunks: Uint8Array[] = []; let size = 0;
    for await (const chunk of stream) { chunks.push(chunk); size += chunk.byteLength; }
    const bytes = new Uint8Array(size); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return { ...metadata, bytes };
  }
  markRead(accountId: string, threadId: string, expectedVersion: string): Promise<unknown> {
    if (typeof expectedVersion !== "string" || !expectedVersion || expectedVersion.length > 128) throw new Error("Read version is required");
    return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/read-state`, { method: "POST", body: JSON.stringify({ expectedVersion }) });
  }

  events(accountId: string, after = "0", limit = 100): Promise<{ items: NativeEvent[]; nextCursor: string }> {
    if (!Number.isInteger(limit) || limit < 1 || limit > 200) throw new Error("Invalid event page limit");
    return this.request<{ items: NativeEvent[]; nextCursor: string }>(`v1/accounts/${id(accountId)}/events?after=${decimal(after)}&limit=${limit}`).then(page => {
      for (const event of page.items) if (event.kind === "message.recalled") validateNativeRecallMetadata(event.data);
      return page;
    });
  }
  ack(accountId: string, consumerId: string, cursor: string): Promise<unknown> {
    return this.request(`v1/accounts/${id(accountId)}/consumers/${id(consumerId)}/ack`, { method: "POST", body: JSON.stringify({ cursor: decimal(cursor) }) });
  }
  async *watch(accountId: string, after = "0", signal?: AbortSignal, options: { onActivity?: () => void } = {}): AsyncGenerator<NativeEvent> {
    const connection = new AbortController();
    const streamSignal = signal ? AbortSignal.any([signal, connection.signal]) : connection.signal;
    const bounded = async <T>(work: () => Promise<T>, milliseconds: number, reason: string): Promise<T> => {
      if (streamSignal.aborted) throw streamSignal.reason;
      let timer: ReturnType<typeof setTimeout> | undefined;
      let onAbort: () => void = () => {};
      const interrupted = new Promise<never>((_resolve, reject) => {
        onAbort = () => reject(streamSignal.reason ?? new Error("Native SSE cancelled"));
        streamSignal.addEventListener("abort", onAbort, { once: true });
        timer = setTimeout(() => connection.abort(new Error(reason)), milliseconds);
      });
      try { return await Promise.race([work(), interrupted]); }
      finally { clearTimeout(timer); streamSignal.removeEventListener("abort", onAbort); }
    };
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    const decoder = new TextDecoder(); let buffer = ""; let data: string[] = []; let dataBytes = 0; let eventCursor = ""; let eventType = "";
    try {
      const response = await bounded(() => this.response(`v1/accounts/${id(accountId)}/events/stream?after=${decimal(after)}`, { signal: streamSignal, headers: { Accept: "text/event-stream" } }, true), this.timeoutMs, "Native SSE connection timeout");
      if (!response.headers.get("content-type")?.startsWith("text/event-stream")) { await response.body?.cancel(); throw new Error("Expected native SSE response"); }
      reader = response.body?.getReader(); if (!reader) throw new Error("Native SSE has no body");
      while (true) {
        // Server keepalive comments arrive every 15 seconds. Bound only the
        // awaited read, not time spent yielding an event to a slow consumer.
        const activeReader = reader;
        const { value, done } = await bounded(() => activeReader.read(), this.streamIdleTimeoutMs, "Native SSE idle timeout"); if (done) break;
        options.onActivity?.();
        buffer += decoder.decode(value, { stream: true });
        if (buffer.length > 1024 * 1024) throw new Error("Native SSE frame exceeds its bound");
        let newline: number;
        while ((newline = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, newline).replace(/\r$/, ""); buffer = buffer.slice(newline + 1);
          if (!line) {
            if (data.length) {
              const payload = JSON.parse(data.join("\n"));
              if (eventType === "error") {
                const code = typeof payload.error?.code === "string" && /^[a-z0-9_]{1,80}$/.test(payload.error.code) ? payload.error.code : "stream_failed";
                throw new NativeApiError(code === "event_cursor_expired" ? 410 : 500, code, false);
              }
              const event = payload as NativeEvent;
              if (!event || typeof event.kind !== "string" || !event.data || typeof event.data !== "object") throw new Error("Invalid native event");
              if (event.kind === "message.recalled") validateNativeRecallMetadata(event.data);
              event.cursor = decimal(event.cursor ?? eventCursor);
              yield event;
            }
            data = []; dataBytes = 0; eventCursor = ""; eventType = "";
          } else if (line.startsWith("data:")) { const item = line.slice(5).replace(/^ /, ""); data.push(item); dataBytes += item.length; }
          else if (line.startsWith("event:")) eventType = line.slice(6).trim();
          else if (line.startsWith("id:")) eventCursor = line.slice(3).trim();
          if (dataBytes > 1024 * 1024) throw new Error("Native SSE event exceeds its bound");
        }
      }
    } finally { connection.abort(); if (reader) { await reader.cancel().catch(() => {}); reader.releaseLock(); } }
  }
}
