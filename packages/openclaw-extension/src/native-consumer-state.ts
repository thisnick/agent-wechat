import { createHash, randomUUID } from "node:crypto";
import { closeSync, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
import { nativeRecallIdentity, nativeRecallInfo, validateNativeRecall, type NativeRecallObservation } from "./native-recall-state.ts";
export type NativeSuppliedAsset = Readonly<{
  id: string; sha256: string; byteLength: number; mime: string; path: string;
  role: "model_image" | "file_reference"; kind: "image" | "audio" | "video" | "file";
  runtimeMedia: boolean; contextRole: "trigger" | "history"; messageId?: string;
  /** Attachment name shown to the agent with the file. */
  fileName?: string;
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
/**
 * A message taken in but not finished. Only its identity is stored: the
 * message itself stays in the official profile and is read from the daemon
 * when it is processed. Finished messages are removed; the event cursor alone
 * records that they were seen.
 */
export type NativeOpenItem = {
  id: string; threadId: string; serverId: string | null;
  /** Server ID of the message this one quotes, so recalling it cancels queued work. */
  quoteServerId?: string;
  origin: string;
  /** Event cursor that committed the message. */
  cursor: string;
  /** Fences asynchronous work when a source-local ID names a later occupant. */
  revision: string;
  /** `awaiting_handoff`: the reply finished but the host has not confirmed the model run. */
  state: "received" | "dispatching" | "handed_off" | "awaiting_handoff";
  runId?: string;
  manifestHash?: string;
  dispatchManifest?: NativeDispatchManifest;
  /** Server IDs of the quoted and group context messages offered with this turn. */
  inputServerIds?: string[];
  invalidatedByRecall?: boolean;
  handoffUnconfirmed?: boolean;
};
/** A thread that still needs a read receipt, and the messages that asked for it. */
type ReadRequest = { token: string; messages: Array<{ id: string; serverId: string | null }> };
/** The newest group message handed to the model; earlier messages are not offered as context again. */
export type NativeGroupBoundary = { cursor: string; messageId?: string; createdAt?: string };
type State = { schemaVersion: 2; serverId: string; accountId: string; cursor: string;
  open: Record<string, NativeOpenItem>; reads: Record<string, ReadRequest>; boundaries: Record<string, NativeGroupBoundary> };

/** A missed message is answered only if it was sent and reached the daemon within this window. */
export const NATIVE_CATCH_UP_WINDOW_MS = 24 * 60 * 60 * 1000;
/** One catch-up pass answers at most this many missed messages, the newest ones. */
export const NATIVE_CATCH_UP_LIMIT = 100;
const OPEN_LIMIT = 5000;
const AWAITING_HANDOFF_LIMIT = 100;
const BOUNDARY_LIMIT = 5000;
const READ_MESSAGE_LIMIT = 100;
const RECALL_MEMORY_LIMIT = 10000;
const OPEN_STATES = ["received", "dispatching", "handed_off", "awaiting_handoff"];
const decimal = (value: unknown): value is string => typeof value === "string" && /^\d+$/.test(value);
const optionalServerId = (value: unknown) => value === null || (typeof value === "string" && /^\d+$/.test(value));

export class NativeConsumerState {
  readonly file: string;
  /** Messages whose turn or reply was in progress when the consumer last stopped. They are not retried. */
  readonly heldOnOpen: string[] = [];
  /** Messages not queued since the last report: older than the catch-up window, beyond the catch-up limit, or the queue was full. */
  skipped = { old: 0, limit: 0, full: 0 };
  private value: State;
  private dirty = false;
  /** Recalls seen while running. Messages read later from the daemon carry their own recall state. */
  private readonly recalls = new Set<string>();
  constructor(directory: string, serverId: string, accountId: string, options: { now?: number } = {}) {
    if (!isAbsolute(directory) || !serverId || !accountId) throw new Error("Native consumer state requires absolute storage and bound server/account identities");
    this.file = join(directory, `${createHash("sha256").update(`${serverId}\0${accountId}`).digest("hex")}.json`);
    this.value = { schemaVersion: 2, serverId, accountId, cursor: "0", open: {}, reads: {}, boundaries: {} };
    if (existsSync(this.file)) {
      const stat = lstatSync(this.file);
      if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 16 * 1024 * 1024 || (stat.mode & 0o077)) throw new Error("Native consumer state is not a bounded private regular file");
      const bytes = readFileSync(this.file, "utf8");
      const parsed = JSON.parse(bytes);
      if (parsed?.serverId !== serverId || parsed.accountId !== accountId || !decimal(parsed.cursor)) throw new Error("Native consumer identity or schema mismatch; state was preserved");
      if (parsed.schemaVersion === 1) {
        this.value = migrateVersion1(parsed, this.heldOnOpen, this.skipped, options.now ?? Date.now());
        // The conversion drops data, so the earlier release's file is kept once beside the new one.
        if (!existsSync(`${this.file}.v1-backup`)) this.write(`${this.file}.v1-backup`, bytes);
      } else if (parsed.schemaVersion === 2) this.value = loadVersion2(parsed, this.heldOnOpen);
      else throw new Error("Native consumer identity or schema mismatch; state was preserved");
      this.persist();
    }
  }
  get cursor(): string { return this.value.cursor; }
  get accountId(): string { return this.value.accountId; }
  items(): NativeOpenItem[] { return Object.values(this.value.open); }
  get(messageId: string): NativeOpenItem | undefined { return this.value.open[messageId]; }
  isRecalled(message: Pick<NativeMessage, "threadId" | "serverId"> & { recall?: NativeMessage["recall"] }): boolean {
    return !!(message.recall && nativeRecallInfo(message as NativeMessage)) || this.recalledIdentity(message.threadId, message.serverId);
  }
  /** A queued message can still be handled by the work that holds this revision. */
  isCurrent(message: NativeMessage, revision?: string): boolean {
    const item = this.get(message.id);
    return !!item && (revision === undefined || item.revision === revision) && item.threadId === message.threadId
      && item.serverId === (message.serverId ?? null) && !item.invalidatedByRecall && !this.isRecalled(message);
  }
  /** A context message is still usable: neither it nor the quote it carries was recalled. */
  isAvailable(message: NativeMessage): boolean {
    return !this.isRecalled(message) && !(message.quote && this.recalledIdentity(message.threadId, message.quote.serverId));
  }
  private recalledIdentity(threadId: string, serverId: string | null | undefined): boolean {
    return !!serverId && this.recalls.has(nativeRecallIdentity(threadId, serverId));
  }
  private persist(): void {
    const bytes = JSON.stringify(this.value);
    if (Buffer.byteLength(bytes) > 16 * 1024 * 1024) throw new Error("Native custody state is full; intake paused without advancing acknowledgement");
    this.write(this.file, bytes);
    this.dirty = false;
  }
  /** Replace a private file in the consumer directory in one durable step. */
  private write(file: string, bytes: string): void {
    const directory = dirname(file);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const stat = lstatSync(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink() || (stat.mode & 0o077)) throw new Error("Native consumer directory must be private (0700)");
    const temporary = `${file}.${randomUUID()}.tmp`;
    let fd: number | undefined;
    try {
      fd = openSync(temporary, "wx", 0o600); writeFileSync(fd, bytes); fsyncSync(fd); closeSync(fd); fd = undefined;
      renameSync(temporary, file);
      const directoryFd = openSync(directory, "r"); try { fsyncSync(directoryFd); } finally { closeSync(directoryFd); }
    } finally { if (fd !== undefined) closeSync(fd); try { unlinkSync(temporary); } catch {} }
  }
  private mutate(change: () => void): void {
    const prior = structuredClone(this.value);
    try { change(); this.persist(); } catch (error) { this.value = prior; throw error; }
  }
  private checkCursor(cursor: string): boolean {
    if (!decimal(cursor) || BigInt(cursor) < BigInt(this.value.cursor)) throw new Error("Native journal cursor cannot move backward");
    return BigInt(cursor) > BigInt(this.value.cursor);
  }
  /** Move past events that need no work. Written with the next change or {@link commit}. */
  advance(cursor: string): void {
    if (this.checkCursor(cursor)) { this.value.cursor = cursor; this.dirty = true; }
  }
  /** Write an advanced cursor before it is acknowledged to the daemon. */
  commit(): void { if (this.dirty) this.persist(); }
  /** Queue a live incoming message. Returns true when it replaces another message under the same source-local ID. */
  accept(cursor: string, message: NativeMessage, origin = "unknown"): boolean {
    if (!this.checkCursor(cursor)) return false;
    if (!message?.id || message.accountId !== this.value.accountId || typeof message.threadId !== "string" || !message.threadId) throw new Error("Native journal message crosses account boundary");
    let replaced = false;
    this.mutate(() => {
      this.value.cursor = cursor;
      // A late replay of a recalled target must not displace a different occupant.
      if (this.isRecalled(message)) return;
      const existing = this.value.open[message.id];
      const serverId = message.serverId ?? null;
      if (existing && existing.threadId === message.threadId && existing.serverId === serverId) return;
      replaced = !!existing;
      if (replaced) this.revokeReferences(message.id);
      else if (Object.keys(this.value.open).length >= OPEN_LIMIT) { this.skipped.full++; return; }
      this.value.open[message.id] = { id: message.id, threadId: message.threadId, serverId,
        ...(message.quote?.serverId ? { quoteServerId: message.quote.serverId } : {}),
        origin, cursor, revision: randomUUID(), state: "received" };
    });
    return replaced;
  }
  /** Turns that offered a replaced message lose their manifest, so a late model callback cannot confirm them. */
  private revokeReferences(messageId: string): void {
    for (const item of this.items()) {
      if (!item.dispatchManifest || !manifestReferences(item.dispatchManifest).includes(messageId)) continue;
      if (item.state === "awaiting_handoff") { delete this.value.open[item.id]; continue; }
      item.dispatchManifest = undefined; item.manifestHash = undefined;
    }
  }
  /** Apply a recall event and advance the cursor in one write, before acknowledgement. */
  acceptRecall(cursor: string, recall: NativeRecallObservation): string[] {
    validateNativeRecall(recall);
    if (!this.checkCursor(cursor)) return [];
    let affected: string[] = [];
    this.mutate(() => { affected = this.applyRecall(recall); this.value.cursor = cursor; });
    return affected;
  }
  /** Apply a recall found by re-reading a message. */
  noteRecall(recall: NativeRecallObservation): string[] {
    validateNativeRecall(recall);
    let affected: string[] = [];
    this.mutate(() => { affected = this.applyRecall(recall); });
    return affected;
  }
  private applyRecall(recall: NativeRecallObservation): string[] {
    const key = nativeRecallIdentity(recall.threadId, recall.serverId);
    this.recalls.delete(key); this.recalls.add(key);
    for (const old of this.recalls) { if (this.recalls.size <= RECALL_MEMORY_LIMIT) break; this.recalls.delete(old); }
    // A source-local opaque ID may now name another row. The exact server
    // identity still carries the recall; discard only its stale hint.
    const hinted = recall.messageId ? this.value.open[recall.messageId] : undefined;
    const hint = recall.messageId && (!hinted || (hinted.threadId === recall.threadId && hinted.serverId === recall.serverId)) ? recall.messageId : undefined;
    const affected = new Set<string>(hint ? [hint] : []);
    for (const item of this.items()) {
      if (item.threadId !== recall.threadId) continue;
      const target = item.serverId === recall.serverId;
      if (!target && item.quoteServerId !== recall.serverId && !item.inputServerIds?.includes(recall.serverId)
        && !(hint && item.dispatchManifest && manifestReferences(item.dispatchManifest).includes(hint))) continue;
      affected.add(item.id);
      if (item.state === "received") {
        // A recalled target never runs. A quoting message is read again, with its quote removed.
        if (target) delete this.value.open[item.id]; else item.revision = randomUUID();
      } else if (item.state === "awaiting_handoff") delete this.value.open[item.id];
      else { item.invalidatedByRecall = true; item.dispatchManifest = undefined; item.manifestHash = undefined; }
    }
    const read = this.value.reads[recall.threadId];
    if (read) {
      read.messages = read.messages.filter(message => message.serverId !== recall.serverId && !affected.has(message.id));
      if (!read.messages.length) delete this.value.reads[recall.threadId];
    }
    return [...affected];
  }
  update(messageId: string, update: Partial<Omit<NativeOpenItem, "id" | "threadId" | "serverId" | "origin" | "revision" | "cursor">>, revision?: string): boolean {
    const item = this.value.open[messageId];
    if (revision !== undefined && item?.revision !== revision) return false;
    if (!item) throw new Error("Unknown native custody message");
    this.mutate(() => Object.assign(this.value.open[messageId], update));
    return true;
  }
  /** Drop queued messages in one write, skipping any another occupant or a started turn now holds. */
  dropQueued(entries: ReadonlyArray<{ id: string; revision: string }>): number {
    const dropped = entries.filter(({ id, revision }) => this.value.open[id]?.revision === revision && this.value.open[id].state === "received");
    if (dropped.length) this.mutate(() => { for (const { id } of dropped) delete this.value.open[id]; });
    return dropped.length;
  }
  /** Remove a message that needs no more work. Ignored when another occupant now holds the ID. */
  finish(messageId: string, revision?: string): boolean {
    const item = this.value.open[messageId];
    if (!item || (revision !== undefined && item.revision !== revision)) return false;
    this.mutate(() => { delete this.value.open[messageId]; });
    return true;
  }
  /**
   * Record the turn before any model side effect. In a group, also move the
   * context boundary in the same write, so a crash cannot offer that context again.
   */
  beginDispatch(messageId: string, dispatchManifest: NativeDispatchManifest, options: { revision?: string; inputServerIds?: string[]; boundary?: { createdAt?: string | null } } = {}): void {
    const item = this.value.open[messageId];
    if (!item || item.state !== "received" || item.invalidatedByRecall || this.recalledIdentity(item.threadId, item.serverId)
      || (options.revision !== undefined && item.revision !== options.revision)) throw new Error("Invalid native context dispatch custody");
    const inputServerIds = [...new Set((options.inputServerIds ?? []).filter(id => typeof id === "string" && id))].slice(0, 1000);
    this.mutate(() => {
      Object.assign(item, { state: "dispatching", dispatchManifest, invalidatedByRecall: false, ...(inputServerIds.length ? { inputServerIds } : {}) });
      const prior = this.value.boundaries[item.threadId];
      if (options.boundary && (!prior || BigInt(item.cursor) > BigInt(prior.cursor))) {
        const createdAt = typeof options.boundary.createdAt === "string" ? options.boundary.createdAt : undefined;
        // A late-delivered mention sent before the boundary message keeps that message as the stopping point.
        const earlier = !!prior?.createdAt && !!createdAt && Date.parse(createdAt) < Date.parse(prior.createdAt);
        delete this.value.boundaries[item.threadId];
        this.value.boundaries[item.threadId] = earlier ? { ...prior!, cursor: item.cursor } : { cursor: item.cursor, messageId, ...(createdAt ? { createdAt } : {}) };
        const threads = Object.keys(this.value.boundaries);
        for (const thread of threads.slice(0, Math.max(0, threads.length - BOUNDARY_LIMIT))) delete this.value.boundaries[thread];
      }
    });
  }
  /**
   * Finish a turn. A read requested by the turn is kept until it is sent. When
   * the host has not yet confirmed the model run, a bounded record waits for its callback.
   */
  complete(messageId: string, revision: string, outcome: { failed: boolean; requestRead: boolean; awaitHandoff: boolean }): boolean {
    const item = this.value.open[messageId];
    if (!item || item.revision !== revision) return false;
    this.mutate(() => {
      if (outcome.requestRead && !outcome.failed) this.addRead(item);
      if (outcome.failed || !outcome.awaitHandoff) { delete this.value.open[messageId]; return; }
      Object.assign(item, { state: "awaiting_handoff", handoffUnconfirmed: true });
      const waiting = this.items().filter(value => value.state === "awaiting_handoff");
      for (const value of waiting.slice(0, Math.max(0, waiting.length - AWAITING_HANDOFF_LIMIT))) delete this.value.open[value.id];
    });
    return true;
  }
  private addRead(item: Pick<NativeOpenItem, "id" | "threadId" | "serverId">): void {
    const messages = (this.value.reads[item.threadId]?.messages ?? []).filter(message => message.id !== item.id);
    messages.push({ id: item.id, serverId: item.serverId });
    // A new token keeps a read started before this request from clearing it.
    this.value.reads[item.threadId] = { token: randomUUID(), messages: messages.slice(-READ_MESSAGE_LIMIT) };
  }
  /** Threads waiting for a read receipt. Pass the token back to {@link clearRead}. */
  pendingReads(): Array<{ threadId: string; token: string }> {
    return Object.entries(this.value.reads).map(([threadId, read]) => ({ threadId, token: read.token }));
  }
  readPending(threadId: string): boolean { return !!this.value.reads[threadId]; }
  /** Clear a sent read, unless another message asked for one since it was started. */
  clearRead(threadId: string, token: string): boolean {
    if (this.value.reads[threadId]?.token !== token) return false;
    this.mutate(() => { delete this.value.reads[threadId]; });
    return true;
  }
  groupBoundary(threadId: string): NativeGroupBoundary | undefined { return this.value.boundaries[threadId]; }
  /** Synchronous bounded receipt at the SDK's prepared-run callback boundary. */
  preparedCallback(messageIds: string[], readPolicy: "never" | "on_model_dispatch", previous?: (...args: any[]) => unknown): (...args: any[]) => unknown {
    if (!messageIds.length || messageIds.length > 1000) throw new Error("Invalid native prepared input manifest");
    const exact = [...new Set(messageIds)];
    const items = exact.map(id => { const item = this.value.open[id]; if (!item || item.state !== "dispatching") throw new Error("Prepared input manifest is not in dispatch custody"); return item; });
    const revisions = items.map(item => item.revision);
    if (new Set(items.map(item => item.threadId)).size !== 1) throw new Error("Prepared input manifest crosses threads");
    const hash = () => createHash("sha256").update(JSON.stringify({ accountId: this.value.accountId, messageIds: exact,
      dispatchManifests: exact.map(id => this.value.open[id]?.dispatchManifest ?? null) })).digest("hex");
    const manifestHash = hash();
    return (...args: any[]) => {
      const runId = typeof args[0] === "string" ? args[0] : args[0]?.runId;
      if (typeof runId !== "string" || !runId || runId.length > 256) throw new Error("Prepared run callback has no valid runId");
      this.mutate(() => {
        const currentHash = hash();
        for (const [index, id] of exact.entries()) {
          const item = this.value.open[id];
          if (!item) throw Error("Native prepared input is no longer in custody");
          if (item.invalidatedByRecall || this.recalledIdentity(item.threadId, item.serverId)) throw Error("Native prepared input was invalidated by a recall");
          if (item.revision !== revisions[index] || currentHash !== manifestHash) throw Error("Native prepared input source identity changed");
          if (item.runId && item.runId !== runId) throw new Error("Native prepared manifest was assigned to another run; operator review required");
          if (readPolicy === "on_model_dispatch") this.addRead(item);
          if (item.state === "awaiting_handoff") delete this.value.open[id];
          else Object.assign(item, { state: "handed_off", runId, manifestHash, handoffUnconfirmed: false });
        }
      });
      return previous?.(...args);
    };
  }
}

function manifestReferences(manifest: NativeDispatchManifest): string[] {
  return [...manifest.messageIds, ...(manifest.historyInputs ?? []).map(input => input.id),
    ...manifest.assets.flatMap(asset => asset.sourceMessageId ? [asset.sourceMessageId] : [])];
}

function loadVersion2(parsed: any, held: string[]): State {
  if (!parsed.open || typeof parsed.open !== "object" || Array.isArray(parsed.open) || !parsed.reads || typeof parsed.reads !== "object"
    || !parsed.boundaries || typeof parsed.boundaries !== "object") throw new Error("Native consumer identity or schema mismatch; state was preserved");
  const open: Record<string, NativeOpenItem> = {};
  for (const [id, item] of Object.entries(parsed.open as Record<string, NativeOpenItem>)) {
    if (item?.id !== id || typeof item.threadId !== "string" || !item.threadId || !optionalServerId(item.serverId) || !OPEN_STATES.includes(item.state)
      || !decimal(item.cursor) || typeof item.revision !== "string" || !item.revision || item.revision.length > 64 || typeof item.origin !== "string"
      || (item.quoteServerId != null && !decimal(item.quoteServerId))
      || (item.inputServerIds != null && (!Array.isArray(item.inputServerIds) || item.inputServerIds.some(value => !decimal(value))))) throw new Error("Invalid native custody record; state was preserved");
    // A turn in progress when the consumer stopped is uncertain and is never repeated.
    if (item.state === "dispatching" || item.state === "handed_off") { held.push(id); continue; }
    // Its model callback ended with the process that registered it.
    if (item.state === "awaiting_handoff") continue;
    open[id] = item;
  }
  for (const [threadId, read] of Object.entries(parsed.reads as Record<string, ReadRequest>)) {
    if (!threadId || typeof read?.token !== "string" || !read.token || !Array.isArray(read.messages)
      || read.messages.some(message => typeof message?.id !== "string" || !optionalServerId(message.serverId))) throw new Error("Invalid native read record; state was preserved");
  }
  for (const [threadId, boundary] of Object.entries(parsed.boundaries as Record<string, NativeGroupBoundary>)) {
    if (!threadId || !decimal(boundary?.cursor) || (boundary.messageId != null && typeof boundary.messageId !== "string")
      || (boundary.createdAt != null && typeof boundary.createdAt !== "string")) throw new Error("Invalid native group context record; state was preserved");
  }
  return { schemaVersion: 2, serverId: parsed.serverId, accountId: parsed.accountId, cursor: parsed.cursor, open, reads: parsed.reads, boundaries: parsed.boundaries };
}

/**
 * Version 1, written by every release before this format, kept a copy of
 * every message. Keep what version 2 stores: the cursor, queued messages the
 * catch-up rules would still answer, pending reads and group context
 * boundaries. Records it cannot use are left out rather than stopping the
 * upgrade; a record for another account still refuses the file.
 */
function migrateVersion1(parsed: any, held: string[], skipped: NativeConsumerState["skipped"], now: number): State {
  if (!parsed.items || typeof parsed.items !== "object" || Array.isArray(parsed.items)) throw new Error("Native consumer identity or schema mismatch; state was preserved");
  type V1Item = { message: NativeMessage; origin?: unknown; state: string; cursor?: string; revision?: unknown; readPending?: boolean;
    groupContext?: boolean; dispatchManifest?: NativeDispatchManifest; runId?: string };
  const items: Array<[string, V1Item]> = [];
  for (const [id, item] of Object.entries(parsed.items as Record<string, V1Item>)) {
    const message = item?.message;
    if (message && message.accountId != null && message.accountId !== parsed.accountId) throw new Error("Native consumer identity or schema mismatch; state was preserved");
    if (message?.id !== id || typeof message.threadId !== "string" || !message.threadId || typeof item.state !== "string") continue;
    items.push([id, { ...item, cursor: decimal(item.cursor) ? item.cursor : undefined }]);
  }
  const state: State = { schemaVersion: 2, serverId: parsed.serverId, accountId: parsed.accountId, cursor: parsed.cursor, open: {}, reads: {}, boundaries: {} };
  const serverId = (value: unknown) => decimal(value) ? value : null;
  const time = (item: V1Item) => item.message.createdAt ? Date.parse(item.message.createdAt) : Number.NaN;
  // Releases before the cursor field are ordered by creation time.
  const order = (a: V1Item, b: V1Item) => {
    const cursors = BigInt(a.cursor ?? "0") - BigInt(b.cursor ?? "0");
    return cursors < 0n ? -1 : cursors > 0n ? 1 : (time(a) || 0) - (time(b) || 0);
  };
  // Queued work follows the catch-up rules: only recent live incoming messages are kept, and at the
  // first start they count toward the catch-up limit only if the current policy would answer them.
  const queued = items.map(([, item]) => item).filter(item => item.state === "received" && item.origin === "live"
    && item.message.direction === "incoming" && !item.message.recall).sort(order);
  const recent = queued.filter(item => { const sent = time(item); if (Number.isFinite(sent) && sent < now - NATIVE_CATCH_UP_WINDOW_MS) { skipped.old++; return false; } return true; });
  for (const item of recent.slice(-OPEN_LIMIT)) {
    const message = item.message, quoteServerId = serverId(message.quote?.serverId);
    state.open[message.id] = { id: message.id, threadId: message.threadId, serverId: serverId(message.serverId), ...(quoteServerId ? { quoteServerId } : {}),
      origin: "live", cursor: item.cursor ?? parsed.cursor,
      revision: typeof item.revision === "string" && item.revision && item.revision.length <= 64 ? item.revision : randomUUID(), state: "received" };
  }
  for (const [id, item] of items) {
    // A turn in progress when the earlier release stopped is reported and not repeated.
    if (item.state === "dispatching" || item.state === "handed_off") held.push(id);
    if (item.readPending === true) {
      const read = state.reads[item.message.threadId] ??= { token: randomUUID(), messages: [] };
      read.messages = [...read.messages, { id, serverId: serverId(item.message.serverId) }].slice(-READ_MESSAGE_LIMIT);
    }
  }
  // Context buffered by version 1 is now read from the profile. The boundary
  // starts at the oldest buffered message, or after the newest message handed
  // to the model, so the next mention sees the same context.
  for (const threadId of new Set(items.map(([, item]) => item.message.threadId).filter(thread => thread.endsWith("@chatroom")))) {
    const inThread = items.map(([, item]) => item).filter(item => item.message.threadId === threadId).sort(order);
    const buffered = inThread.find(item => item.groupContext === true && item.state === "ignored");
    const offered = inThread.filter(item => ["dispatching", "handed_off", "completed", "needs_review"].includes(item.state)
      && (item.dispatchManifest || item.runId || item.state === "completed")).at(-1);
    const createdAt = (item: V1Item) => typeof item.message.createdAt === "string" ? { createdAt: item.message.createdAt } : {};
    if (buffered) state.boundaries[threadId] = { cursor: buffered.cursor && BigInt(buffered.cursor) > 0n ? (BigInt(buffered.cursor) - 1n).toString() : "0", ...createdAt(buffered) };
    else if (offered) state.boundaries[threadId] = { cursor: offered.cursor ?? "0", messageId: offered.message.id, ...createdAt(offered) };
  }
  return state;
}
