import {
  NativeApiError,
  NativeWeChatClient,
  isNativeTombstone,
  type NativeContact,
  type NativeGroup,
  type NativeEvent,
  type NativeMessage,
  type NativePage,
  type NativeSendIntent,
  type NativeThread,
} from "@agent-wechat/shared";
import { HistoryCache } from "./history-cache.ts";
import { messageSummary } from "./thread-preview.ts";

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
  selectedGroup: NativeGroup | null;
  nextMessages: string | null;
  loading: boolean;
  opening: boolean;
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
  selectedGroup: null,
  nextMessages: null,
  loading: false,
  opening: false,
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
          unsupported_application_media:
            "This message doesn't contain a supported downloadable attachment.",
          unsupported_group_creation:
            "Group creation isn't available on this agent-wechat hub yet.",
          invalid_group_members:
            "Choose distinct contacts; group creation needs at least two.",
          ordinary_contact_required:
            "One of these contacts cannot be added to an ordinary group.",
          ordinary_group_required:
            "Refresh this group and check that you are still a member.",
          group_state_incomplete:
            "Refresh the group before changing its settings.",
          operation_cannot_cancel:
            "This change has started and can no longer be cancelled.",
          invalid_group_text:
            "Use a name of at most 512 UTF-8 bytes without control characters.",
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
const compareMessages = (a: NativeMessage, b: NativeMessage): number =>
  timestamp(a.createdAt) - timestamp(b.createdAt) ||
  a.id.localeCompare(b.id, undefined, { numeric: true });
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
  ].sort(compareMessages);
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
  private detailsRequest = 0;
  private threadRequests = new Map<string, number>();
  private catalogFlight: {
    account: string;
    generation: number;
    promise: Promise<void>;
  } | null = null;
  private stream?: AbortController;
  private cursor = "0";
  private pending = new Map<string, PendingSend[]>();
  private histories = new HistoryCache();
  private historyLoaded = false;
  private historySource = "";
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
  private remember(): void {
    const { account, selected, messages, nextMessages, selectedGroup } =
      this.value;
    if (this.historyLoaded && account && selected)
      this.histories.set(account, selected, {
        messages,
        nextMessages,
        group: selectedGroup,
        sourceGeneration: this.historySource,
      });
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
    this.histories.clear();
  }
  async activate(account: string): Promise<void> {
    this.remember();
    this.historyLoaded = false;
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
      selectedGroup: null,
      nextMessages: null,
      loading: true,
      opening: false,
      connected: false,
      error: null,
      capabilities: {},
    });
    const controller = (this.stream = new AbortController());
    try {
      const boundary = await this.refresh(account, generation, false);
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
      void this.loadCatalog(account, generation).catch(() => {});
      for (const send of sends) void this.checkSend(send.key);
    }
  }
  async select(threadId: string): Promise<void> {
    const account = this.value.account;
    if (!account) return;
    this.remember();
    const cached = this.histories.get(account, threadId);
    this.historyLoaded = !!cached;
    this.historySource = cached?.sourceGeneration ?? "";
    const previousGroup =
      this.value.selected === threadId
        ? this.value.selectedGroup
        : (cached?.group ?? null);
    const generation = this.generation,
      selected = ++this.threadGeneration;
    const current = () =>
      this.current(account, generation) && selected === this.threadGeneration;
    this.publish({
      selected: threadId,
      selectedGroup: previousGroup,
      messages: cached?.messages ?? [],
      nextMessages: cached?.nextMessages ?? null,
      loading: true,
      opening: true,
      error: null,
    });
    if (threadId.endsWith("@chatroom"))
      void this.refreshGroup(account, threadId, generation, selected).catch(
        () => {},
      );
    try {
      for (let attempt = 0; attempt < 2; attempt++) {
        const baseline =
          attempt === 0 ? (cached?.messages ?? []) : this.value.messages;
        // A list entry can be minutes old. Fetch the version for this open.
        const readBoundary = await this.refreshThread(
          account,
          threadId,
          generation,
        );
        if (!current()) return;
        if (
          attempt === 0 &&
          cached &&
          this.value.connected &&
          readBoundary.unreadCount === 0
        ) {
          this.publish({ loading: false });
          return; // The continuous event stream already maintains this cache.
        }
        const page = await this.client.messages(account, threadId, {
          limit: 40,
        });
        if (!current()) return;
        const latest = (
          page.items.filter(
            (item) => !isNativeTombstone(item),
          ) as NativeMessage[]
        ).reduce(mergeMessage, []);
        // Preserve older loaded pages, replace the current server range, and apply
        // only events received after this request on top of its snapshot.
        const older =
          page.nextCursor && latest.length
            ? baseline.filter((item) => compareMessages(item, latest[0]) < 0)
            : [];
        const arrivals = this.value.messages.filter(
          (item) => !baseline.includes(item),
        );
        const removed = new Set(
          page.items.filter(isNativeTombstone).map((item) => item.id),
        );
        const messages = arrivals
          .reduce(mergeMessage, latest.reduce(mergeMessage, older))
          .filter((item) => !removed.has(item.id));
        const sameSource = this.historySource === page.sourceGeneration;
        this.historyLoaded = true;
        this.historySource = page.sourceGeneration;
        this.publish({
          messages,

          nextMessages:
            page.nextCursor && sameSource
              ? this.value.nextMessages
              : page.nextCursor,
          loading: false,
        });
        this.remember();
        if ((readBoundary.unreadCount ?? 0) <= 0) return;
        try {
          await this.client.markRead(
            account,
            threadId,
            readBoundary.readStateVersion,
          );
          if (this.current(account, generation))
            await this.refreshThread(account, threadId, generation);
          return;
        } catch (error) {
          if (!current()) return;
          // A conflict admits no read. Reload both version and history once;
          // never clear unseen messages by retrying only with a newer version.
          if (
            error instanceof NativeApiError &&
            error.code === "read_state_conflict" &&
            attempt === 0
          )
            continue;
          this.publish({
            error: `Couldn’t clear unread status. ${readableError(error)}`,
          });
          return;
        }
      }
    } catch (error) {
      if (current())
        this.publish({ error: readableError(error), loading: false });
    } finally {
      if (current()) this.publish({ opening: false });
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
      this.historySource = page.sourceGeneration;
      this.remember();
    } catch (error) {
      if (
        this.current(account, generation) &&
        threadGeneration === this.threadGeneration
      )
        this.publish({ loading: false, error: readableError(error) });
    }
  }
  private async refreshThread(
    account: string,
    threadId: string,
    generation: number,
  ): Promise<NativeThread> {
    const key = JSON.stringify([account, threadId]);
    const request = (this.threadRequests.get(key) ?? 0) + 1;
    this.threadRequests.set(key, request);
    let thread: NativeThread;
    try {
      thread = await this.client.thread(account, threadId);
    } catch (error) {
      if (!(error instanceof NativeApiError) || error.status !== 404)
        throw error;
      // A new direct conversation has no session row until its first message.
      return {
        id: threadId,
        accountId: account,
        peerId: threadId,
        kind: threadId.endsWith("@chatroom") ? "group" : "direct",
        displayName: null,
        unreadCount: 0,
        readStateVersion: "",
        history: {},
      };
    }
    if (
      this.current(account, generation) &&
      this.threadRequests.get(key) === request
    ) {
      const found = this.value.threads.some((item) => item.id === threadId);
      this.publish({
        threads: found
          ? this.value.threads.map((item) =>
              item.id === threadId ? { ...item, ...thread } : item,
            )
          : [thread, ...this.value.threads],
      });
    }
    return thread;
  }
  private async refreshGroup(
    account: string,
    thread: string,
    generation: number,
    selected: number,
  ): Promise<void> {
    const group = await this.client.group(account, thread);
    if (
      this.current(account, generation) &&
      selected === this.threadGeneration &&
      thread === this.value.selected
    ) {
      this.publish({ selectedGroup: group });
      this.remember();
    }
  }
  async markRead(threadId: string): Promise<unknown> {
    const account = this.value.account,
      generation = this.generation;
    if (!account) throw new Error("Select a conversation first.");
    const thread = await this.refreshThread(account, threadId, generation);
    const result = await this.client.markRead(
      account,
      threadId,
      thread.readStateVersion,
    );
    if (this.current(account, generation))
      await this.refreshThread(account, threadId, generation);
    return result;
  }
  private loadCatalog(account: string, generation: number): Promise<void> {
    if (
      this.catalogFlight?.account === account &&
      this.catalogFlight.generation === generation
    )
      return this.catalogFlight.promise;
    const request = ++this.detailsRequest;
    const promise = (async () => {
      const [contacts, capabilities, accounts] = await Promise.all([
        inventory((cursor) =>
          this.client.contacts(account, { cursor, limit: 200 }),
        ),
        this.client.capabilities(account),
        this.client.accounts(),
      ]);
      if (this.current(account, generation) && request === this.detailsRequest)
        this.publish({
          contacts: contacts.items,
          capabilities,
          accounts: accounts.items.filter(
            (item) => typeof item.id === "string",
          ) as Account[],
        });
    })().finally(() => {
      if (this.catalogFlight?.promise === promise) this.catalogFlight = null;
    });
    this.catalogFlight = { account, generation, promise };
    return promise;
  }
  async refresh(
    account = this.value.account,
    generation = this.generation,
    catalog = true,
  ): Promise<string> {
    if (!account) return "0";
    const request = ++this.catalogRequest;
    const revisions = new Map(this.threadRequests);
    const threads = await inventory((cursor) =>
      this.client.threads(account, { cursor, limit: 200 }),
    );
    if (this.current(account, generation) && request === this.catalogRequest)
      this.publish({
        threads: threads.items.map((thread) => {
          const key = JSON.stringify([account, thread.id]);
          return revisions.get(key) !== this.threadRequests.get(key)
            ? (this.value.threads.find((item) => item.id === thread.id) ??
                thread)
            : thread;
        }),
      });
    if (catalog && this.current(account, generation)) {
      await Promise.all([
        this.loadCatalog(account, generation),
        this.value.selected?.endsWith("@chatroom")
          ? this.refreshGroup(
              account,
              this.value.selected,
              generation,
              this.threadGeneration,
            ).catch(() => {})
          : null,
      ]);
    }
    return threads.cursor;
  }
  private async canonical(
    account: string,
    messageId: string,
    generation: number,
  ): Promise<NativeMessage | undefined> {
    const message = await this.client.message(account, messageId);
    if (!this.current(account, generation)) return;
    if (message.threadId === this.value.selected) {
      this.publish({ messages: mergeMessage(this.value.messages, message) });
      this.remember();
    } else {
      const cached = this.histories.get(account, message.threadId);
      if (cached)
        this.histories.set(account, message.threadId, {
          ...cached,
          messages: mergeMessage(cached.messages, message),
        });
    }
    this.publish({
      sends: this.value.sends.filter(
        (send) =>
          send.intent?.localMessageId !== message.id &&
          !(message.serverId && send.intent?.serverId === message.serverId),
      ),
    });
    this.updatePreview(message);
    return message;
  }
  private updatePreview(message: NativeMessage): void {
    const senderName =
      this.value.contacts.find((item) => item.id === message.senderId)
        ?.displayName ?? null;
    this.publish({
      threads: this.value.threads.map((thread) => {
        if (thread.id !== message.threadId) return thread;
        const last = thread.lastMessage;
        if (
          last &&
          last.id !== message.id &&
          (timestamp(last.createdAt) > timestamp(message.createdAt) ||
            (timestamp(last.createdAt) === timestamp(message.createdAt) &&
              last.id.localeCompare(message.id, undefined, { numeric: true }) >=
                0))
        )
          return thread;
        return { ...thread, lastMessage: messageSummary(message, senderName) };
      }),
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
      const message = await this.canonical(
        account,
        event.data.messageId,
        generation,
      );
      if (message && this.current(account, generation)) {
        await this.refreshThread(account, message.threadId, generation);
        if (this.current(account, generation)) this.updatePreview(message);
      }
    } else if (
      event.kind === "send.updated" &&
      typeof event.data.id === "string"
    ) {
      await this.acceptReceipt(
        await this.client.send(account, event.data.id),
        account,
        generation,
      );
    } else if (["thread.read", "thread.read-state"].includes(event.kind)) {
      if (typeof event.data.threadId === "string")
        await this.refreshThread(account, event.data.threadId, generation);
      else await this.refresh(account, generation, false);
    } else if (event.kind === "group.updated") {
      if (typeof event.data.groupId === "string") {
        await this.refreshThread(account, event.data.groupId, generation).catch(
          () => this.refresh(account, generation, false),
        );
        if (event.data.groupId === this.value.selected)
          await this.refreshGroup(
            account,
            event.data.groupId,
            generation,
            this.threadGeneration,
          ).catch(() => {});
      } else await this.refresh(account, generation, false);
    } else if (
      ["account.updated", "account.connection", "auth.updated"].includes(
        event.kind,
      )
    ) {
      await this.accounts();
    } else if (
      event.kind === "contacts.updated" ||
      (event.kind === "metadata.changed" && event.data.kind !== "avatar")
    ) {
      void this.loadCatalog(account, generation).catch(() => {});
    } else if (event.kind === "sync.completed") {
      await this.refresh(account, generation, false);
    }
    // Push hints and control diagnostics carry no conversation content.
  }
  private async recover(account: string, generation: number): Promise<void> {
    const boundary = await this.refresh(account, generation);
    if (!this.current(account, generation)) return;
    this.histories.clear(account);
    this.historyLoaded = false;
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
    if (this.value.selectedGroup?.isInGroup === false) {
      this.publish({
        error:
          "You are no longer in this group. Cached history is still available.",
      });
      return;
    }
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
