import { createHash, randomUUID } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

type MonitorState<T> = {
  version: 1;
  lastSeen: Record<string, number>;
  groupHistory: Record<string, T[]>;
};

const emptyState = <T>(): MonitorState<T> => ({
  version: 1,
  lastSeen: Object.create(null) as Record<string, number>,
  groupHistory: Object.create(null) as Record<string, T[]>,
});

export class MonitorStateStore<T> {
  private readonly filePath: string;

  constructor(stateDir: string, accountId: string) {
    const account = createHash("sha256").update(accountId).digest("hex").slice(0, 16);
    this.filePath = path.join(stateDir, "agent-wechat", "monitor", `${account}.json`);
  }

  async load(validateItem: (value: unknown) => value is T): Promise<MonitorState<T>> {
    let parsed: unknown;
    try {
      parsed = JSON.parse(await fs.readFile(this.filePath, "utf8"));
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return emptyState();
      throw error;
    }
    if (!parsed || typeof parsed !== "object" || (parsed as { version?: unknown }).version !== 1) {
      throw new Error("Unsupported WeChat monitor state");
    }
    const input = parsed as { lastSeen?: unknown; groupHistory?: unknown };
    const state = emptyState<T>();
    if (input.lastSeen && typeof input.lastSeen === "object") {
      for (const [chatId, localId] of Object.entries(input.lastSeen)) {
        if (typeof localId === "number" && Number.isSafeInteger(localId) && localId >= 0) {
          state.lastSeen[chatId] = localId;
        }
      }
    }
    if (input.groupHistory && typeof input.groupHistory === "object") {
      for (const [chatId, items] of Object.entries(input.groupHistory)) {
        if (Array.isArray(items) && items.every(validateItem)) state.groupHistory[chatId] = items;
      }
    }
    return state;
  }

  async save(lastSeen: Map<string, number>, groupHistory: Map<string, T[]>): Promise<void> {
    const directory = path.dirname(this.filePath);
    await fs.mkdir(directory, { recursive: true, mode: 0o700 });
    const state: MonitorState<T> = {
      version: 1,
      lastSeen: Object.fromEntries(lastSeen),
      groupHistory: Object.fromEntries(groupHistory),
    };
    const temporary = path.join(directory, `.${path.basename(this.filePath)}.${randomUUID()}.tmp`);
    try {
      await fs.writeFile(temporary, JSON.stringify(state), { flag: "wx", mode: 0o600 });
      await fs.rename(temporary, this.filePath);
    } finally {
      await fs.rm(temporary, { force: true }).catch(() => undefined);
    }
  }
}
