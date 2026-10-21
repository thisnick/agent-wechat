import { createHash, randomUUID } from "node:crypto";
import { closeSync, existsSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, isAbsolute, join } from "node:path";
import type { NativeMessage } from "@agent-wechat/shared";
export type NativeSuppliedAsset = Readonly<{
  id: string; sha256: string; byteLength: number; mime: string; path: string;
  role: "model_image" | "file_reference"; kind: "image" | "audio" | "video" | "file";
  runtimeMedia: boolean; contextRole: "trigger";
  sourceSha256?: string; sourceByteLength?: number; sourceMime?: string; sourcePath?: string;
}>;
export type NativeDispatchManifest = Readonly<{
  schemaVersion: 2; runId: string; accountId: string; messageIds: readonly string[]; messageSid: string;
  threadId: string; senderId: string; sessionKey: string; agentId: string; channelAccountId: string; inputBodySha256: string;
  newInputs: readonly Readonly<{ id: string; version: string; contentSha256: string; role: "trigger" }>[];
  assets: readonly NativeSuppliedAsset[]; policySha256: string; manifestSha256: string; hostSourceTurnId: string;
}>;
export type NativeCustodyItem = {
  message: NativeMessage;
  origin: string;
  state: "received" | "dispatching" | "handed_off" | "completed" | "ignored" | "needs_review";
  runId?: string;
  manifestHash?: string;
  readPending?: boolean;
  handoffUnconfirmed?: boolean;
  dispatchManifest?: NativeDispatchManifest;
};
type State = { schemaVersion: 1; serverId: string; accountId: string; cursor: string; items: Record<string, NativeCustodyItem> };
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
        if (item.state === "dispatching") item.state = "needs_review";
      }
      this.value = parsed;
      this.persist();
    }
  }
  get cursor(): string { return this.value.cursor; }
  items(): NativeCustodyItem[] { return Object.values(this.value.items); }
  get(messageId: string): NativeCustodyItem | undefined { return this.value.items[messageId]; }
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
  accept(cursor: string, message?: NativeMessage, origin = "unknown"): void {
    if (!/^\d+$/.test(cursor) || BigInt(cursor) < BigInt(this.value.cursor)) throw new Error("Native journal cursor cannot move backward");
    if (message && (!message.id || message.accountId !== this.value.accountId)) throw new Error("Native journal message crosses account boundary");
    this.mutate(() => {
    if (message && !this.value.items[message.id]) {
      const completed = Object.entries(this.value.items).filter(([, item]) => !item.readPending && ["completed", "ignored"].includes(item.state));
      for (const [key] of completed.slice(0, Math.max(0, completed.length - 1000))) delete this.value.items[key];
      if (Object.keys(this.value.items).length >= 5000) throw new Error("Native custody queue is full; operator review required");
      this.value.items[message.id] = { message, origin, state: "received" };
    }
    this.value.cursor = cursor;
    });
  }
  update(messageId: string, update: Partial<Omit<NativeCustodyItem, "message" | "origin">>): void {
    const item = this.value.items[messageId]; if (!item) throw new Error("Unknown native custody message");
    this.mutate(() => Object.assign(this.value.items[messageId], update));
  }
  /** Synchronous bounded receipt at the SDK's prepared-run callback boundary. */
  preparedCallback(messageIds: string[], readPolicy: "never" | "on_model_dispatch", previous?: (...args: any[]) => unknown): (...args: any[]) => unknown {
    if (!messageIds.length || messageIds.length > 1000) throw new Error("Invalid native prepared input manifest");
    const exact = [...new Set(messageIds)];
    const items = exact.map(id => { const item = this.value.items[id]; if (!item || item.state !== "dispatching") throw new Error("Prepared input manifest is not in dispatch custody"); return item; });
    if (new Set(items.map(item => item.message.threadId)).size !== 1) throw new Error("Prepared input manifest crosses threads");
    const manifestHash = createHash("sha256").update(JSON.stringify({ accountId: this.value.accountId, messageIds: exact,
      dispatchManifests: items.map(item => item.dispatchManifest ?? null) })).digest("hex");
    return (...args: any[]) => {
      const runId = typeof args[0] === "string" ? args[0] : args[0]?.runId;
      if (typeof runId !== "string" || !runId || runId.length > 256) throw new Error("Prepared run callback has no valid runId");
      this.mutate(() => {
      for (const id of exact) {
        const item = this.value.items[id];
        if (item.runId && item.runId !== runId) throw new Error("Native prepared manifest was assigned to another run; operator review required");
        Object.assign(item, { state: "handed_off", runId, manifestHash, readPending: readPolicy === "on_model_dispatch", handoffUnconfirmed: false });
      }
      });
      return previous?.(...args);
    };
  }
}
