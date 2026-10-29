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
  type QuoteSendReference,
  type NativeThreadMessage,
  type TextMentions,
} from "@agent-wechat/shared";
import { allMentionProblem, canMentionAll, mentionOptions, type MentionToken } from "./mentions.ts";
import type { BrowserCache } from "./browser-cache.ts";
import { HISTORY_TTL_MS, HistoryCache } from "./history-cache.ts";
import { messageSummary } from "./thread-preview.ts";
import { canReply } from "./reply.ts";
import { quoteTargetMatches } from "./quote-preview.ts";

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
  quote?: QuoteSendReference;
  quotePreview?: NativeThreadMessage;
  mentions?: TextMentions;
  intent?: NativeSendIntent;
  uncertain?: boolean;
  dismissed?: boolean;
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
  reconnecting: boolean;
  signInRequired: string | null;
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
  reconnecting: false,
  signInRequired: null,
  error: null,
  capabilities: {},
});
function isSignOut(event: NativeEvent): boolean {
  return (
    [
      "account.updated",
      "account.connection",
      "auth.updated",
      "account.push",
    ].includes(event.kind) &&
    (event.data.authentication === "logged_out" ||
      event.data.authenticated === false ||
      event.data.state === "auth_required" ||
      event.data.reason === "auth_required")
  );
}
export function readableError(error: unknown): string {
  if (error instanceof NativeApiError)
    return (
      (
        {
          unauthorized: "The agent-wechat hub access key wasn't accepted.",
          forbidden: "This access key doesn't allow that action.",
          auth_required: "Sign in to WeChat again.",
          operation_in_progress:
            "This profile has an operation in progress. Wait for it to finish, then try again.",
          account_sign_out_required: "Sign out before removing this profile.",
          account_confirmation_mismatch:
            "The profile confirmation does not match.",
          read_state_conflict:
            "The conversation changed. Refresh it and try again.",
          idempotency_conflict: "This send key is already in use.",
          invalid_mentions: "Choose the mentioned members again.",
          invalid_mention_text: "The message or its mention positions are invalid. Edit it and try again.",
          mentions_require_group: "Mentions are available in groups only.",
          mention_member_unavailable: "A mentioned member is unavailable. Remove the mention or choose them again.",
          mention_group_membership_unavailable: "Refresh the group’s members before sending mentions.",
          mention_all_forbidden: "You can’t mention everyone in this group.",
          mention_all_permission_unavailable: "Group permissions are unavailable. Refresh the group before mentioning everyone.",
          quote_mentions_unsupported: "Send mentions separately from quoted replies.",
          quote_target_unavailable:
            "That message is no longer available to quote.",
          quote_target_mismatch: "The quoted message changed. Select it again.",
          quote_target_ambiguous:
            "That message cannot be identified reliably. Choose another message.",
          quote_content_unavailable:
            "The content of that message cannot be quoted.",
          quote_sender_unavailable:
            "The sender of that message is unavailable.",
          unsupported_quote_type: "This message type cannot be quoted yet.",
          invalid_quote_reference:
            "Select the message you want to reply to again.",
          invalid_quote_request:
            "Select the message you want to reply to again.",
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
  const recalled = new Set(
    messages
      .filter(
        (item) =>
          item.recall &&
          item.accountId === message.accountId &&
          item.threadId === message.threadId,
      )
      .map((item) => item.serverId)
      .filter(Boolean),
  );
  if (message.recall)
    recalled.add(
      message.serverId ??
        messages.find((item) => item.id === message.id)?.serverId ??
        null,
    );
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
  ]
    .map((item) =>
      item.accountId === message.accountId &&
      item.threadId === message.threadId &&
      item.quote &&
      recalled.has(item.quote.serverId)
        ? {
            ...item,
            quote: {
              serverId: item.quote.serverId,
              type: item.quote.type,
              state: "recalled",
            },
          }
        : item,
    )
    .sort(compareMessages);
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
  private signOutListeners = new Set<(account: string) => void>();
  onSignedOut = (listener: (account: string) => void): (() => void) => {
    this.signOutListeners.add(listener);
    return () => {
      this.signOutListeners.delete(listener);
    };
  };
  private mediaInvalidationListeners = new Set<
    (account: string, messageId: string) => void
  >();
  onMediaInvalidated = (
    listener: (account: string, messageId: string) => void,
  ): (() => void) => {
    this.mediaInvalidationListeners.add(listener);
    return () => {
      this.mediaInvalidationListeners.delete(listener);
    };
  };
  private invalidateMedia(account: string, messageId: string): void {
    for (const listener of this.mediaInvalidationListeners)
      listener(account, messageId);
  }
  private settingsListeners = new Set<(account: string) => void>();
  onAccountSettings = (listener: (account: string) => void): (() => void) => {
    this.settingsListeners.add(listener);
    return () => {
      this.settingsListeners.delete(listener);
    };
  };
  private mediaListeners = new Set<(assetId: string) => void>();
  onMediaReady = (listener: (assetId: string) => void): (() => void) => {
    this.mediaListeners.add(listener);
    return () => {
      this.mediaListeners.delete(listener);
    };
  };
  private generation = 0;
  private threadGeneration = 0;
  private groupRequest = 0;
  private accountRequest = 0;
  private accountsCheckedAt = -Infinity;
  private catalogRequest = 0;
  private conversationRefresh?: {
    account: string;
    generation: number;
    running: boolean;
    again: boolean;
    catalog: boolean;
    groups: Set<string>;
    promise: Promise<void>;
  };
  private detailsRequest = 0;
  private threadRequests = new Map<string, number>();
  private interactionReads = new Set<string>();
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
  private historyValidatedAt?: number;
  private stopped = false;
  private cache?: BrowserCache;
  private restoring = false;
  private activationRequest = 0;
  private reconcileFlight?: Promise<void>;
  private restored = new Set<string>();
  private replayReady = false;
  private readonly now: () => number;
  private historyRequest?: AbortController;
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
    cache?: BrowserCache,
    now: () => number = Date.now,
    initialAccounts?: Account[],
  ) {
    this.client = client;
    this.receipts = receipts;
    this.cache = cache;
    this.now = now;
    if (initialAccounts) {
      this.value.accounts = initialAccounts;
      this.accountsCheckedAt = this.now();
    }
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
  private persist(): void {
    const account = this.value.account;
    if (!account || !this.cache || this.restoring || this.stopped) return;
    void this.cache.writeAccount(account, {
      threads: this.value.threads,
      contacts: this.value.contacts,
      histories: this.histories.export(account),
      cursor: this.cursor,
      ownUsername:
        this.value.accounts.find((item) => item.id === account)?.ownUsername ??
        undefined,
    });
  }
  async clearLocalCache(account = this.value.account): Promise<void> {
    if (!account) return;
    this.histories.clear(account);
    this.restored.clear();
    await this.cache?.clearAccount(account);
  }
  invalidateLocalCache(account: string): void {
    this.histories.clear(account);
    this.restored.clear();
    if (this.value.account === account) {
      this.historyLoaded = false;
      this.historySource = "";
      this.historyValidatedAt = undefined;
    }
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
        validatedAt: this.historyValidatedAt,
      });
    this.persist();
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
    ) {
      this.accountsCheckedAt = this.now();
      this.acceptAccounts(accounts);
    }
    return accounts;
  }
  private acceptAccounts(accounts: Account[]): void {
    const account = this.value.account;
    if (
      account &&
      accounts.some(
        (item) => item.id === account && item.authenticated === false,
      )
    ) {
      this.value = { ...this.value, accounts };
      this.signedOut(account);
    } else this.publish({ accounts });
  }
  private signedOut(account: string, cursor?: string): void {
    if (this.value.account !== account) return;
    this.resetAccount(account, true, cursor);
    for (const listener of this.signOutListeners) listener(account);
  }
  close(): void {
    this.activationRequest++;
    this.generation++;
    this.threadGeneration++;
    this.stream?.abort();
    this.historyRequest?.abort();
    this.publish(initial());
    this.stopped = true;
    this.mediaListeners.clear();
    this.settingsListeners.clear();
    this.mediaInvalidationListeners.clear();
    this.signOutListeners.clear();
    this.histories.clear();
  }
  deactivate(account = this.value.account): void {
    this.resetAccount(account, false);
  }
  private resetAccount(
    account: string | null,
    signedOut: boolean,
    cursor?: string,
  ): void {
    this.activationRequest++;
    if (account) {
      if (this.value.account === account) this.remember();
      this.histories.clear(account);
      if (this.cache?.lastAccount() === account)
        this.cache.rememberAccount(null);
      void this.cache?.signedOut(account, cursor);
    }
    if (this.value.account !== account) return;
    this.generation++;
    this.threadGeneration++;
    this.catalogRequest++;
    this.stream?.abort();
    this.historyRequest?.abort();
    this.historyLoaded = false;
    this.historySource = "";
    this.historyValidatedAt = undefined;
    this.restoring = false;
    this.replayReady = false;
    this.restored.clear();
    this.cursor = "0";
    this.publish({
      ...initial(),
      accounts: this.value.accounts.map((item) =>
        signedOut && item.id === account
          ? { ...item, authenticated: false, connection: "disconnected" }
          : item,
      ),
      signInRequired: signedOut ? account : null,
    });
  }
  async activate(account: string): Promise<void> {
    const activation = ++this.activationRequest;
    if (this.cache) {
      const accounts =
        this.now() - this.accountsCheckedAt < 5000 &&
        this.now() >= this.accountsCheckedAt
          ? this.value.accounts
          : await this.accounts();
      if (activation !== this.activationRequest || this.stopped) return;
      if (
        !accounts.some(
          (item) => item.id === account && item.authenticated === true,
        )
      ) {
        this.cache.rememberAccount(null);
        throw new NativeApiError(409, "auth_required");
      }
    }
    this.remember();
    this.historyLoaded = false;
    this.replayReady = false;
    this.historyValidatedAt = undefined;
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
      reconnecting: false,
      signInRequired: null,
      error: null,
      capabilities: {},
    });
    const controller = (this.stream = new AbortController());
    this.restoring = !!this.cache;
    this.cache?.rememberAccount(account);
    try {
      let saved = this.cache
        ? await this.cache.readAccount(account)
        : undefined;
      if (!this.current(account, generation)) return;
      const own = this.value.accounts.find(
        (item) => item.id === account,
      )?.ownUsername;
      if (saved && own && saved.ownUsername !== own) {
        await this.cache?.clearAccount(account);
        saved = undefined;
      }
      if (saved) {
        this.cursor = saved.cursor;
        this.publish({ threads: saved.threads, contacts: saved.contacts });
        for (const [thread, history] of saved.histories) {
          this.histories.set(account, thread, history);
          this.restored.add(JSON.stringify([account, thread]));
        }
      }
      const boundary = await this.refresh(account, generation, false);
      if (!this.current(account, generation)) return;
      if (saved) {
        try {
          if (BigInt(this.cursor) > BigInt(boundary))
            throw new Error("Source journal rewound");
          for (let page = 0; BigInt(this.cursor) < BigInt(boundary); page++) {
            if (page >= 10) throw new Error("Catch-up exceeds cache limit");
            const events = await this.client.events(account, this.cursor, 200);
            if (!events.items.length)
              throw new Error("Incomplete cache catch-up");
            for (const event of events.items) {
              if (!this.current(account, generation)) return;
              // Events after this inventory's boundary belong to the live
              // stream; consuming them here would skip their sidebar updates.
              if (BigInt(event.cursor) > BigInt(boundary)) break;
              if (
                ["message.committed", "message.recalled"].includes(event.kind)
              ) {
                if (
                  event.kind === "message.recalled" &&
                  typeof event.data.messageId === "string"
                ) {
                  this.invalidateMedia(account, event.data.messageId);
                  const histories = this.histories.export(account);
                  const original = histories
                    .flatMap(([, history]) => history.messages)
                    .find((message) => message.id === event.data.messageId);
                  if (original?.serverId)
                    for (const [, history] of histories)
                      for (const message of history.messages)
                        if (message.quote?.serverId === original.serverId)
                          this.invalidateMedia(account, message.id);
                }
                const affected =
                  typeof event.data.threadId === "string"
                    ? event.data.threadId
                    : this.histories
                        .export(account)
                        .find(([, history]) =>
                          history.messages.some(
                            (message) => message.id === event.data.messageId,
                          ),
                        )?.[0];
                if (affected) this.histories.delete(account, affected);
                else this.histories.clear(account);
              } else if (
                !(saved.signedOut && isSignOut(event)) &&
                ![
                  "sync.completed",
                  "thread.read",
                  "thread.read-state",
                  "group.updated",
                  "thread.updated",
                  "contacts.updated",
                  "metadata.changed",
                ].includes(event.kind)
              )
                await this.consume(event, account, generation);
              if (!this.current(account, generation)) return;
              this.cursor = event.cursor;
            }
          }
        } catch {
          if (!this.current(account, generation)) return;
          this.histories.clear(account);
          this.restored.clear();
          await this.cache?.clearHistory(account);
          if (!this.current(account, generation)) return;
          this.cursor = boundary;
        }
      } else this.cursor = boundary;
      if (saved?.signedOut) {
        await this.accounts();
        if (!this.current(account, generation)) return;
      }
      this.replayReady = true;
      this.restoring = false;
      const previous = this.cache?.view(account).thread;
      const target =
        previous && this.value.threads.some((thread) => thread.id === previous)
          ? previous
          : this.value.threads[0]?.id;
      if (target) await this.select(target);
      else this.publish({ loading: false });
    } catch (error) {
      if (this.current(account, generation))
        this.publish({ error: readableError(error), loading: false });
    }
    if (this.current(account, generation)) {
      this.restoring = false;
      this.persist();
      void this.watch(account, generation, controller.signal);
      void this.loadCatalog(account, generation).catch(() => {});
      for (const send of sends) void this.checkSend(send.key);
    }
  }
  async reconcile(): Promise<void> {
    if (this.reconcileFlight) return this.reconcileFlight;
    const account = this.value.account,
      generation = this.generation;
    if (!account || this.value.opening) return;
    this.reconcileFlight = (async () => {
      if (this.value.connected) {
        const selected = this.value.selected;
        if (selected && !this.fresh(this.histories.get(account, selected)))
          await this.select(selected, true);
        return;
      }
      await this.accounts();
      if (!this.current(account, generation)) return;
      const boundary = await this.refresh(account, generation, false);
      if (!this.current(account, generation)) return;
      if (BigInt(this.cursor) > BigInt(boundary))
        await this.recover(account, generation);
      else if (this.value.selected)
        await this.select(this.value.selected, true);
    })()
      .catch(() => {})
      .finally(() => {
        this.reconcileFlight = undefined;
      });
    return this.reconcileFlight;
  }
  private fresh(history: { validatedAt?: number } | undefined): boolean {
    const at = history?.validatedAt;
    return (
      at !== undefined &&
      Number.isFinite(at) &&
      this.now() >= at &&
      this.now() - at < HISTORY_TTL_MS
    );
  }
  async select(threadId: string, force = false): Promise<void> {
    const account = this.value.account;
    if (!account) return;
    this.remember();
    const cached = this.histories.get(account, threadId);
    this.historyRequest?.abort();
    const request = (this.historyRequest = new AbortController());
    const restoredKey = JSON.stringify([account, threadId]);
    const fromDisk = this.restored.has(restoredKey);
    this.cache?.saveView(account, { thread: threadId });
    this.historyLoaded = !!cached && !fromDisk;
    this.historySource = cached?.sourceGeneration ?? "";
    this.historyValidatedAt = cached?.validatedAt;
    const previousGroup =
      this.value.selected === threadId
        ? this.value.selectedGroup
        : cached?.group ?? null;
    const generation = this.generation,
      selected = ++this.threadGeneration;
    const current = () =>
      this.current(account, generation) && selected === this.threadGeneration;
    this.publish({
      selected: threadId,
      selectedGroup: previousGroup ? { ...previousGroup, canMentionAll: null } : null,
      messages: fromDisk ? [] : cached?.messages ?? [],
      nextMessages: cached?.nextMessages ?? null,
      loading: true,
      opening: true,
      error: null,
    });
    try {
      for (let attempt = 0; attempt < 2; attempt++) {
        const baseline =
          attempt === 0 ? cached?.messages ?? [] : this.value.messages;
        // Reuse a reconciled list entry only while its history is still fresh.
        const listed = this.value.threads.find(
          (thread) => thread.id === threadId,
        );
        const readBoundary =
          attempt === 0 &&
          !force &&
          listed &&
          ((cached && this.value.connected && this.fresh(cached)) ||
            (fromDisk && this.replayReady))
            ? listed
            : await this.refreshThread(
                account,
                threadId,
                generation,
                request.signal,
              );
        if (!current()) return;
        if (
          attempt === 0 &&
          cached &&
          (!fromDisk || this.replayReady) &&
          !force &&
          (this.value.connected || (fromDisk && this.replayReady)) &&
          this.fresh(cached) &&
          readBoundary.lastMessage !== undefined &&
          (readBoundary.lastMessage?.id ?? null) ===
            (cached.messages.at(-1)?.id ?? null) &&
          readBoundary.unreadCount === 0
        ) {
          this.historyLoaded = true;
          this.restored.delete(restoredKey);
          this.publish({ messages: cached.messages, loading: false });
          return; // Replay maintains this cache only until its history TTL expires.
        }
        const validatedAt = this.now();
        const page = await this.client.messages(
          account,
          threadId,
          {
            limit: 40,
          },
          request.signal,
        );
        if (!current()) return;
        const latest = (
          page.items.filter(
            (item) => !isNativeTombstone(item),
          ) as NativeMessage[]
        ).reduce(mergeMessage, []);
        // Preserve older loaded pages, replace the current server range, and apply
        // only events received after this request on top of its snapshot.
        const older =
          page.nextCursor &&
          latest.length &&
          this.historySource === page.sourceGeneration
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
        this.restored.delete(restoredKey);
        this.historyLoaded = true;
        this.historySource = page.sourceGeneration;
        this.historyValidatedAt = validatedAt;
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
      if (current()) {
        this.publish({ opening: false });
        if (threadId.endsWith("@chatroom"))
          void this.refreshGroup(account, threadId, generation, selected).catch(
            () => {},
          );
      }
    }
  }
  async older(): Promise<void> {
    const { account, selected, nextMessages } = this.value,
      generation = this.generation,
      threadGeneration = this.threadGeneration;
    if (!account || !selected || !nextMessages || this.value.loading) return;
    this.publish({ loading: true });
    try {
      const page = await this.client.messages(
        account,
        selected,
        {
          cursor: nextMessages,
          limit: 100,
        },
        this.historyRequest?.signal,
      );
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
        if (
          error instanceof NativeApiError &&
          error.code === "pagination_cursor_expired"
        ) {
          this.histories.clear(account);
          await this.select(selected);
        } else this.publish({ loading: false, error: readableError(error) });
    }
  }
  async revealMessage(id: string, signal: AbortSignal): Promise<boolean> {
    const { account, selected } = this.value;
    const generation = this.generation,
      threadGeneration = this.threadGeneration;
    const current = () =>
      !signal.aborted &&
      !!account &&
      this.current(account, generation) &&
      threadGeneration === this.threadGeneration;
    while (current()) {
      if (
        this.value.messages.some(
          (message) =>
            message.id === id &&
            message.accountId === account &&
            message.threadId === selected,
        )
      )
        return true;
      if (this.value.loading) {
        await pause(50, signal);
        continue;
      }
      const cursor = this.value.nextMessages;
      if (!cursor) return false;
      await this.older();
      if (this.value.nextMessages === cursor) return false;
    }
    return false;
  }
  async revealQuote(
    parent: NativeMessage,
    signal: AbortSignal,
  ): Promise<NativeMessage | undefined> {
    const { account, selected } = this.value;
    const generation = this.generation,
      threadGeneration = this.threadGeneration;
    if (
      !parent.quote ||
      parent.accountId !== account ||
      parent.threadId !== selected
    )
      return;
    const current = () =>
      !signal.aborted &&
      !!account &&
      this.current(account, generation) &&
      threadGeneration === this.threadGeneration;
    while (current()) {
      const matches = this.value.messages.filter((message) =>
        quoteTargetMatches(parent, message),
      );
      if (matches.length) return matches.length === 1 ? matches[0] : undefined;
      if (this.value.loading) {
        await pause(50, signal);
        continue;
      }
      const cursor = this.value.nextMessages;
      if (!cursor) return;
      await this.older();
      if (this.value.nextMessages === cursor) return;
    }
  }
  private async refreshThread(
    account: string,
    threadId: string,
    generation: number,
    signal?: AbortSignal,
  ): Promise<NativeThread> {
    const key = JSON.stringify([account, threadId]);
    const inventoryVersion = this.catalogRequest;
    const request = (this.threadRequests.get(key) ?? 0) + 1;
    this.threadRequests.set(key, request);
    let thread: NativeThread;
    try {
      thread = await this.client.thread(account, threadId, signal);
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
      inventoryVersion === this.catalogRequest &&
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
    const request = this.invalidateAllMentionPermission();
    const group = await this.client.group(account, thread);
    if (
      this.current(account, generation) &&
      selected === this.threadGeneration &&
      thread === this.value.selected &&
      request === this.groupRequest &&
      group.id === thread
    ) {
      this.publish({ selectedGroup: group });
      this.remember();
    }
  }
  private invalidateAllMentionPermission(): number {
    const request = ++this.groupRequest;
    const group = this.value.selectedGroup;
    if (group && group.canMentionAll !== null)
      this.publish({ selectedGroup: { ...group, canMentionAll: null } });
    return request;
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
  async readOnInteraction(account: string, threadId: string): Promise<void> {
    if (
      this.stopped ||
      account !== this.value.account ||
      threadId !== this.value.selected ||
      !this.historyLoaded ||
      this.value.opening
    )
      return;
    const thread = this.value.threads.find((item) => item.id === threadId);
    if (!thread?.readStateVersion || (thread.unreadCount ?? 0) <= 0) return;
    const generation = this.generation;
    const version = thread.readStateVersion;
    const key = JSON.stringify([account, threadId, version]);
    if (this.interactionReads.has(key)) return;
    this.interactionReads.add(key);
    try {
      // Focusing or replying acknowledges this view, never a newer unseen version.
      await this.client.markRead(account, threadId, version);
    } catch (error) {
      if (
        this.current(account, generation) &&
        this.value.selected === threadId &&
        !this.value.error &&
        !(
          error instanceof NativeApiError &&
          error.code === "read_state_conflict"
        )
      )
        this.publish({
          error: `Couldn’t clear unread status. ${readableError(error)}`,
        });
    } finally {
      if (this.current(account, generation))
        await this.refreshThread(account, threadId, generation).catch(() => {});
      this.interactionReads.delete(key);
    }
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
        this.now() - this.accountsCheckedAt < 5000
          ? Promise.resolve({ items: this.value.accounts })
          : this.client.accounts(),
      ]);
      if (
        this.current(account, generation) &&
        request === this.detailsRequest
      ) {
        this.acceptAccounts(
          accounts.items.filter(
            (item) => typeof item.id === "string",
          ) as Account[],
        );
        if (!this.current(account, generation)) return;
        this.publish({
          contacts: contacts.items,
          capabilities,
        });
        this.persist();
      }
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
    if (catalog && this.current(account, generation) && this.value.selected?.endsWith("@chatroom"))
      this.invalidateAllMentionPermission();
    const request = ++this.catalogRequest;
    const revisions = new Map(this.threadRequests);
    const threads = await inventory((cursor) =>
      this.client.threads(account, { cursor, limit: 200 }),
    );
    if (this.current(account, generation) && request === this.catalogRequest) {
      // A point read begun during this inventory may carry a newer setting.
      // Restart at page one instead of splicing it into an older page order.
      if (
        threads.items.some(
          (thread) =>
            revisions.get(JSON.stringify([account, thread.id])) !==
            this.threadRequests.get(JSON.stringify([account, thread.id])),
        )
      )
        return this.refresh(account, generation, catalog);
      this.publish({ threads: threads.items });
      this.persist();
    }
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
  /** Coalesce metadata events and mutation completion without opening a chat. */
  refreshConversations(
    account = this.value.account,
    catalog = false,
    group?: string,
  ): Promise<void> {
    const generation = this.generation;
    if (!account || !this.current(account, generation))
      return Promise.resolve();
    if (this.value.selected?.endsWith("@chatroom") && (catalog || group === this.value.selected))
      this.invalidateAllMentionPermission();
    // Invalidate older inventories and point reads immediately, before the timer.
    this.catalogRequest++;
    const previous = this.conversationRefresh;
    if (previous?.account === account && previous.generation === generation) {
      previous.again ||= previous.running;
      previous.catalog ||= catalog;
      if (group) previous.groups.add(group);
      return previous.promise;
    }
    const work = {
      account,
      generation,
      running: false,
      again: false,
      catalog,
      groups: new Set(group ? [group] : []),
      promise: Promise.resolve(),
    };
    this.conversationRefresh = work;
    work.promise = (async () => {
      await pause(50, this.stream?.signal ?? new AbortController().signal);
      do {
        if (!this.current(account, generation)) return;
        work.again = false;
        work.running = true;
        const withCatalog = work.catalog;
        work.catalog = false;
        const groups = [...work.groups];
        work.groups.clear();
        const before = this.catalogRequest;
        await this.refresh(account, generation, withCatalog);
        if (!this.current(account, generation)) return;
        if (this.catalogRequest !== before + 1) work.again = true;
        if (!withCatalog && groups.includes(this.value.selected ?? ""))
          await this.refreshGroup(
            account,
            this.value.selected!,
            generation,
            this.threadGeneration,
          ).catch(() => {});
      } while (work.again || work.catalog || work.groups.size);
      this.persist();
    })().finally(() => {
      if (this.conversationRefresh === work)
        this.conversationRefresh = undefined;
    });
    return work.promise;
  }
  async refreshMessage(account: string, messageId: string): Promise<void> {
    if (this.value.account !== account) return;
    await this.canonical(account, messageId, this.generation);
  }
  private async canonical(
    account: string,
    messageId: string,
    generation: number,
  ): Promise<NativeMessage | undefined> {
    const message = await this.client.message(account, messageId);
    if (!this.current(account, generation)) return;
    if (message.recall) {
      this.invalidateMedia(account, messageId);
      const messages =
        message.threadId === this.value.selected
          ? this.value.messages
          : this.histories.get(account, message.threadId)?.messages ?? [];
      const server =
        message.serverId ??
        messages.find((item) => item.id === message.id)?.serverId;
      for (const quoted of messages) {
        if (
          server &&
          quoted.accountId === account &&
          quoted.threadId === message.threadId &&
          quoted.quote?.serverId === server
        )
          this.invalidateMedia(account, quoted.id);
      }
    }
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
    const settled = sends.find((send) => send.intent?.id === intent.id)?.intent;
    // Accepted and cancelled intents cannot return to the queue. An admission
    // response or status read may arrive after cancellation or an SSE update.
    if (
      settled &&
      ["accepted", "cancelled"].includes(settled.state) &&
      intent.state !== settled.state
    )
      intent = settled;
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
      await this.refreshConversations(account);
    }
  }
  async consume(
    event: NativeEvent,
    account: string,
    generation = this.generation,
  ): Promise<void> {
    if (!this.current(account, generation)) return;
    if (isSignOut(event)) {
      this.signedOut(account, event.cursor);
      return;
    }
    if (event.kind === "account.settings_updated")
      for (const listener of this.settingsListeners) listener(account);
    if (
      event.kind === "message.recalled" &&
      typeof event.data.messageId === "string"
    )
      this.invalidateMedia(account, event.data.messageId);
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
        await this.refreshConversations(account);
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
    } else if (["thread.updated", "group.updated"].includes(event.kind)) {
      await this.refreshConversations(
        account,
        false,
        typeof event.data.groupId === "string" ? event.data.groupId
          : event.kind === "group.updated" && typeof event.data.threadId === "string" ? event.data.threadId : undefined,
      );
    } else if (
      ["account.updated", "account.connection", "auth.updated"].includes(
        event.kind,
      )
    ) {
      await this.accounts();
    } else if (
      event.kind === "contacts.updated" &&
      event.data.origin === "contact_sync"
    ) {
      void this.refreshConversations(account, true).catch((error) => {
        if (this.current(account, generation))
          this.publish({ error: readableError(error) });
      });
    } else if (
      event.kind === "contacts.updated" ||
      (event.kind === "metadata.changed" && event.data.kind !== "avatar")
    ) {
      await this.refreshConversations(account, true);
    } else if (event.kind === "sync.completed") {
      await this.refreshConversations(account);
    }
    // Push hints and control diagnostics carry no conversation content.
  }
  private async recover(account: string, generation: number): Promise<void> {
    const boundary = await this.refresh(account, generation);
    if (!this.current(account, generation)) return;
    this.histories.clear(account);
    this.restored.clear();
    await this.cache?.clearHistory(account);
    if (!this.current(account, generation)) return;
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
        if (this.value.reconnecting) await this.refreshConversations(account);
        if (signal.aborted || !this.current(account, generation)) return;
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
                this.publish({
                  connected: true,
                  reconnecting: false,
                  error: null,
                });
            },
          },
        )) {
          if (signal.aborted || !this.current(account, generation)) return;
          const metadataOnly =
            [
              "thread.updated",
              "group.updated",
              "contacts.updated",
              "sync.completed",
            ].includes(event.kind) ||
            (event.kind === "metadata.changed" && event.data.kind !== "avatar");
          const applied = this.consume(event, account, generation);
          if (metadataOnly) {
            // Do not serialize an entire contact-sync burst into one read per
            // event. The shared refresh persists the final order when complete.
            void applied.catch((error) => {
              if (this.current(account, generation))
                this.publish({ error: readableError(error) });
            });
          } else await applied;
          if (this.current(account, generation)) {
            this.cursor = event.cursor;
            if (metadataOnly) continue;
            if (
              [
                "message.committed",
                "message.recalled",
                "group.updated",
                "sync.completed",
                "contacts.updated",
                "metadata.changed",
                "thread.read",
                "thread.read-state",
              ].includes(event.kind)
            )
              this.persist();
            else void this.cache?.checkpoint(account, this.cursor);
          }
        }
        if (!signal.aborted && this.current(account, generation))
          this.publish({ connected: false, reconnecting: true });
      } catch (error) {
        if (signal.aborted || !this.current(account, generation)) return;
        if (error instanceof NativeApiError && error.code === "auth_required") {
          this.signedOut(account);
          return;
        }
        this.publish({
          connected: false,
          reconnecting: true,
          error: readableError(error),
        });
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
  async send(text: string, replyTo?: NativeMessage, tokens: MentionToken[] = []): Promise<boolean> {
    const account = this.value.account,
      threadId = this.value.selected,
      generation = this.generation;
    if (!account || !threadId || !text.trim()) return false;
    let mentions: TextMentions | undefined;
    try { mentions = mentionOptions(text, tokens); }
    catch (error) { this.publish({ error: error instanceof Error ? error.message : "Choose the mentioned members again." }); return false; }
    if (mentions && (replyTo || !threadId.endsWith("@chatroom") || this.value.capabilities.inlineTextMentions !== true)) {
      this.publish({ error: replyTo ? "Send mentions separately from quoted replies." : "This hub cannot send inline mentions. Remove them or update the hub." });
      return false;
    }
    if (mentions?.all && !canMentionAll(this.value.selectedGroup, threadId)) {
      this.publish({ error: allMentionProblem(this.value.selectedGroup, threadId) });
      return false;
    }
    if (this.value.selectedGroup?.isInGroup === false) {
      this.publish({
        error:
          "You are no longer in this group. Cached history is still available.",
      });
      return false;
    }
    const target =
      replyTo &&
      (this.value.messages.find((message) => message.id === replyTo.id) ??
        replyTo);
    if (
      target &&
      (replyTo?.accountId !== account ||
        replyTo?.threadId !== threadId ||
        target.accountId !== account ||
        target.threadId !== threadId ||
        target.serverId !== replyTo?.serverId ||
        !canReply(target) ||
        this.value.capabilities.quotedTextSend !== true)
    ) {
      this.publish({
        error:
          "This message can’t be quoted. Choose another message or remove the reply.",
      });
      return false;
    }
    const quote = target
      ? { messageId: target.id, serverId: target.serverId! }
      : undefined;
    const quotePreview = target
      ? messageSummary(
          target,
          target.direction === "outgoing"
            ? "You"
            : this.value.contacts.find(
                (contact) => contact.id === target.senderId,
              )?.displayName ?? target.senderId,
        )
      : undefined;
    const key = crypto.randomUUID();
    this.publish({
      sends: [
        ...this.value.sends,
        { key, text, threadId, ...(quote && { quote, quotePreview }), ...(mentions && { mentions }) },
      ],
      error: null,
    });
    void this.readOnInteraction(account, threadId);
    try {
      const intent = await this.client.sendText(
        account,
        threadId,
        text,
        key,
        mentions ? { mentions } : quote,
      );
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
      if (
        this.current(account, generation) &&
        error instanceof NativeApiError &&
        error.code === "auth_required"
      )
        this.signedOut(account);
      if (
        (quote || mentions) &&
        error instanceof NativeApiError &&
        !error.retryable &&
        [
          "invalid_quote_reference",
          "invalid_quote_request",
          "unsupported_quote_type",
          "quote_target_unavailable",
          "quote_target_mismatch",
          "quote_target_ambiguous",
          "quote_content_unavailable",
          "quote_sender_unavailable",
          "invalid_mentions", "invalid_mention_text", "mentions_require_group", "mention_member_unavailable", "mention_group_membership_unavailable", "mention_all_forbidden", "mention_all_permission_unavailable", "quote_mentions_unsupported", "invalid_request",
        ].includes(error.code)
      ) {
        // These admission errors occur before an intent exists. Preserve the
        // composer's text/reference instead of showing an uncertain send.
        if (this.current(account, generation)) {
          if (this.value.selected === threadId && ["mention_all_forbidden", "mention_all_permission_unavailable"].includes(error.code))
            this.invalidateAllMentionPermission();
          this.publish({
            sends: this.value.sends.filter((send) => send.key !== key),
            error: readableError(error),
          });
        } else {
          const sends = (this.pending.get(account) ?? []).filter(
            (send) => send.key !== key,
          );
          this.pending.set(account, sends);
          this.receipts?.write(account, sends);
        }
        return false;
      }
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
    return true;
  }
  async checkSend(key: string): Promise<void> {
    const account = this.value.account,
      generation = this.generation;
    if (!account) return;
    const known = this.value.sends.find((send) => send.key === key)?.intent;
    try {
      const receipt = known
        ? await this.client.send(account, known.id)
        : await this.client.sendByKey(account, key);
      if (this.value.sends.find((send) => send.key === key)?.intent !== known)
        return;
      if ("clientId" in receipt)
        await this.acceptReceipt(receipt, account, generation, key);
    } catch (error) {
      if (
        this.current(account, generation) &&
        this.value.sends.find((send) => send.key === key)?.intent === known
      )
        this.publish({
          sends: this.value.sends.map((send) =>
            send.key === key
              ? { ...send, uncertain: true, error: readableError(error) }
              : send,
          ),
        });
    }
  }
  async cancelSend(key: string): Promise<void> {
    const account = this.value.account,
      generation = this.generation,
      known = this.value.sends.find((send) => send.key === key)?.intent;
    if (!account || known?.state !== "queued") return;
    try {
      const receipt = await this.client.cancelSend(account, known.id);
      if ("clientId" in receipt)
        await this.acceptReceipt(receipt, account, generation, key);
    } catch (error) {
      if (
        this.current(account, generation) &&
        this.value.sends.find((send) => send.key === key)?.intent === known
      )
        this.publish({
          sends: this.value.sends.map((send) =>
            send.key === key
              ? { ...send, uncertain: true, error: readableError(error) }
              : send,
          ),
        });
    }
  }
  dismissSend(key: string): void {
    this.publish({
      sends: this.value.sends.map((send) =>
        send.key === key &&
        (send.uncertain ||
          ["failed", "rejected", "cancelled", "needs_review"].includes(
            send.intent?.state ?? "",
          ))
          ? { ...send, dismissed: true }
          : send,
      ),
    });
  }
}
