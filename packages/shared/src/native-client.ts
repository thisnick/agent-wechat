/** Native v1 HTTP transport. This does not start a daemon or use the legacy API. */
import type { LoginRequest, QrRefresh, NativeTombstone, NativeMessage, NativeThread, NativeContact, NativeGroup, NativeGroupMember, NativePage, NativeOperation, NativeSendIntent, NativeEvent, NativeIdentity, NativeStatus, MediaAsset, MediaJob, MediaTransferRequest, MediaSendRequest, OwnedMediaSource } from "./native.generated.js";
export type * from "./native.generated.js";
export { NATIVE_MAX_SOURCE_BYTES } from "./native.generated.ts";
import { NATIVE_MAX_SOURCE_BYTES } from "./native.generated.ts";
export type NativeListOptions = { cursor?: string; limit?: number; query?: string; kind?: string };
export function isNativeTombstone(value: unknown): value is NativeTombstone { const item = value as Partial<NativeTombstone> | null; return item?.deleted === true && typeof item.id === "string" && typeof item.version === "string"; }
function listQuery(input?: string | NativeListOptions): string { const options = typeof input === "string" ? { cursor: input } : input; if (!options) return ""; if (options.limit != null && (!Number.isInteger(options.limit) || options.limit < 1 || options.limit > 200)) throw new Error("Invalid native page limit"); const query = new URLSearchParams(); for (const [key, value] of Object.entries(options)) if (value != null) query.set(key, String(value)); return query.size ? `?${query}` : ""; }
export type NativeLoginOptions = Partial<Omit<LoginRequest, "qrRefresh">> & { qrRefresh?: Partial<QrRefresh> };
export type NativeClientOptions = { baseUrl: string; token: string; fetch?: typeof fetch; timeoutMs?: number };
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
  readonly baseUrl: URL;
  private readonly token: string;
  private readonly fetchImpl: typeof fetch;
  private readonly timeoutMs: number;
  constructor(options: NativeClientOptions) {
    this.baseUrl = validateNativeBaseUrl(options.baseUrl);
    const nodeEnvironment = (globalThis as typeof globalThis & { process?: { env?: Record<string, string | undefined> } }).process?.env;
    if (this.baseUrl.protocol === "https:" && nodeEnvironment?.NODE_TLS_REJECT_UNAUTHORIZED === "0") throw new Error("Native API refuses HTTPS with TLS verification disabled");
    if (typeof options.token !== "string" || !options.token || /[\r\n]/.test(options.token)) throw new Error("Native API bearer token is required");
    this.token = options.token; this.fetchImpl = options.fetch ?? fetch; this.timeoutMs = options.timeoutMs ?? 30000;
  }
  private async response(path: string, init: RequestInit = {}, stream = false): Promise<Response> {
    const headers = new Headers(init.headers); headers.set("Authorization", `Bearer ${this.token}`);
    if (init.body != null && !(init.body instanceof FormData)) headers.set("Content-Type", "application/json");
    const timeout = stream ? undefined : AbortSignal.timeout(this.timeoutMs);
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
  updateAccount(accountId: string, options: { alias?: string; autoConnect?: boolean }): Promise<Record<string, unknown>> { return this.request(`v1/accounts/${id(accountId)}`, { method: "PATCH", body: JSON.stringify(options) }); }
  capabilities(accountId: string): Promise<Record<string, unknown>> { return this.request(`v1/accounts/${id(accountId)}/capabilities`); }
  connect(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/connect`, { method: "POST", body: "{}" }); }
  sync(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/sync`, { method: "POST", body: "{}" }); }
  login(accountId: string, options: NativeLoginOptions = {}, key?: string): Promise<NativeOperation> {
    if (key != null && (!key || key.length > 200 || /[\r\n]/.test(key))) throw new Error("Invalid login idempotency key");
    return this.request(`v1/accounts/${id(accountId)}/auth/login`, { method: "POST", headers: key ? { "Idempotency-Key": key } : undefined, body: JSON.stringify(options) });
  }
  operation(accountId: string, operationId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/operations/${id(operationId)}`); }
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
  thread(accountId: string, threadId: string): Promise<NativeThread> { return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}`); }
  contact(accountId: string, username: string): Promise<NativeContact> { return this.request(`v1/accounts/${id(accountId)}/contacts/${id(username)}`); }
  group(accountId: string, roomId: string): Promise<NativeGroup> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}`); }
  groupMembers(accountId: string, roomId: string, options?: string | NativeListOptions): Promise<NativePage<NativeGroupMember>> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/members${listQuery(options)}`); }
  refreshAvatar(accountId: string, username: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/avatars/${id(username)}/refresh`, { method: "POST", body: "{}" }); }
  async avatar(accountId: string, username: string, signal?: AbortSignal): Promise<{ bytes: Uint8Array; contentType: string; etag: string | null }> { const response = await this.response(`v1/accounts/${id(accountId)}/avatars/${id(username)}`, { signal }); const type = response.headers.get("content-type") ?? ""; if (!type.startsWith("image/")) { await response.body?.cancel(); throw new Error("Cached avatar is not an image"); } return { bytes: await this.readBounded(response, 16 * 1024 * 1024), contentType: type, etag: response.headers.get("etag") }; }
  refreshContact(accountId: string, username: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/contacts/${id(username)}/refresh`, { method: "POST", body: "{}" }); }
  refreshGroup(accountId: string, roomId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/refresh`, { method: "POST", body: "{}" }); }
  refreshGroupMembers(accountId: string, roomId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/groups/${id(roomId)}/members/refresh`, { method: "POST", body: "{}" }); }
  syncContacts(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/contact-sync`, { method: "POST", body: "{}" }); }
  syncGroups(accountId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/group-sync`, { method: "POST", body: "{}" }); }
  async messages(accountId: string, threadId: string, options?: string | NativeListOptions): Promise<NativePage<NativeMessage>> {
    const result = await this.request<NativePage<NativeMessage>>(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/messages${listQuery(options)}`);
    for (const msg of result.items) {
      if (isNativeTombstone(msg)) continue;
      if (typeof msg.id !== "string" || msg.accountId !== accountId || msg.threadId !== threadId
        || (msg.serverId != null && (typeof msg.serverId !== "string" || !/^\d+$/.test(msg.serverId)))) throw new Error("Invalid native message identity");
    }
    return result;
  }
  async message(accountId: string, messageId: string): Promise<NativeMessage> {
    const message = await this.request<NativeMessage>(`v1/accounts/${id(accountId)}/messages/${id(messageId)}`);
    if (message.id !== messageId || message.accountId !== accountId || (message.serverId != null && (typeof message.serverId !== "string" || !/^\d+$/.test(message.serverId)))) throw new Error("Invalid native message identity");
    return message;
  }
  sendText(accountId: string, threadId: string, text: string, idempotencyKey: string): Promise<NativeSendIntent> {
    if (!idempotencyKey || /[\r\n]/.test(idempotencyKey)) throw new Error("An idempotency key is required");
    return this.request(`v1/accounts/${id(accountId)}/sends`, { method: "POST", headers: { "Idempotency-Key": idempotencyKey }, body: JSON.stringify({ threadId, text }) });
  }
  send(accountId: string, sendId: string, signal?: AbortSignal): Promise<NativeSendIntent> { return this.request(`v1/accounts/${id(accountId)}/sends/${id(sendId)}`, { signal }); }
  sendByKey(accountId: string, key: string): Promise<NativeSendIntent | MediaJob> { if (!key || key.length > 200) throw new Error("Invalid idempotency key"); return this.request(`v1/accounts/${id(accountId)}/sends/by-idempotency-key?key=${encodeURIComponent(key)}`); }
  cancelSend(accountId: string, sendId: string): Promise<NativeSendIntent | MediaJob> { return this.request(`v1/accounts/${id(accountId)}/sends/${id(sendId)}/cancel`, { method: "POST", body: "{}" }); }
  async upload(accountId: string, bytes: Uint8Array, name: string, contentType = "application/octet-stream", signal?: AbortSignal): Promise<OwnedMediaSource> {
    if (!bytes.length || bytes.length > NATIVE_MAX_SOURCE_BYTES || !name || name.length > 255 || /[\r\n/\\]/.test(name)) throw new Error("Invalid bounded media upload");
    const form = new FormData(); form.set("file", new Blob([new Uint8Array(bytes).buffer], { type: contentType }), name);
    return this.request(`v1/accounts/${id(accountId)}/uploads`, { method: "POST", body: form, signal });
  }
  sendMedia(accountId: string, input: MediaSendRequest, key: string): Promise<MediaJob> { if (!key || /[\r\n]/.test(key)) throw new Error("An idempotency key is required"); return this.request(`v1/accounts/${id(accountId)}/sends`, { method: "POST", headers: { "Idempotency-Key": key }, body: JSON.stringify(input) }); }
  mediaAssets(accountId: string, messageId: string): Promise<{ items: MediaAsset[] }> { return this.request(`v1/accounts/${id(accountId)}/messages/${id(messageId)}/media`); }
  transferMedia(accountId: string, input: MediaTransferRequest): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/messages/${id(input.messageId)}/media-transfers`, { method: "POST", body: JSON.stringify(input) }); }
  mediaAsset(accountId: string, assetId: string): Promise<MediaAsset> { return this.request(`v1/accounts/${id(accountId)}/media/${id(assetId)}`); }
  job(accountId: string, jobId: string, signal?: AbortSignal): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}`, { signal }); }
  cancelJob(accountId: string, jobId: string): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}/cancel`, { method: "POST", body: "{}" }); }
  retryJob(accountId: string, jobId: string): Promise<MediaJob> { return this.request(`v1/accounts/${id(accountId)}/jobs/${id(jobId)}/retry`, { method: "POST", body: "{}" }); }
  repairMediaWriteback(accountId: string, limit = 25): Promise<NativeOperation> {
    if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new Error("Invalid media writeback limit");
    return this.request(`v1/accounts/${id(accountId)}/media-writeback/repair`, { method: "POST", body: JSON.stringify({ limit }) });
  }
  async mediaContent(accountId: string, assetId: string, options: { maxBytes?: number; range?: string; signal?: AbortSignal } = {}): Promise<{ bytes: Uint8Array; contentType: string; etag: string | null; contentRange: string | null; status: number }> {
    const max = options.maxBytes ?? 128 * 1024 * 1024;
    if (!Number.isInteger(max) || max < 1 || max > 128 * 1024 * 1024 || (options.range && !/^bytes=(?:\d+-\d*|-\d+)$/.test(options.range))) throw new Error("Invalid bounded content request");
    const response = await this.response(`v1/accounts/${id(accountId)}/media/${id(assetId)}/content`, { signal: options.signal, headers: options.range ? { Range: options.range } : {} });
    const length = response.headers.get("content-length"); if (length && (!/^\d+$/.test(length) || Number(length) > max)) { await response.body?.cancel(); throw new Error("Native media response exceeds its bound"); }
    return { bytes: await this.readBounded(response, max), contentType: response.headers.get("content-type") ?? "application/octet-stream", etag: response.headers.get("etag"), contentRange: response.headers.get("content-range"), status: response.status };
  }
  markRead(accountId: string, threadId: string, messageIds: string[], expectedVersion: string): Promise<unknown> {
    if (!messageIds.length || messageIds.length > 200 || messageIds.some(value => typeof value !== "string" || !value) || typeof expectedVersion !== "string") throw new Error("Invalid exact-ID read projection");
    return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/read-state`, { method: "POST", body: JSON.stringify({ messageIds: [...new Set(messageIds)], expectedVersion }) });
  }
  markReadThrough(accountId: string, threadId: string, throughMessageId: string, expectedVersion: string): Promise<unknown> { id(throughMessageId); if (typeof expectedVersion !== "string" || !expectedVersion) throw new Error("Read version is required"); return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/read-state`, { method: "POST", body: JSON.stringify({ throughMessageId, expectedVersion }) }); }
  events(accountId: string, after = "0", limit = 100): Promise<{ items: NativeEvent[]; nextCursor: string }> {
    if (!Number.isInteger(limit) || limit < 1 || limit > 200) throw new Error("Invalid event page limit");
    return this.request(`v1/accounts/${id(accountId)}/events?after=${decimal(after)}&limit=${limit}`);
  }
  ack(accountId: string, consumerId: string, cursor: string): Promise<unknown> {
    return this.request(`v1/accounts/${id(accountId)}/consumers/${id(consumerId)}/ack`, { method: "POST", body: JSON.stringify({ cursor: decimal(cursor) }) });
  }
  async *watch(accountId: string, after = "0", signal?: AbortSignal): AsyncGenerator<NativeEvent> {
    const response = await this.response(`v1/accounts/${id(accountId)}/events/stream?after=${decimal(after)}`, { signal, headers: { Accept: "text/event-stream" } }, true);
    if (!response.headers.get("content-type")?.startsWith("text/event-stream")) throw new Error("Expected native SSE response");
    const reader = response.body?.getReader(); if (!reader) throw new Error("Native SSE has no body");
    const decoder = new TextDecoder(); let buffer = ""; let data: string[] = []; let dataBytes = 0; let eventCursor = ""; let eventType = "";
    try {
      while (true) {
        const { value, done } = await reader.read(); if (done) break;
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
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
  }
}
