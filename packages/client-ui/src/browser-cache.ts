import type {
  NativeContact,
  NativeMessage,
  NativeThread,
} from "@agent-wechat/shared";
import type { CachedHistory } from "./history-cache.ts";
import type { ConversationCategory } from "./conversation-filters.ts";

const stores = [
  "threads",
  "contacts",
  "messages",
  "chatState",
  "syncState",
  "avatars",
  "avatarMeta",
  "media",
  "mediaMeta",
  "drafts",
  "receipts",
] as const;
type Store = (typeof stores)[number];
type BlobStore = "avatars" | "media";
type BlobMetaStore = "avatarMeta" | "mediaMeta";
const blobStores: readonly BlobStore[] = ["avatars", "media"];
const metaStore = (kind: BlobStore): BlobMetaStore =>
  kind === "media" ? "mediaMeta" : "avatarMeta";
// Blob size and recency are available without loading cached payloads.
const fetchedStats: Store[] = [
  "threads",
  "contacts",
  "messages",
  "chatState",
  "syncState",
  "avatarMeta",
  "mediaMeta",
];
const fetched: Store[] = [...fetchedStats, "avatars", "media"];
export type AccountSnapshot = {
  threads: NativeThread[];
  contacts: NativeContact[];
  histories: [string, CachedHistory][];
  cursor: string;
  ownUsername?: string;
  signedOut?: boolean;
};
export type ViewState = {
  thread?: string;
  section?: "chats" | "contacts";
  categories?: ConversationCategory[];
  filter?: "all" | "unread" | "groups";
};
export type SavedBlob = {
  blob: Blob;
  checkedAt: number;
  refreshedAt?: number;
  etag?: string | null;
};
type StoredBlob = Omit<SavedBlob, "blob"> & {
  content: ArrayBuffer;
  contentType: string;
};
type Row = {
  key: string[];
  scope: string;
  data: unknown;
  lastUsed: number;
  bytes?: number;
  messageId?: string;
};
type BlobRow = Pick<Row, "key" | "scope" | "data" | "messageId">;
type BlobMetadata = Omit<Row, "data">;
const blobBytes = (data: StoredBlob | SavedBlob | undefined): number => {
  if (!data || typeof data !== "object") return 0;
  if ("content" in data && typeof data.content?.byteLength === "number")
    return data.content.byteLength;
  if ("blob" in data && typeof data.blob?.size === "number")
    return data.blob.size;
  return 0;
};
const metadataFromRow = (
  row: Partial<Row>,
  fallbackLastUsed = 0,
): BlobMetadata | null => {
  if (!Array.isArray(row.key)) return null;
  const payload = row.data as StoredBlob | SavedBlob | undefined;
  return {
    key: row.key,
    scope:
      typeof row.scope === "string"
        ? row.scope
        : JSON.stringify(row.key.slice(0, -1)),
    lastUsed:
      typeof row.lastUsed === "number" && Number.isFinite(row.lastUsed)
        ? row.lastUsed
        : fallbackLastUsed,
    bytes:
      typeof row.bytes === "number" &&
      Number.isFinite(row.bytes) &&
      row.bytes >= 0
        ? row.bytes
        : blobBytes(payload),
    messageId: row.messageId,
  };
};
const request = <T>(value: IDBRequest<T>): Promise<T> =>
  new Promise((resolve, reject) => {
    value.onsuccess = () => resolve(value.result);
    value.onerror = () => reject(value.error);
  });
const decimal = (value: string) => /^\d+$/.test(value);
const older = (a: string, b: string) =>
  decimal(a) && decimal(b) && BigInt(a) < BigInt(b);

/** Optional browser replica. Credentials never enter this database. */
export class BrowserCache {
  readonly namespace: string[];
  readonly mediaBudget: number;
  private database: Promise<IDBDatabase | null>;
  private tail: Promise<unknown> = Promise.resolve();
  private recovery = new Map<string, unknown>();
  private storage?: Storage;
  private now: () => number;
  private channel?: BroadcastChannel;
  private invalidations = new Set<(account: string) => void>();
  private closing = false;
  private closed = false;
  private failed = false;
  private cancelBlockedOpen?: () => void;
  private cancelOpen?: () => void;
  constructor(
    origin: string,
    hubId: string,
    options: {
      factory?: IDBFactory;
      storage?: Storage;
      name?: string;
      now?: () => number;
      mediaBudget?: number;
      broadcast?: boolean;
      blockedTimeoutMs?: number;
    } = {},
  ) {
    this.namespace = [new URL(origin).origin, hubId];
    this.storage = options.storage;
    this.now = options.now ?? Date.now;
    this.mediaBudget = options.mediaBudget ?? 256 * 1024 * 1024;
    const factory = options.factory ?? globalThis.indexedDB;
    this.database = new Promise((resolve) => {
      if (!factory) {
        this.failed = true;
        resolve(null);
        return;
      }
      let open: IDBOpenDBRequest;
      try {
        open = factory.open(options.name ?? "agent-wechat-cache", 2);
      } catch {
        this.failed = true;
        resolve(null);
        return;
      }
      let settled = false;
      let blockedTimer: ReturnType<typeof setTimeout> | undefined;
      const clearBlockedTimer = () => {
        if (blockedTimer === undefined) return;
        clearTimeout(blockedTimer);
        blockedTimer = undefined;
      };
      const settle = (db: IDBDatabase | null) => {
        if (settled) return;
        settled = true;
        clearBlockedTimer();
        resolve(db);
      };
      const failOpen = () => {
        if (settled) return;
        this.failed = true;
        settle(null);
      };
      this.cancelBlockedOpen = () => {
        if (blockedTimer !== undefined) settle(null);
      };
      this.cancelOpen = () => {
        settle(null);
      };
      open.onupgradeneeded = (event) => {
        clearBlockedTimer();
        for (const name of stores) {
          if (open.result.objectStoreNames.contains(name)) continue;
          const store = open.result.createObjectStore(name, { keyPath: "key" });
          store.createIndex("scope", "scope");
        }
        if (event.oldVersion === 1) {
          const tx = open.transaction!;
          for (const kind of blobStores) {
            const payload = tx.objectStore(kind);
            const meta = tx.objectStore(metaStore(kind));
            // Media and avatars are disposable caches: if any legacy row hits a
            // request-level read or write error during upgrade, clear that cache
            // so the upgrade still commits and drafts/receipts remain accessible.
            let reset = false;
            const clearCache = () => {
              if (reset) return;
              reset = true;
              payload.clear();
              meta.clear();
            };
            const recover = (errEvent: Event) => {
              errEvent.preventDefault();
              errEvent.stopPropagation();
              clearCache();
            };
            // Backfill the small rows without modifying any existing payload bytes.
            const scan = payload.openCursor();
            scan.onerror = recover;
            scan.onsuccess = () => {
              if (reset) return;
              const cursor = scan.result;
              if (!cursor) return;
              const entry = metadataFromRow(cursor.value as Partial<Row>);
              if (entry) {
                try {
                  const write = meta.put(entry);
                  write.onerror = recover;
                } catch {
                  clearCache();
                  return;
                }
              }
              cursor.continue();
            };
          }
        }
      };
      open.onerror = () => {
        failOpen();
      };
      open.onblocked = () => {
        if (settled || this.closing || this.closed) {
          settle(null);
          return;
        }
        if (blockedTimer !== undefined) return;
        blockedTimer = setTimeout(failOpen, options.blockedTimeoutMs ?? 3000);
        (blockedTimer as unknown as { unref?: () => void }).unref?.();
      };
      open.onsuccess = () => {
        const db = open.result;
        if (settled || this.closed) {
          db.close();
          settle(null);
          return;
        }
        db.onversionchange = () => {
          db.close();
          this.failed = true;
        };
        settle(db);
      };
    });
    if (
      options.broadcast !== false &&
      typeof window !== "undefined" &&
      typeof BroadcastChannel !== "undefined"
    ) {
      this.channel = new BroadcastChannel(
        `agentwechat-cache:${JSON.stringify(this.namespace)}`,
      );
      this.channel.onmessage = (event) => {
        if (
          event.data?.kind === "invalidate" &&
          typeof event.data.account === "string"
        )
          for (const listener of this.invalidations)
            listener(event.data.account);
      };
    }
  }
  get available() {
    return !this.failed;
  }
  private key(account: string, id: string) {
    return [...this.namespace, account, id];
  }
  private scope(account: string) {
    return JSON.stringify([...this.namespace, account]);
  }
  private row(account: string, id: string, data: unknown): Row {
    return {
      key: this.key(account, id),
      scope: this.scope(account),
      data,
      lastUsed: this.now(),
      bytes: new TextEncoder().encode(JSON.stringify(data) ?? "").byteLength,
    };
  }
  private async transaction<T>(
    names: Store[],
    mode: IDBTransactionMode,
    work: (tx: IDBTransaction) => Promise<T> | T,
  ): Promise<T | undefined> {
    const db = await this.database;
    if (!db || this.closed) return;
    try {
      const tx = db.transaction(names, mode);
      const done = new Promise<void>((resolve, reject) => {
        tx.oncomplete = () => resolve();
        tx.onabort = () =>
          reject(tx.error ?? new Error("Cache transaction aborted"));
        tx.onerror = () => {}; // The abort handler owns the failure.
      });
      // Observe aborts even if a request rejects before we reach `await done`.
      void done.catch(() => {});
      let value: T;
      try {
        value = await work(tx);
      } catch (error) {
        try {
          tx.abort();
        } catch {}
        throw error;
      }
      await done;
      return value;
    } catch {
      this.failed = true;
      return;
    }
  }
  private enqueue<T>(work: () => Promise<T>): Promise<T> {
    const queued = this.tail.then(work);
    this.tail = queued.catch(() => {});
    return queued;
  }
  async flush() {
    await this.tail;
  }
  private async rows<T = Row>(
    tx: IDBTransaction,
    name: Store,
    account: string,
  ): Promise<T[]> {
    return request(
      tx.objectStore(name).index("scope").getAll(this.scope(account)),
    );
  }
  private async removeScope(tx: IDBTransaction, name: Store, account: string) {
    const keys = await request(
      tx.objectStore(name).index("scope").getAllKeys(this.scope(account)),
    );
    for (const key of keys) tx.objectStore(name).delete(key);
  }
  async readAccount(account: string): Promise<AccountSnapshot | undefined> {
    await this.flush();
    return this.transaction(
      ["threads", "contacts", "messages", "chatState", "syncState"],
      "readonly",
      async (tx) => {
        const sync = await request<Row | undefined>(
          tx.objectStore("syncState").get(this.key(account, "state")),
        );
        if (!sync) return undefined;
        const state = sync.data as {
          cursor: string;
          threadIds: string[];
          contactIds: string[];
          ownUsername?: string;
          signedOut?: boolean;
        };
        if (!decimal(state.cursor)) return undefined;
        const threads = new Map(
          (await this.rows(tx, "threads", account)).map((row) => [
            (row.data as NativeThread).id,
            row.data as NativeThread,
          ]),
        );
        const contacts = new Map(
          (await this.rows(tx, "contacts", account)).map((row) => [
            (row.data as NativeContact).id,
            row.data as NativeContact,
          ]),
        );
        const messages = new Map(
          (await this.rows(tx, "messages", account)).map((row) => [
            (row.data as NativeMessage).id,
            row.data as NativeMessage,
          ]),
        );
        const histories: [string, CachedHistory][] = [];
        for (const row of await this.rows(tx, "chatState", account)) {
          const chat = row.data as Omit<CachedHistory, "messages"> & {
            ids: string[];
          };
          const items = chat.ids.map((id) => messages.get(id));
          if (
            items.some(
              (item) =>
                !item ||
                item.accountId !== account ||
                item.threadId !== row.key[3],
            )
          )
            continue;
          const { ids: _, ...metadata } = chat;
          histories.push([
            row.key[3],
            { ...metadata, messages: items as NativeMessage[] },
          ]);
        }
        return {
          threads: state.threadIds.flatMap((id) => threads.get(id) ?? []),
          contacts: state.contactIds.flatMap((id) => contacts.get(id) ?? []),
          histories,
          cursor: state.cursor,
          ...(state.ownUsername ? { ownUsername: state.ownUsername } : {}),
          ...(state.signedOut ? { signedOut: true } : {}),
        };
      },
    );
  }
  writeAccount(account: string, snapshot: AccountSnapshot): Promise<unknown> {
    // Capture at admission, not when an earlier queued transaction finishes.
    const value = structuredClone(snapshot);
    return this.enqueue(() =>
      this.transaction(
        ["threads", "contacts", "messages", "chatState", "syncState"],
        "readwrite",
        async (tx) => {
          const previous = await request<Row | undefined>(
            tx.objectStore("syncState").get(this.key(account, "state")),
          );
          const cursor = (previous?.data as { cursor?: string } | undefined)
            ?.cursor;
          // A slower tab must not replace a replica that has already applied newer events.
          if (cursor && older(value.cursor, cursor)) return;
          for (const name of [
            "threads",
            "contacts",
            "messages",
            "chatState",
          ] as Store[])
            await this.removeScope(tx, name, account);
          for (const item of value.threads)
            if (item.accountId === account)
              tx.objectStore("threads").put(this.row(account, item.id, item));
          for (const item of value.contacts)
            tx.objectStore("contacts").put(this.row(account, item.id, item));
          for (const [thread, chat] of value.histories) {
            const items = chat.messages.filter(
              (item) => item.accountId === account && item.threadId === thread,
            );
            for (const item of items)
              tx.objectStore("messages").put(this.row(account, item.id, item));
            const { messages: _, ...metadata } = chat;
            tx.objectStore("chatState").put(
              this.row(account, thread, {
                ...metadata,
                ids: items.map((item) => item.id),
              }),
            );
          }
          tx.objectStore("syncState").put(
            this.row(account, "state", {
              cursor: value.cursor,
              threadIds: value.threads.map((item) => item.id),
              contactIds: value.contacts.map((item) => item.id),
              ownUsername: value.ownUsername,
              signedOut: value.signedOut ?? false,
            }),
          );
        },
      ),
    );
  }
  checkpoint(account: string, cursor: string) {
    return this.enqueue(() =>
      this.transaction(["syncState"], "readwrite", async (tx) => {
        const row = await request<Row | undefined>(
          tx.objectStore("syncState").get(this.key(account, "state")),
        );
        if (!row) return;
        const state = row.data as { cursor: string };
        if (older(cursor, state.cursor)) return;
        state.cursor = cursor;
        tx.objectStore("syncState").put(row);
      }),
    );
  }
  signedOut(account: string, cursor?: string) {
    return this.enqueue(() =>
      this.transaction(["syncState"], "readwrite", async (tx) => {
        const row = await request<Row | undefined>(
          tx.objectStore("syncState").get(this.key(account, "state")),
        );
        if (!row) return;
        const state = row.data as { cursor: string; signedOut?: boolean };
        state.signedOut = true;
        if (cursor && decimal(cursor) && !older(cursor, state.cursor))
          state.cursor = cursor;
        tx.objectStore("syncState").put(row);
      }),
    );
  }
  async primeRecovery(accounts: string[]) {
    await this.flush();
    await this.transaction(["receipts", "drafts"], "readonly", async (tx) => {
      for (const account of accounts)
        for (const name of ["receipts", "drafts"] as const)
          for (const row of await this.rows(tx, name, account))
            this.recovery.set(JSON.stringify([name, ...row.key]), row.data);
    });
  }
  readState<T>(
    account: string,
    id: string,
    kind: "receipts" | "drafts" = "receipts",
  ): T | undefined {
    return this.recovery.get(
      JSON.stringify([kind, ...this.key(account, id)]),
    ) as T | undefined;
  }
  states<T>(account: string, prefix: string): T[] {
    const root =
      JSON.stringify(["receipts", ...this.namespace, account]).slice(0, -1) +
      ",";
    return [...this.recovery]
      .filter(
        ([key]) =>
          key.startsWith(root) &&
          (JSON.parse(key) as string[])[4].startsWith(prefix),
      )
      .map(([, value]) => value as T);
  }
  writeState(
    account: string,
    id: string,
    data: unknown,
    kind: "receipts" | "drafts" = "receipts",
  ) {
    const key = JSON.stringify([kind, ...this.key(account, id)]);
    if (data == null) this.recovery.delete(key);
    else this.recovery.set(key, structuredClone(data));
    const row = this.row(
      account,
      id,
      data == null ? null : this.recovery.get(key),
    );
    void this.enqueue(() =>
      this.transaction([kind], "readwrite", (tx) => {
        if (data == null) tx.objectStore(kind).delete(row.key);
        else tx.objectStore(kind).put(row);
      }),
    );
  }
  async blob(
    kind: BlobStore,
    account: string,
    id: string,
  ): Promise<SavedBlob | undefined> {
    await this.flush();
    const key = this.key(account, id);
    const metaName = metaStore(kind);
    const names: Store[] = [kind, metaName];
    const data = await this.transaction(names, "readwrite", async (tx) => {
      const store = tx.objectStore(kind);
      const meta = tx.objectStore(metaName);
      const [row, entry] = await Promise.all([
        request<Row | BlobRow | undefined>(store.get(key)),
        request<BlobMetadata | undefined>(meta.get(key)),
      ]);
      if (!row) return;
      const lastUsed = this.now();
      // Updating recency must not serialize the cached ArrayBuffer again.
      if (entry) {
        meta.put({ ...entry, lastUsed });
      } else {
        const repaired = metadataFromRow(row, lastUsed);
        if (repaired) meta.put({ ...repaired, lastUsed });
      }
      return row.data as StoredBlob | SavedBlob;
    });
    if (!data) return;
    try {
      // WebKit can lose an IndexedDB Blob's backing file after a reload. Store
      // bytes instead, and recover old Blob entries without trusting their URL.
      const content =
        "content" in data ? data.content : await data.blob.arrayBuffer();
      const contentType =
        "contentType" in data ? data.contentType : data.blob.type;
      return {
        blob: new Blob([content], { type: contentType }),
        checkedAt: data.checkedAt,
        refreshedAt: data.refreshedAt,
        etag: data.etag,
      };
    } catch {
      // Fetch the hub's cached bytes again if a legacy Blob is no longer readable.
      await this.enqueue(() =>
        this.transaction(names, "readwrite", (tx) => {
          tx.objectStore(kind).delete(key);
          tx.objectStore(metaName).delete(key);
        }),
      );
      return;
    }
  }
  saveBlob(
    kind: BlobStore,
    account: string,
    id: string,
    data: SavedBlob,
    messageId?: string,
  ) {
    if (data.blob.size > this.mediaBudget) return;
    void this.enqueue(async () => {
      const stored: StoredBlob = {
        content: await data.blob.arrayBuffer(),
        contentType: data.blob.type,
        checkedAt: data.checkedAt,
        refreshedAt: data.refreshedAt,
        etag: data.etag,
      };
      const row: BlobMetadata = {
        key: this.key(account, id),
        scope: this.scope(account),
        lastUsed: this.now(),
        bytes: data.blob.size,
        messageId,
      };
      return this.transaction(
        ["avatars", "avatarMeta", "media", "mediaMeta"],
        "readwrite",
        async (tx) => {
          const payloadStore = tx.objectStore(kind);
          const payloadMeta = tx.objectStore(metaStore(kind));
          payloadStore.put({
            key: row.key,
            scope: row.scope,
            data: stored,
            ...(messageId !== undefined ? { messageId } : {}),
          } satisfies BlobRow);
          payloadMeta.put(row);
          const entries = (
            await Promise.all(
              blobStores.map(async (blobKind) => {
                const meta = tx.objectStore(metaStore(blobKind));
                return (await request<BlobMetadata[]>(meta.getAll())).map(
                  (entry) => ({ kind: blobKind, row: entry }),
                );
              }),
            )
          ).flat();
          let bytes = entries.reduce(
            (sum, item) => sum + (item.row.bytes ?? 0),
            0,
          );
          for (const { kind: entryKind, row: entry } of entries.sort(
            (a, b) => a.row.lastUsed - b.row.lastUsed,
          )) {
            if (bytes <= this.mediaBudget) break;
            tx.objectStore(entryKind).delete(entry.key);
            tx.objectStore(metaStore(entryKind)).delete(entry.key);
            bytes -= entry.bytes ?? 0;
          }
        },
      );
    });
  }
  invalidateMessage(account: string, message: string) {
    void this.enqueue(() =>
      this.transaction(["media", "mediaMeta"], "readwrite", async (tx) => {
        const meta = tx.objectStore("mediaMeta");
        const rows = await this.rows<BlobMetadata>(tx, "mediaMeta", account);
        for (const row of rows) {
          if (row.messageId !== message) continue;
          tx.objectStore("media").delete(row.key);
          meta.delete(row.key);
        }
      }),
    );
  }
  onInvalidated(listener: (account: string) => void) {
    this.invalidations.add(listener);
    return () => {
      this.invalidations.delete(listener);
    };
  }
  async clearAccount(account: string, notify = true) {
    await this.enqueue(() =>
      this.transaction(fetched, "readwrite", async (tx) => {
        for (const name of fetched) await this.removeScope(tx, name, account);
      }),
    );
    if (notify) this.channel?.postMessage({ kind: "invalidate", account });
  }
  async clearHistory(account: string) {
    const history: Store[] = [
      "threads",
      "contacts",
      "messages",
      "chatState",
      "syncState",
    ];
    await this.enqueue(() =>
      this.transaction(history, "readwrite", async (tx) => {
        for (const name of history) await this.removeScope(tx, name, account);
      }),
    );
    this.channel?.postMessage({ kind: "invalidate", account });
  }
  async stats(account: string): Promise<{
    bytes: number;
    messages: number;
    chats: number;
    available: boolean;
  }> {
    await this.flush();
    const result = await this.transaction(
      fetchedStats,
      "readonly",
      async (tx) => {
        const rows = await Promise.all(
          fetchedStats.map((name) =>
            this.rows<Pick<Row, "bytes">>(tx, name, account),
          ),
        );
        return {
          bytes: rows.reduce(
            (sum, list) =>
              sum + list.reduce((count, row) => count + (row.bytes ?? 0), 0),
            0,
          ),
          messages: rows[2].length,
          chats: rows[3].length,
          available: true,
        };
      },
    );
    return result ?? { bytes: 0, messages: 0, chats: 0, available: false };
  }
  private preferenceKey(id: string) {
    return `agentwechat-view:${JSON.stringify([...this.namespace, id])}`;
  }
  preference<T>(id: string): T | undefined {
    try {
      return (
        JSON.parse(this.storage?.getItem(this.preferenceKey(id)) ?? "null") ??
        undefined
      );
    } catch {
      return;
    }
  }
  setPreference(id: string, data: unknown) {
    try {
      if (data == null) this.storage?.removeItem(this.preferenceKey(id));
      else this.storage?.setItem(this.preferenceKey(id), JSON.stringify(data));
    } catch {}
  }
  lastAccount(): string | undefined {
    const value = this.preference<unknown>("account");
    return typeof value === "string" ? value : undefined;
  }
  rememberAccount(account: string | null) {
    this.setPreference("account", account);
  }
  view(account: string): ViewState {
    return this.preference<ViewState>(`view:${account}`) ?? {};
  }
  saveView(account: string, patch: ViewState) {
    this.setPreference(`view:${account}`, { ...this.view(account), ...patch });
  }
  async close() {
    this.closing = true;
    this.cancelBlockedOpen?.();
    await this.flush();
    this.closed = true;
    this.cancelOpen?.();
    this.channel?.close();
    (await this.database)?.close();
  }
}
