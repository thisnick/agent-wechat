import {
  NativeApiError,
  type NativeMessage,
  type NativeOperation,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import { pause, readableError } from "./session.ts";

export function canRecall(message: NativeMessage): boolean {
  return (
    message.direction === "outgoing" &&
    message.type > 0 &&
    message.type < 10000 &&
    !message.recall &&
    !!message.serverId &&
    /^[1-9]\d{0,19}$/.test(message.serverId) &&
    BigInt(message.serverId) <= 18446744073709551615n
  );
}
type Receipt = { key: string; serverId: string; operationId?: string };
type Storage = { read: () => unknown; write: (receipt: Receipt) => void };
type State = {
  busy: boolean;
  operation: NativeOperation | null;
  error: string;
  uncertain: boolean;
};
const ended = (operation: NativeOperation) =>
  [
    "succeeded",
    "failed",
    "cancelled",
    "rejected",
    "needs_review",
    "expired",
  ].includes(operation.state);

export class MessageRecall {
  private value: State = {
    busy: false,
    operation: null,
    error: "",
    uncertain: false,
  };
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private client: NativeWeChatClient;
  private message: NativeMessage;
  private receipt: Receipt | null = null;
  private storage: Storage;
  private interval: number;
  constructor(
    client: NativeWeChatClient,
    message: NativeMessage,
    storage: Storage,
    interval = 700,
  ) {
    this.client = client;
    this.message = message;
    this.storage = storage;
    this.interval = interval;
    const saved = storage.read() as Receipt | null;
    if (
      saved &&
      saved.serverId === message.serverId &&
      typeof saved.key === "string" &&
      saved.key.length > 0 &&
      saved.key.length <= 200 &&
      !/[\r\n]/.test(saved.key) &&
      (saved.operationId === undefined || typeof saved.operationId === "string")
    ) {
      this.receipt = saved;
      // Inspect a known receipt after reload; never start a recall on mounting a message.
      if (saved.operationId) queueMicrotask(() => void this.check());
      else this.value.uncertain = true;
    }
  }
  snapshot = () => this.value;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private publish(change: Partial<State>) {
    if (this.controller.signal.aborted) return;
    this.value = { ...this.value, ...change };
    for (const listener of this.listeners) listener();
  }
  async recall(): Promise<void> {
    if (
      !canRecall(this.message) ||
      this.value.busy ||
      this.value.operation ||
      this.controller.signal.aborted
    )
      return;
    if (this.receipt?.operationId) {
      await this.check();
      return;
    }
    this.receipt ??= {
      key: crypto.randomUUID(),
      serverId: this.message.serverId!,
    };
    this.storage.write(this.receipt);
    this.publish({ busy: true, error: "", uncertain: false });
    try {
      const operation = await this.client.recallMessage(
        this.message.accountId,
        this.message.id,
        this.receipt.serverId,
        this.receipt.key,
        this.controller.signal,
      );
      this.receipt.operationId = operation.id;
      this.storage.write(this.receipt);
      await this.watch(operation);
    } catch (error) {
      this.publish({
        error:
          error instanceof NativeApiError &&
          [
            "recall_target_changed",
            "recall_target_unavailable",
            "recall_identity_mismatch",
          ].includes(error.code)
            ? "This message can’t be recalled."
            : readableError(error),
        uncertain: !(error instanceof NativeApiError && error.status < 500),
      });
    } finally {
      this.publish({ busy: false });
    }
  }
  async check(): Promise<void> {
    if (
      !this.receipt?.operationId ||
      this.value.busy ||
      this.controller.signal.aborted
    )
      return;
    this.publish({ busy: true, error: "" });
    try {
      await this.watch(
        await this.client.operation(
          this.message.accountId,
          this.receipt.operationId,
          this.controller.signal,
        ),
      );
    } catch (error) {
      this.publish({ error: readableError(error), uncertain: true });
    } finally {
      this.publish({ busy: false });
    }
  }
  private async watch(initial: NativeOperation): Promise<void> {
    let operation = initial;
    for (;;) {
      const result = operation.result as Record<string, unknown> | null;
      if (
        operation.accountId !== this.message.accountId ||
        operation.kind !== "message_recall" ||
        operation.id !== this.receipt?.operationId ||
        !result ||
        result.messageId !== this.message.id ||
        result.threadId !== this.message.threadId ||
        result.serverId !== this.message.serverId
      )
        throw new Error("Invalid recall operation");
      if (this.controller.signal.aborted) return;
      this.publish({
        operation,
        uncertain: operation.state === "needs_review",
      });
      if (ended(operation)) {
        if (operation.state !== "succeeded")
          this.publish({
            error:
              operation.state === "needs_review"
                ? "Recall couldn’t be confirmed. Check the conversation."
                : "This message couldn’t be recalled.",
          });
        return;
      }
      await pause(this.interval, this.controller.signal);
      if (this.controller.signal.aborted) return;
      operation = await this.client.operation(
        this.message.accountId,
        operation.id,
        this.controller.signal,
      );
    }
  }
  close() {
    this.controller.abort();
    this.listeners.clear();
  }
}
