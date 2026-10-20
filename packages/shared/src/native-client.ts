/** Native v1 HTTP transport. This does not start a daemon or use the legacy API. */
export type NativeMentions = { state: "known" | "unknown" | string; userIds: string[]; self: boolean | null; all: boolean | null; source: string | null };
export type NativeMessage = { id: string; accountId: string; threadId: string; serverId: string | null; senderId: string | null; type: number; direction: string | null; body: string | null; createdAt: string | null; mentions: NativeMentions; version: string; media: unknown[] };
export type NativeThread = { id: string; accountId: string; peerId: string; kind: string; displayName: string | null; unreadCount: number | null; readStateVersion: string; history: unknown };
export type NativePage<T> = { items: T[]; nextCursor: string | null; coverage: unknown; sourceGeneration: string };
export type NativeOperation = { id: string; accountId: string; kind: string; state: string; result: unknown; createdAt: string; updatedAt: string };
export type NativeSendIntent = { id: string; accountId: string; destination: string; state: string; clientId: string | null; serverId: string | null; errorCode: string | null; createdAt: string };
export type NativeEvent = { cursor: string; kind: string; data: Record<string, unknown>; createdAt: string };
export type NativeIdentity = { product: string; backend: string; version: string; apiVersion: number; target?: string; capabilities: string[] };
export type NativeStatus = { serverId: string; dataDir: string; pid: number; ownershipGeneration: string; version: NativeIdentity; state: string };
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
    if (init.body != null) headers.set("Content-Type", "application/json");
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
  createAccount(accountId: string): Promise<Record<string, unknown>> { return this.request("v1/accounts", { method: "POST", body: JSON.stringify({ id: accountId }) }); }
  login(accountId: string, options: Record<string, unknown> = {}): Promise<NativeOperation> {
    return this.request(`v1/accounts/${id(accountId)}/auth/login`, { method: "POST", body: JSON.stringify(options) });
  }
  operation(accountId: string, operationId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/operations/${id(operationId)}`); }
  cancelOperation(accountId: string, operationId: string): Promise<NativeOperation> { return this.request(`v1/accounts/${id(accountId)}/operations/${id(operationId)}`, { method: "DELETE" }); }
  async qr(accountId: string, operationId: string): Promise<Uint8Array> {
    const response = await this.response(`v1/accounts/${id(accountId)}/operations/${id(operationId)}/qr`);
    if (!response.headers.get("content-type")?.startsWith("image/png")) throw new Error("Native login QR response is not PNG");
    return this.readBounded(response, 1024 * 1024);
  }
  disconnect(accountId: string): Promise<unknown> { return this.request(`v1/accounts/${id(accountId)}/auth/disconnect`, { method: "POST", body: "{}" }); }
  logoutLocal(accountId: string): Promise<unknown> { return this.request(`v1/accounts/${id(accountId)}/auth/logout-local`, { method: "POST", body: "{}" }); }
  threads(accountId: string, cursor?: string): Promise<NativePage<NativeThread>> { return this.request(`v1/accounts/${id(accountId)}/threads${cursor ? `?cursor=${id(cursor)}` : ""}`); }
  contacts(accountId: string): Promise<NativePage<Record<string, unknown>>> { return this.request(`v1/accounts/${id(accountId)}/contacts`); }
  groups(accountId: string): Promise<NativePage<Record<string, unknown>>> { return this.request(`v1/accounts/${id(accountId)}/groups`); }
  async messages(accountId: string, threadId: string, cursor?: string): Promise<NativePage<NativeMessage>> {
    const result = await this.request<NativePage<NativeMessage>>(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/messages${cursor ? `?cursor=${id(cursor)}` : ""}`);
    for (const msg of result.items) {
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
  markRead(accountId: string, threadId: string, messageIds: string[], expectedVersion: string): Promise<unknown> {
    if (!messageIds.length || messageIds.length > 200 || messageIds.some(value => typeof value !== "string" || !value) || typeof expectedVersion !== "string") throw new Error("Invalid exact-ID read projection");
    return this.request(`v1/accounts/${id(accountId)}/threads/${id(threadId)}/read-state`, { method: "POST", body: JSON.stringify({ messageIds: [...new Set(messageIds)], expectedVersion }) });
  }
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
