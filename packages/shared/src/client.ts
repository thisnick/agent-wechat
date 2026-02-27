import type {
  Chat,
  Contact,
  Message,
  SendResult,
  MediaResult,
  LoginResult,
  LoginSubscriptionEvent,
  OpenChatResult,
  Session,
  SendParams,
} from "./types/index.js";

// Re-export Status/LoginState types used by the client
export type LoginState = { status: string };
export type StatusResponse = {
  container: string;
  loginState: LoginState;
  version: string;
};
export type AuthStatus = {
  status: "logged_in" | "logged_out" | "app_not_running" | "unknown";
  loggedInUser?: string;
};

export interface WeChatClientOptions {
  baseUrl: string;
  token?: string;
  sessionId?: string;
  headers?: Record<string, string>;
}

function normalizeUrl(base: string): string {
  const url = base.startsWith("http") ? base : `http://${base}`;
  return url.replace(/\/$/, "");
}

function qs(params: Record<string, unknown>): string {
  const entries = Object.entries(params).filter(([, v]) => v != null);
  if (entries.length === 0) return "";
  return (
    "?" +
    entries
      .map(
        ([k, v]) =>
          `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`,
      )
      .join("&")
  );
}

export class WeChatClient {
  private base: string;
  private headers: Record<string, string>;

  constructor(options: WeChatClientOptions) {
    this.base = normalizeUrl(options.baseUrl);
    this.headers = { "Content-Type": "application/json" };
    if (options.token) this.headers.Authorization = `Bearer ${options.token}`;
    if (options.sessionId)
      this.headers["X-Session-Id"] = options.sessionId;
    if (options.headers) Object.assign(this.headers, options.headers);
  }

  /** Get the base URL (for WebSocket URL derivation, etc.) */
  get baseUrl(): string {
    return this.base;
  }

  // ---- internal helpers ----

  private async get<T>(path: string): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      headers: this.headers,
    });
    if (!res.ok)
      throw new Error(
        `${res.status} ${res.statusText}: ${await res.text()}`,
      );
    return res.json() as Promise<T>;
  }

  private async post<T>(path: string, body?: unknown): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method: "POST",
      headers: this.headers,
      body: body != null ? JSON.stringify(body) : undefined,
    });
    if (!res.ok)
      throw new Error(
        `${res.status} ${res.statusText}: ${await res.text()}`,
      );
    return res.json() as Promise<T>;
  }

  private async del<T>(path: string): Promise<T> {
    const res = await fetch(`${this.base}${path}`, {
      method: "DELETE",
      headers: this.headers,
    });
    if (!res.ok)
      throw new Error(
        `${res.status} ${res.statusText}: ${await res.text()}`,
      );
    return res.json() as Promise<T>;
  }

  // ---- Status ----

  async status(): Promise<StatusResponse> {
    return this.get("/api/status");
  }

  async loginState(): Promise<LoginState> {
    const s = await this.status();
    return s.loginState;
  }

  async authStatus(): Promise<AuthStatus> {
    return this.get("/api/status/auth");
  }

  async login(): Promise<LoginResult> {
    return this.post("/api/status/login");
  }

  async logout(): Promise<{ success: boolean; error?: string }> {
    return this.post("/api/status/logout");
  }

  // ---- Chats ----

  async listChats(
    limit?: number,
    offset?: number,
  ): Promise<Chat[]> {
    return this.get(`/api/chats${qs({ limit, offset })}`);
  }

  async getChat(id: string): Promise<Chat | null> {
    return this.get(`/api/chats/${encodeURIComponent(id)}`);
  }

  async findChats(name: string): Promise<Chat[]> {
    return this.get(`/api/chats/find${qs({ name })}`);
  }

  async openChat(
    chatId: string,
    clearUnreads?: boolean,
  ): Promise<OpenChatResult> {
    return this.post(
      `/api/chats/${encodeURIComponent(chatId)}/open${qs({ clearUnreads })}`,
    );
  }

  // ---- Contacts ----

  async listContacts(
    limit?: number,
    offset?: number,
  ): Promise<Contact[]> {
    return this.get(`/api/contacts${qs({ limit, offset })}`);
  }

  async findContacts(name: string): Promise<Contact[]> {
    return this.get(`/api/contacts/find${qs({ name })}`);
  }

  // ---- Messages ----

  async listMessages(
    chatId: string,
    limit?: number,
    offset?: number,
  ): Promise<Message[]> {
    return this.get(
      `/api/messages/${encodeURIComponent(chatId)}${qs({ limit, offset })}`,
    );
  }

  async getMedia(
    chatId: string,
    localId: number,
  ): Promise<MediaResult> {
    return this.get(
      `/api/messages/${encodeURIComponent(chatId)}/media/${localId}`,
    );
  }

  async sendMessage(params: SendParams): Promise<SendResult> {
    return this.post("/api/messages/send", params);
  }

  // ---- Debug ----

  async screenshot(): Promise<{ base64: string }> {
    return this.get("/api/debug/screenshot");
  }

  async a11y(
    format: "json" | "aria",
  ): Promise<{ tree: unknown; aria: string | null; error?: string }> {
    return this.get(`/api/debug/a11y${qs({ format })}`);
  }

  // ---- Sessions ----

  async createSession(name: string): Promise<Session> {
    return this.post("/api/sessions", { name });
  }

  async listSessions(): Promise<Session[]> {
    return this.get("/api/sessions");
  }

  async getSession(id: string): Promise<Session | null> {
    return this.get(`/api/sessions/${encodeURIComponent(id)}`);
  }

  async startSession(id: string): Promise<Session> {
    return this.post(
      `/api/sessions/${encodeURIComponent(id)}/start`,
    );
  }

  async stopSession(id: string): Promise<Session> {
    return this.post(
      `/api/sessions/${encodeURIComponent(id)}/stop`,
    );
  }

  async deleteSession(
    id: string,
  ): Promise<{ success: boolean }> {
    return this.del(`/api/sessions/${encodeURIComponent(id)}`);
  }

  // ---- Login subscription (WebSocket) ----

  /**
   * Subscribe to login events via WebSocket.
   * Uses the native WebSocket API (Node 22+).
   * Returns a handle with close() to tear down the connection.
   */
  /** Extract the bearer token (if any) for use in WebSocket query params. */
  private get wsToken(): string | undefined {
    const auth = this.headers.Authorization;
    return auth?.replace(/^Bearer\s+/i, "");
  }

  loginSubscribe(opts: {
    timeoutMs?: number;
    newAccount?: boolean;
    onEvent: (event: LoginSubscriptionEvent) => void;
    onError?: (err: Error) => void;
    onClose?: () => void;
  }): { close: () => void } {
    const wsUrl = this.base.replace(/^http/, "ws");
    const params = qs({
      timeoutMs: opts.timeoutMs,
      newAccount: opts.newAccount,
      token: this.wsToken,
    });
    const ws = new WebSocket(`${wsUrl}/api/ws/login${params}`);

    ws.addEventListener("message", (event) => {
      try {
        const data =
          typeof event.data === "string"
            ? event.data
            : String(event.data);
        const parsed = JSON.parse(data) as LoginSubscriptionEvent;
        opts.onEvent(parsed);
      } catch (e) {
        opts.onError?.(e instanceof Error ? e : new Error(String(e)));
      }
    });

    ws.addEventListener("error", (event) => {
      const msg =
        "message" in event && typeof (event as any).message === "string"
          ? (event as any).message
          : "WebSocket error";
      opts.onError?.(new Error(msg));
    });

    ws.addEventListener("close", () => {
      opts.onClose?.();
    });

    return {
      close: () => {
        ws.close();
      },
    };
  }
}
