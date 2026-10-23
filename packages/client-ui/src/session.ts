import {
  NativeApiError,
  NativeWeChatClient,
  isNativeTombstone,
  type NativeContact,
  type NativeEvent,
  type NativeMessage,
  type NativePage,
  type NativeSendIntent,
  type NativeThread,
} from "@agent-wechat/shared";

export type Account = {
  id: string;
  alias?: string;
  ownUsername?: string | null;
  authenticated?: boolean;
  connection?: string;
};
export type PendingSend = {
  key: string;
  text: string;
  threadId: string;
  intent?: NativeSendIntent;
  uncertain?: boolean;
  error?: string;
};
export type SessionState = {
  account: string | null;
  accounts: Account[];
  threads: NativeThread[];
  contacts: NativeContact[];
  messages: NativeMessage[];
  sends: PendingSend[];
  selected: string | null;
  nextMessages: string | null;
  loading: boolean;
  connected: boolean;
  error: string | null;
  capabilities: Record<string, unknown>;
};
const initial = (): SessionState => ({
  account: null,
  accounts: [],
  threads: [],
  contacts: [],
  messages: [],
  sends: [],
  selected: null,
  nextMessages: null,
  loading: false,
  connected: false,
  error: null,
  capabilities: {},
});
export function readableError(error: unknown): string {
  if (error instanceof NativeApiError)
    return (
      (
        {
          unauthorized: "The agent-wechat hub access key wasn't accepted.",
          forbidden: "This access key doesn't allow that action.",
          auth_required: "Sign in to WeChat again.",
          operation_in_progress:
            "A sign-in is already running for this account. Finish it in the original tab, or try again after it expires.",
          read_state_conflict:
            "The conversation changed. Refresh it and try again.",
          idempotency_conflict: "This send key is already in use.",
          unsupported_simulator_operation:
            "This action isn't available on the simulator.",
          unsupported_group_creation:
            "Group creation isn't available on this agent-wechat hub yet.",
          invalid_group_creation:
            "Choose a group name and at least two distinct contacts.",
          pagination_cursor_expired:
            "This history view expired. Reopen the conversation.",
        } as Record<string, string>
      )[error.code] ?? `agent-wechat hub request failed (${error.code}).`
    );
  return "Couldn't reach the agent-wechat hub. Check the connection and try again.";
}
function timestamp(value: string | null): number {
  return value && /^\d+$/.test(value)
    ? Number(value)
    : Date.parse(value ?? "") || 0;
}
export function mergeMessage(
  messages: NativeMessage[],
  message: NativeMessage,
): NativeMessage[] {
  return [
    ...messages.filter(
      (item) =>
        item.id !== message.id &&
        !(
          message.serverId &&
          item.threadId === message.threadId &&
          item.serverId === message.serverId
        ),
    ),
    message,
  ].sort(
    (a, b) =>
      timestamp(a.createdAt) - timestamp(b.createdAt) ||
      a.id.localeCompare(b.id, undefined, { numeric: true }),
  );
}
export function pause(
  milliseconds: number,
  signal: AbortSignal,
): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    const done = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener("abort", done, { once: true });
  });
}
export async function inventory<T>(
  read: (cursor?: string) => Promise<NativePage<T>>,
): Promise<{ items: T[]; cursor: string }> {
  const items = new Map<string, T>();
  let next: string | undefined,
    boundary = "0";
  const seen = new Set<string>();
  for (let pageNumber = 0; pageNumber < 500; pageNumber++) {
    const page = await read(next);
    if (!next) boundary = page.asOfEventCursor ?? "0";
    for (const item of page.items) {
      if (isNativeTombstone(item)) items.delete(item.id);
      else items.set((item as { id: string }).id, item);
    }
    if (!page.nextCursor)
      return { items: [...items.values()], cursor: boundary };
    if (seen.has(page.nextCursor)) throw new Error("Repeated inventory cursor");
    seen.add(page.nextCursor);
    next = page.nextCursor;
  }
  throw new Error("Inventory exceeds the supported view size");
}

/** Each account activation and thread selection fences every late response. */
export class HubSession {
  private value = initial();
  private listeners = new Set<() => void>();
  private mediaListeners = new Set<(assetId: string) => void>();
  onMediaReady = (listener: (assetId: string) => void): (() => void) => {
    this.mediaListeners.add(listener);
    return () => {
      this.mediaListeners.delete(listener);
    };
  };
  private generation = 0;
  private threadGeneration = 0;
  private accountRequest = 0;
  private catalogRequest = 0;
  private stream?: AbortController;
  private cursor = "0";
  private pending = new Map<string, PendingSend[]>();
  private stopped = false;
  readonly client: NativeWeChatClient;
  private receipts?: {
    read(account: string): PendingSend[];
    write(account: string, sends: PendingSend[]): void;
  };
  constructor(
    client: NativeWeChatClient,
    receipts?: {
      read(account: string): PendingSend[];
      write(account: string, sends: PendingSend[]): void;
    },
  ) {
    this.client = client;
    this.receipts = receipts;
  }
  snapshot = (): SessionState => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private current(account: string, generation: number): boolean {
    return (
      !this.stopped &&
      generation === this.generation &&
      account === this.value.account
    );
  }
  private publish(patch: Partial<SessionState>): void {
    if (this.stopped) return;
    this.value = { ...this.value, ...patch };
    if (patch.sends && this.value.account) {
      this.pending.set(this.value.account, patch.sends);
      this.receipts?.write(this.value.account, patch.sends);
    }
    for (const listener of this.listeners) listener();
  }
  clearError(): void {
    this.publish({ error: null });
  }
  async accounts(): Promise<Account[]> {
    const request = ++this.accountRequest,
      generation = this.generation;
    const result = await this.client.accounts();
    const accounts = result.items.filter(
      (item) => typeof item.id === "string",
    ) as Account[];
    if (
      !this.stopped &&
      request === this.accountRequest &&
      generation === this.generation
    )
      this.publish({ accounts });
    return accounts;
  }
  close(): void {
    this.generation++;
    this.threadGeneration++;
    this.stream?.abort();
    this.publish(initial());
    this.stopped = true;
    this.mediaListeners.clear();
  }
  async activate(account: string): Promise<void> {
    this.stream?.abort();
    const generation = ++this.generation;
    this.threadGeneration++;
    this.catalogRequest++;
    this.cursor = "0";
    const sends =
      this.pending.get(account) ?? this.receipts?.read(account) ?? [];
    this.publish({
      account,
      threads: [],
      contacts: [],
      messages: [],
      sends,
      selected: null,
      nextMessages: null,
      loading: true,
      connected: false,
      error: null,
      capabilities: {},
    });
    const controller = (this.stream = new AbortController());
    try {
      const boundary = await this.refresh(account, generation);
      if (!this.current(account, generation)) return;
      this.cursor = boundary;
      if (this.value.threads[0]) await this.select(this.value.threads[0].id);
      else this.publish({ loading: false });
    } catch (error) {
      if (this.current(account, generation))
        this.publish({ error: readableError(error), loading: false });
    }
    if (this.current(account, generation)) {
      void this.watch(account, generation, controller.signal);
      for (const send of sends) void this.checkSend(send.key);
    }
  }
  async select(threadId: string): Promise<void> {
    const account = this.value.account;
    if (!account) return;
    const generation = this.generation,
      selected = ++this.threadGeneration;
    this.publish({
      selected: threadId,
      messages: [],
      nextMessages: null,
      loading: true,
      error: null,
    });
    try {
      const page = await this.client.messages(account, threadId, {
        limit: 100,
      });
      if (
        !this.current(account, generation) ||
        selected !== this.threadGeneration
      )
        return;
      const messages = this.value.messages.reduce(
        mergeMessage,
        (
          page.items.filter(
            (item) => !isNativeTombstone(item),
          ) as NativeMessage[]
        ).reduce(mergeMessage, []),
      );
      this.publish({ messages, nextMessages: page.nextCursor, loading: false });
    } catch (error) {
      if (
        this.current(account, generation) &&
        selected === this.threadGeneration
      )
        this.publish({ error: readableError(error), loading: false });
    }
  }
  async older(): Promise<void> {
    const { account, selected, nextMessages } = this.value,
      generation = this.generation,
      threadGeneration = this.threadGeneration;
    if (!account || !selected || !nextMessages || this.value.loading) return;
    this.publish({ loading: true });
    try {
      const page = await this.client.messages(account, selected, {
        cursor: nextMessages,
        limit: 100,
      });
      if (
        !this.current(account, generation) ||
        threadGeneration !== this.threadGeneration
      )
        return;
      const messages = this.value.messages.reduce(
        mergeMessage,
        (
          page.items.filter(
            (item) => !isNativeTombstone(item),
          ) as NativeMessage[]
        ).reduce(mergeMessage, []),
      );
      this.publish({ messages, nextMessages: page.nextCursor, loading: false });
    } catch (error) {
      if (
        this.current(account, generation) &&
        threadGeneration === this.threadGeneration
      )
        this.publish({ loading: false, error: readableError(error) });
    }
  }
  async markRead(threadId: string): Promise<unknown> {
    const account = this.value.account,
      generation = this.generation;
    const thread = this.value.threads.find((thread) => thread.id === threadId);
    if (!account || !thread) throw new Error("Select a conversation first.");
    const result = await this.client.markRead(
      account,
      threadId,
      thread.readStateVersion,
    );
    if (this.current(account, generation))
      await this.refresh(account, generation);
    return result;
  }
  async refresh(
    account = this.value.account,
    generation = this.generation,
  ): Promise<string> {
    if (!account) return "0";
    const request = ++this.catalogRequest;
    const [threads, contacts, capabilities, accounts] = await Promise.all([
      inventory((cursor) =>
        this.client.threads(account, { cursor, limit: 200 }),
      ),
      inventory((cursor) =>
        this.client.contacts(account, { cursor, limit: 200 }),
      ),
      this.client.capabilities(account),
      this.client.accounts(),
    ]);
    if (this.current(account, generation) && request === this.catalogRequest)
      this.publish({
        threads: threads.items,
        contacts: contacts.items,
        capabilities,
        accounts: accounts.items.filter(
          (item) => typeof item.id === "string",
        ) as Account[],
      });
    return threads.cursor;
  }
  private async canonical(
    account: string,
    messageId: string,
    generation: number,
  ): Promise<void> {
    const message = await this.client.message(account, messageId);
    if (!this.current(account, generation)) return;
    if (message.threadId === this.value.selected)
      this.publish({ messages: mergeMessage(this.value.messages, message) });
    this.publish({
      sends: this.value.sends.filter(
        (send) =>
          send.intent?.localMessageId !== message.id &&
          !(message.serverId && send.intent?.serverId === message.serverId),
      ),
    });
  }
  private async acceptReceipt(
    intent: NativeSendIntent,
    account: string,
    generation: number,
    key?: string,
  ): Promise<void> {
    if (!this.current(account, generation) || intent.accountId !== account)
      return;
    let sends = this.value.sends;
    if (key && sends.some((send) => send.key === key))
      sends = sends.filter(
        (send) => send.key === key || send.intent?.id !== intent.id,
      );
    const known = sends.some(
      (send) => send.key === key || send.intent?.id === intent.id,
    );
    if (!known && !key && !intent.localMessageId)
      sends = [
        ...sends,
        {
          key: `observed:${intent.id}`,
          text: "Message from another client",
          threadId: intent.destination,
          intent,
        },
      ];
    this.publish({
      sends: sends.map((send) =>
        send.key === key || send.intent?.id === intent.id
          ? { ...send, intent, uncertain: false, error: undefined }
          : send,
      ),
    });
    if (intent.localMessageId) {
      await this.canonical(account, intent.localMessageId, generation);
      if (this.current(account, generation))
        this.publish({
          sends: this.value.sends.filter(
            (send) => send.key !== key && send.intent?.id !== intent.id,
          ),
        });
    }
  }
  async consume(
    event: NativeEvent,
    account: string,
    generation = this.generation,
  ): Promise<void> {
    if (!this.current(account, generation)) return;
    if (
      event.kind === "media.ready" &&
      typeof event.data.assetId === "string"
    ) {
      for (const listener of this.mediaListeners) listener(event.data.assetId);
    }
    if (
      (event.kind === "message.committed" ||
        event.kind === "message.recalled") &&
      typeof event.data.messageId === "string"
    ) {
      await this.canonical(account, event.data.messageId, generation);
      if (this.current(account, generation))
        await this.refresh(account, generation);
    } else if (
      event.kind === "send.updated" &&
      typeof event.data.id === "string"
    ) {
      await this.acceptReceipt(
        await this.client.send(account, event.data.id),
        account,
        generation,
      );
    } else if (
      [
        "account.updated",
        "account.connection",
        "auth.updated",
        "sync.completed",
        "thread.read",
        "thread.read-state",
        "metadata.changed",
        "contacts.updated",
      ].includes(event.kind)
    ) {
      await this.refresh(account, generation);
    }
    // Push hints and control diagnostics carry no conversation content.
  }
  private async recover(account: string, generation: number): Promise<void> {
    const boundary = await this.refresh(account, generation);
    if (!this.current(account, generation)) return;
    const selected = this.value.selected;
    if (selected) await this.select(selected);
    if (this.current(account, generation)) this.cursor = boundary;
  }
  private async watch(
    account: string,
    generation: number,
    signal: AbortSignal,
  ): Promise<void> {
    while (!signal.aborted && this.current(account, generation)) {
      try {
        for await (const event of this.client.watch(
          account,
          this.cursor,
          signal,
          {
            onActivity: () => {
              if (
                this.current(account, generation) &&
                !signal.aborted &&
                !this.value.connected
              )
                this.publish({ connected: true, error: null });
            },
          },
        )) {
          if (signal.aborted || !this.current(account, generation)) return;
          await this.consume(event, account, generation);
          if (this.current(account, generation)) this.cursor = event.cursor;
        }
        if (!signal.aborted && this.current(account, generation))
          this.publish({ connected: false });
      } catch (error) {
        if (signal.aborted || !this.current(account, generation)) return;
        this.publish({ connected: false, error: readableError(error) });
        if (
          error instanceof NativeApiError &&
          ["cursor_expired", "event_cursor_expired"].includes(error.code)
        ) {
          try {
            await this.recover(account, generation);
          } catch {
            /* Retry recovery on the next connection. */
          }
        }
      }
      await pause(1500, signal);
    }
  }
  async send(text: string): Promise<void> {
    const account = this.value.account,
      threadId = this.value.selected,
      generation = this.generation;
    if (!account || !threadId || !text.trim()) return;
    const key = crypto.randomUUID();
    this.publish({
      sends: [...this.value.sends, { key, text, threadId }],
      error: null,
    });
    try {
      const intent = await this.client.sendText(account, threadId, text, key);
      if (this.current(account, generation)) {
        await this.acceptReceipt(intent, account, generation, key);
        // Re-read after admission: send.updated can beat the POST response.
        await this.acceptReceipt(
          await this.client.send(account, intent.id),
          account,
          generation,
          key,
        );
      } else {
        const sends = (this.pending.get(account) ?? []).map((send) =>
          send.key === key ? { ...send, intent } : send,
        );
        this.pending.set(account, sends);
        this.receipts?.write(account, sends);
      }
    } catch (error) {
      const update = (sends: PendingSend[]) =>
        sends.map((send) =>
          send.key === key
            ? { ...send, uncertain: true, error: readableError(error) }
            : send,
        );
      if (this.current(account, generation))
        this.publish({ sends: update(this.value.sends) });
      else {
        const sends = update(this.pending.get(account) ?? []);
        this.pending.set(account, sends);
        this.receipts?.write(account, sends);
      }
    }
  }
  async checkSend(key: string): Promise<void> {
    const account = this.value.account,
      generation = this.generation;
    if (!account) return;
    try {
      const known = this.value.sends.find((send) => send.key === key)?.intent;
      const receipt = known
        ? await this.client.send(account, known.id)
        : await this.client.sendByKey(account, key);
      if ("clientId" in receipt)
        await this.acceptReceipt(receipt, account, generation, key);
    } catch (error) {
      if (this.current(account, generation))
        this.publish({
          sends: this.value.sends.map((send) =>
            send.key === key
              ? { ...send, uncertain: true, error: readableError(error) }
              : send,
          ),
        });
    }
  }
}
