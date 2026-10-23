import type {
  GroupCreateRequest,
  NativeOperation,
  NativeWeChatClient,
} from "@agent-wechat/shared";
import { pause, readableError } from "./session.ts";
export type GroupReceipt = {
  key: string;
  input: GroupCreateRequest;
  operation?: NativeOperation;
};
type Storage = {
  read(): GroupReceipt | null;
  write(value: GroupReceipt | null): void;
};
type Snapshot = { receipt: GroupReceipt | null; busy: boolean; error: string };
const ended = new Set([
  "succeeded",
  "failed",
  "needs_review",
  "cancelled",
  "rejected",
]);
/** Status recovery never repeats an uncertain group-creation request. */
export class GroupCreation {
  private value: Snapshot;
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private watching = false;
  private client: NativeWeChatClient;
  private account: string;
  private storage: Storage;
  constructor(client: NativeWeChatClient, account: string, storage: Storage) {
    this.client = client;
    this.account = account;
    this.storage = storage;
    this.value = { receipt: storage.read(), busy: false, error: "" };
    if (this.value.receipt) queueMicrotask(() => void this.check());
  }
  snapshot = (): Snapshot => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private publish(patch: Partial<Snapshot>): void {
    if (this.controller.signal.aborted) return;
    this.value = { ...this.value, ...patch };
    this.storage.write(this.value.receipt);
    for (const listener of this.listeners) listener();
  }
  private accept(operation: NativeOperation, key: string): void {
    if (this.value.receipt?.key !== key || this.controller.signal.aborted)
      return;
    if (
      operation.accountId !== this.account ||
      operation.kind !== "group_create"
    )
      throw new Error("Invalid group creation receipt");
    this.publish({ receipt: { ...this.value.receipt!, operation }, error: "" });
  }
  async create(input: GroupCreateRequest): Promise<void> {
    if (this.value.receipt || this.controller.signal.aborted) return;
    const key = crypto.randomUUID();
    this.publish({
      receipt: {
        key,
        input: { name: input.name.trim(), members: [...input.members].sort() },
      },
      busy: true,
      error: "",
    });
    try {
      this.accept(
        await this.client.createGroup(
          this.account,
          this.value.receipt!.input,
          key,
        ),
        key,
      );
      void this.watch();
    } catch (error) {
      this.publish({ error: readableError(error) });
    } finally {
      this.publish({ busy: false });
    }
  }
  async check(): Promise<void> {
    const receipt = this.value.receipt;
    if (!receipt || this.controller.signal.aborted) return;
    this.publish({ busy: true, error: "" });
    try {
      this.accept(
        receipt.operation
          ? await this.client.operation(
              this.account,
              receipt.operation.id,
              this.controller.signal,
            )
          : await this.client.groupCreationByKey(this.account, receipt.key),
        receipt.key,
      );
      void this.watch();
    } catch (error) {
      this.publish({ error: readableError(error) });
    } finally {
      this.publish({ busy: false });
    }
  }
  private async watch(): Promise<void> {
    if (this.watching) return;
    this.watching = true;
    try {
      while (!this.controller.signal.aborted) {
        const receipt = this.value.receipt;
        const operation = receipt?.operation;
        if (!operation || ended.has(operation.state)) return;
        await pause(800, this.controller.signal);
        if (this.controller.signal.aborted) return;
        this.accept(
          await this.client.operation(
            this.account,
            operation.id,
            this.controller.signal,
          ),
          receipt!.key,
        );
      }
    } catch (error) {
      this.publish({ error: readableError(error) });
    } finally {
      this.watching = false;
    }
  }
  clear(): void {
    if (
      this.value.receipt?.operation &&
      ended.has(this.value.receipt.operation.state) &&
      this.value.receipt.operation.state !== "needs_review"
    )
      this.publish({ receipt: null, error: "", busy: false });
  }
  close(): void {
    this.controller.abort();
    this.listeners.clear();
  }
}
