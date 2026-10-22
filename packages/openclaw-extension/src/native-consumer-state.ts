import { createHash, randomUUID } from "node:crypto";
import { closeSync, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
import { nativeRecallIdentity, nativeRecallInfo, recalledNativeMessage, validateNativeRecall, type NativeRecallObservation } from "./native-recall-state.ts";
export type NativeSuppliedAsset = Readonly<{
  id: string; sha256: string; byteLength: number; mime: string; path: string;
  role: "model_image" | "file_reference"; kind: "image" | "audio" | "video" | "file";
  runtimeMedia: boolean; contextRole: "trigger" | "history"; messageId?: string;
  /** Actual stored message owning the bytes, when this input quotes it. */
  sourceMessageId?: string;
  sourceSha256?: string; sourceByteLength?: number; sourceMime?: string; sourcePath?: string;
}>;
export type NativeDispatchManifest = Readonly<{
  schemaVersion: 2; runId: string; accountId: string; messageIds: readonly string[]; messageSid: string;
  threadId: string; senderId: string; sessionKey: string; agentId: string; channelAccountId: string; inputBodySha256: string;
  newInputs: readonly Readonly<{ id: string; version: string; contentSha256: string; role: "trigger" }>[];
  historyInputs?: readonly Readonly<{ id: string; version: string; contentSha256: string; role: "history" }>[];
  assets: readonly NativeSuppliedAsset[]; policySha256: string; manifestSha256: string; hostSourceTurnId: string;
}>;
export type NativeCustodyItem = {
  message: NativeMessage;
  /** Fences asynchronous work when a source-local ID names a later occupant. */
  revision?: string;
  origin: string;
  state: "received" | "dispatching" | "handed_off" | "completed" | "ignored" | "needs_review";
  runId?: string;
  manifestHash?: string;
  readPending?: boolean;
  phoneReadState?: "accepted" | "rejected" | "needs_review";
  handoffUnconfirmed?: boolean;
  dispatchManifest?: NativeDispatchManifest;
  /** Ingress order is independent of timestamps and opaque server message IDs. */
  cursor?: string;
  groupContext?: boolean;
  invalidatedByRecall?: boolean;
};
type State = { schemaVersion: 1; serverId: string; accountId: string; cursor: string; items: Record<string, NativeCustodyItem>; recalls?: Record<string, NativeRecallObservation> };
export class NativeConsumerState {
  readonly file: string;
  private value: State;
  constructor(directory: string, serverId: string, accountId: string) {
    if (!isAbsolute(directory) || !serverId || !accountId) throw new Error("Native consumer state requires absolute storage and bound server/account identities");
    this.file = join(directory, `${createHash("sha256").update(`${serverId}\0${accountId}`).digest("hex")}.json`);
    this.value = { schemaVersion: 1, serverId, accountId, cursor: "0", items: {} };
    if (existsSync(this.file)) {
      const stat = lstatSync(this.file);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 16 * 1024 * 1024 || (stat.mode & 0o077)) throw new Error("Native consumer state is not a bounded private regular file");
      const parsed = JSON.parse(readFileSync(this.file, "utf8")) as State;
      if (parsed.schemaVersion !== 1 || parsed.serverId !== serverId || parsed.accountId !== accountId || !/^\d+$/.test(parsed.cursor) || !parsed.items || typeof parsed.items !== "object") throw new Error("Native consumer identity or schema mismatch; state was preserved");
      for (const [id, item] of Object.entries(parsed.items)) {
        if (item.message?.id !== id || item.message.accountId !== accountId || typeof item.message.threadId !== "string" || !["received", "dispatching", "handed_off", "completed", "ignored", "needs_review"].includes(item.state)) throw new Error("Invalid native custody record; state was preserved");
        if ((item.cursor != null && !/^\d+$/.test(item.cursor)) || (item.groupContext != null && typeof item.groupContext !== "boolean")) throw new Error("Invalid native group context record; state was preserved");
        if (item.revision != null && (typeof item.revision !== "string" || !item.revision || item.revision.length > 64)) throw Error("Invalid native custody revision; state was preserved");
        item.revision ??= randomUUID();
        if (item.groupContext && (!item.cursor || item.origin !== "live" || item.state !== "ignored" || item.message.direction !== "incoming" || !item.message.threadId.endsWith("@chatroom"))) throw new Error("Invalid native buffered group context; state was preserved");
        if (item.state === "dispatching") item.state = "needs_review";
      }
      if (parsed.recalls != null && (!parsed.recalls || typeof parsed.recalls !== "object" || Array.isArray(parsed.recalls))) throw Error("Invalid native recall state; state was preserved");
      for (const [key, recall] of Object.entries(parsed.recalls ?? {})) {
        validateNativeRecall(recall);
        if (key !== nativeRecallIdentity(recall.threadId, recall.serverId)) throw Error("Invalid native recall identity; state was preserved");
      }
      this.value = parsed;
      this.persist();
    }
  }
  get cursor(): string { return this.value.cursor; }
  get accountId(): string { return this.value.accountId; }
  items(): NativeCustodyItem[] { return Object.values(this.value.items); }
  get(messageId: string): NativeCustodyItem | undefined { return this.value.items[messageId]; }
  isRecalled(message: NativeMessage): boolean { return !!nativeRecallInfo(message) || !!(message.serverId && this.value.recalls?.[nativeRecallIdentity(message.threadId, message.serverId)]); }
  isCurrent(message: NativeMessage, revision?: string): boolean {
    const current = this.get(message.id);
    return !!current && (revision === undefined || current.revision === revision) && !this.isRecalled(message) && JSON.stringify(current.message) === JSON.stringify(message);
  }
  private persist(): void {
    const directory = dirname(this.file);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const stat = lstatSync(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077)) throw new Error("Native consumer directory must be private (0700)");
    const bytes = JSON.stringify(this.value);
    if (Buffer.byteLength(bytes) > 16 * 1024 * 1024) throw new Error("Native custody state is full; intake paused without advancing acknowledgement");
    const temporary = `${this.file}.${randomUUID()}.tmp`;
    let fd: number | undefined;
    try {
      fd = openSync(temporary, "wx", 0o600); writeFileSync(fd, bytes); fsyncSync(fd); closeSync(fd); fd = undefined;
      renameSync(temporary, this.file);
      const directoryFd = openSync(directory, "r"); try { fsyncSync(directoryFd); } finally { closeSync(directoryFd); }
    } finally { if (fd !== undefined) closeSync(fd); try { unlinkSync(temporary); } catch {} }
  }
  private mutate(change: () => void): void {
    const prior = structuredClone(this.value);
    try { change(); this.persist(); } catch (error) { this.value = prior; throw error; }
  }
  accept(cursor: string, message?: NativeMessage, origin = "unknown"): boolean {
    if (!/^\d+$/.test(cursor) || BigInt(cursor) < BigInt(this.value.cursor)) throw new Error("Native journal cursor cannot move backward");
    if (message && (!message.id || message.accountId !== this.value.accountId)) throw new Error("Native journal message crosses account boundary");
    let replaced = false;
    this.mutate(() => {
    if (message?.serverId && nativeRecallInfo(message)) this.rememberRecall({ messageId: message.id, threadId: message.threadId, serverId: message.serverId, ...nativeRecallInfo(message)! });
    if (message?.serverId) {
      const recall = this.value.recalls?.[nativeRecallIdentity(message.threadId, message.serverId)];
      if (recall) message = recalledNativeMessage(message, recall);
    }
    const existing = message ? this.value.items[message.id] : undefined;
    // A late replay of an already recalled exact target must not displace the
    // different occupant now retained under the same source-local ID.
    replaced = !!message && !nativeRecallInfo(message) && !!existing && (existing.message.threadId !== message.threadId || existing.message.serverId !== message.serverId);
    if (message && (!existing || replaced)) {
      const completed = Object.entries(this.value.items).filter(([, item]) => !item.readPending && !item.groupContext
        && item.phoneReadState !== "needs_review" && item.phoneReadState !== "rejected"
        && ["completed", "ignored"].includes(item.state));
      for (const [key] of completed.slice(0, Math.max(0, completed.length - 1000))) delete this.value.items[key];
      if (!existing && Object.keys(this.value.items).length >= 5000) throw new Error("Native custody queue is full; operator review required");
      if (replaced) {
        for (const item of this.items()) {
          const manifest = item.dispatchManifest;
          if (!manifest || ![...manifest.messageIds, ...(manifest.historyInputs ?? []).map(input => input.id),
            ...manifest.assets.flatMap(asset => asset.sourceMessageId ? [asset.sourceMessageId] : [])].includes(message.id)) continue;
          if (["dispatching", "handed_off"].includes(item.state)) item.state = "needs_review";
          item.dispatchManifest = undefined; item.manifestHash = undefined; item.readPending = false;
        }
      }
      this.value.items[message.id] = { message, revision: randomUUID(), origin, state: nativeRecallInfo(message) ? "ignored" : "received", cursor };
    }
    this.value.cursor = cursor;
    });
    return replaced;
  }
  /** Persist invalidation and the event cursor together, before consumer ACK. */
  acceptRecall(cursor: string, recall: NativeRecallObservation): string[] {
    validateNativeRecall(recall);
    if (!/^\d+$/.test(cursor) || BigInt(cursor) < BigInt(this.value.cursor)) throw Error("Native journal cursor cannot move backward");
    let affected: string[] = [];
    this.mutate(() => { affected = this.rememberRecall(recall); this.value.cursor = cursor; });
    return affected;
  }
  private rememberRecall(recall: NativeRecallObservation): string[] {
    const hinted = recall.messageId ? this.get(recall.messageId)?.message : undefined;
    if (hinted && (hinted.threadId !== recall.threadId || hinted.serverId !== recall.serverId)) {
      // A source-local opaque ID may now name another row. The exact server
      // identity still carries the valid recall; discard only its stale hint.
      const { messageId: _stale, ...exact } = recall; recall = exact;
    }
    const key = nativeRecallIdentity(recall.threadId, recall.serverId);
    this.value.recalls ??= {};
    this.value.recalls[key] ??= { threadId: recall.threadId, serverId: recall.serverId, observedAt: recall.observedAt,
      ...(recall.messageId ? { messageId: recall.messageId } : {}), ...(recall.recalledAt ? { recalledAt: recall.recalledAt } : {}) };
    const affected = new Set<string>();
    for (const item of this.items()) {
      if (item.message.threadId !== recall.threadId) continue;
      const target = item.message.serverId === recall.serverId;
      const quote = item.message.quote?.serverId === recall.serverId;
      if (!target && !quote) continue;
      affected.add(item.message.id);
      item.message = target ? recalledNativeMessage(item.message, recall) : { ...item.message, quote: undefined };
      if (target) item.groupContext = false;
      item.readPending = false;
      if (["dispatching", "handed_off"].includes(item.state)) item.state = "needs_review";
      else if (target && item.state === "received") item.state = "ignored";
      item.invalidatedByRecall = true;
      item.dispatchManifest = undefined; item.manifestHash = undefined;
    }
    for (const item of this.items()) {
      const manifest = item.dispatchManifest;
      if (item.message.threadId !== recall.threadId || !manifest) continue;
      const references = [...manifest.messageIds, ...(manifest.historyInputs ?? []).map(input => input.id),
        ...manifest.assets.flatMap(asset => asset.sourceMessageId ? [asset.sourceMessageId] : [])];
      if (!references.some(id => affected.has(id) || id === recall.messageId)) continue;
      affected.add(item.message.id);
      item.invalidatedByRecall = true; item.readPending = false;
      if (["dispatching", "handed_off"].includes(item.state)) item.state = "needs_review";
      item.dispatchManifest = undefined; item.manifestHash = undefined;
    }
    return [...affected];
  }
  update(messageId: string, update: Partial<Omit<NativeCustodyItem, "message" | "origin" | "revision">>, revision?: string): boolean {
    const item = this.value.items[messageId];
    if (revision !== undefined && item?.revision !== revision) return false;
    if (!item) throw new Error("Unknown native custody message");
    this.mutate(() => Object.assign(this.value.items[messageId], update));
    return true;
  }
  groupHistory(threadId: string, beforeId: string, limit: number): NativeCustodyItem[] {
    const before = this.value.items[beforeId]?.cursor;
    if (!before || limit <= 0) return [];
    return this.items().filter(item => item.groupContext && item.state === "ignored" && item.origin === "live"
      && item.message.threadId === threadId && item.cursor && BigInt(item.cursor) < BigInt(before))
      .sort((a, b) => BigInt(a.cursor!) < BigInt(b.cursor!) ? -1 : BigInt(a.cursor!) > BigInt(b.cursor!) ? 1 : 0).slice(-limit);
  }
  bufferGroupMessage(messageId: string, limit: number, revision?: string): boolean {
    const item = this.value.items[messageId];
    if (revision !== undefined && item?.revision !== revision) return false;
    if (!item || item.state !== "received" || item.origin !== "live" || item.message.direction !== "incoming" || !item.cursor || !item.message.threadId.endsWith("@chatroom") || !Number.isInteger(limit) || limit < 0 || limit > 200) throw new Error("Invalid native group history admission");
    this.mutate(() => {
      Object.assign(this.value.items[messageId], { state: "ignored", groupContext: limit > 0 });
      const history = this.items().filter(value => value.groupContext).sort((a, b) => BigInt(a.cursor!) < BigInt(b.cursor!) ? -1 : 1);
      const thread = history.filter(value => value.message.threadId === item.message.threadId);
      for (const value of thread.slice(0, Math.max(0, thread.length - limit))) value.groupContext = false;
      const retained = history.filter(value => value.groupContext);
      for (const value of retained.slice(0, Math.max(0, retained.length - 1000))) value.groupContext = false;
    });
    return true;
  }
  beginDispatch(messageId: string, dispatchManifest: NativeDispatchManifest, historyIds: string[] = []): void {
    if (this.value.items[messageId]?.state !== "received" || this.isRecalled(this.value.items[messageId].message) || historyIds.some(id => !this.value.items[id]?.groupContext
      || this.value.items[id].message.threadId !== this.value.items[messageId].message.threadId)) throw new Error("Invalid native context dispatch custody");
    // Offering context and recording the uncertain turn are one durable write.
    // A crash must not replay that turn or offer its context to another turn.
    this.mutate(() => {
      Object.assign(this.value.items[messageId], { state: "dispatching", dispatchManifest, invalidatedByRecall: false });
      for (const id of historyIds) this.value.items[id].groupContext = false;
    });
  }
  /** Synchronous bounded receipt at the SDK's prepared-run callback boundary. */
  preparedCallback(messageIds: string[], readPolicy: "never" | "on_model_dispatch", previous?: (...args: any[]) => unknown): (...args: any[]) => unknown {
    if (!messageIds.length || messageIds.length > 1000) throw new Error("Invalid native prepared input manifest");
    const exact = [...new Set(messageIds)];
    const items = exact.map(id => { const item = this.value.items[id]; if (!item || item.state !== "dispatching") throw new Error("Prepared input manifest is not in dispatch custody"); return item; });
    const revisions = items.map(item => item.revision);
    if (new Set(items.map(item => item.message.threadId)).size !== 1) throw new Error("Prepared input manifest crosses threads");
    const manifestHash = createHash("sha256").update(JSON.stringify({ accountId: this.value.accountId, messageIds: exact,
      dispatchManifests: items.map(item => item.dispatchManifest ?? null) })).digest("hex");
    return (...args: any[]) => {
      const runId = typeof args[0] === "string" ? args[0] : args[0]?.runId;
      if (typeof runId !== "string" || !runId || runId.length > 256) throw new Error("Prepared run callback has no valid runId");
      this.mutate(() => {
      const currentHash = createHash("sha256").update(JSON.stringify({ accountId: this.value.accountId, messageIds: exact,
        dispatchManifests: exact.map(id => this.value.items[id]?.dispatchManifest ?? null) })).digest("hex");
      for (const [index, id] of exact.entries()) {
        const item = this.value.items[id];
        if (!item || item.invalidatedByRecall || this.isRecalled(item.message)) throw Error("Native prepared input was invalidated by a recall");
        if (item.revision !== revisions[index] || currentHash !== manifestHash) throw Error("Native prepared input source identity changed");
        if (item.runId && item.runId !== runId) throw new Error("Native prepared manifest was assigned to another run; operator review required");
        Object.assign(item, { state: "handed_off", runId, manifestHash, readPending: readPolicy === "on_model_dispatch", handoffUnconfirmed: false });
      }
      });
      return previous?.(...args);
    };
  }
}
