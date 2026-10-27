import {
  NativeApiError,
  type NativeGroupCreateRequest,
  type NativeOperation,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import { pause, readableError } from "./session.ts";
export type GroupAction =
  | { action: "create"; members: string[]; name?: string }
  | { action: "rename"; groupId: string; name: string }
  | { action: "leave" | "clear_announcement"; groupId: string }
  | {
      action: "add_members" | "remove_members";
      groupId: string;
      members: string[];
    }
  | { action: "my_name"; groupId: string; displayName: string }
  | { action: "mute"; groupId: string; muted: boolean }
  | { action: "pin"; groupId: string; pinned: boolean };
export type GroupReceipt = {
  key: string;
  input: GroupAction;
  operation?: NativeOperation;
  notAdmitted?: boolean;
  admissionStatus?: number;
};
export function validGroupReceipt(
  value: unknown,
  target: string,
): value is GroupReceipt {
  if (!value || typeof value !== "object") return false;
  const receipt = value as GroupReceipt;
  const input = receipt.input;
  if (
    typeof receipt.key !== "string" ||
    !receipt.key ||
    receipt.key.length > 200 ||
    /[\r\n]/.test(receipt.key) ||
    !input ||
    typeof input !== "object"
  )
    return false;
  if (
    ![
      "create",
      "rename",
      "leave",
      "add_members",
      "remove_members",
      "my_name",
      "clear_announcement",
      "mute",
      "pin",
    ].includes(input.action)
  )
    return false;
  if (
    input.action === "create" ? target !== "create" : input.groupId !== target
  )
    return false;
  if (
    ["create", "add_members", "remove_members"].includes(input.action) &&
    !("members" in input)
  )
    return false;
  if (
    "members" in input &&
    (!Array.isArray(input.members) ||
      input.members.length > 40 ||
      input.members.some(
        (member) =>
          typeof member !== "string" || !member || member.length > 256,
      ))
  )
    return false;
  if (
    (input.action === "create" &&
      input.name !== undefined &&
      typeof input.name !== "string") ||
    (input.action === "rename" && typeof input.name !== "string") ||
    (input.action === "my_name" && typeof input.displayName !== "string") ||
    (input.action === "mute" && typeof input.muted !== "boolean") ||
    (input.action === "pin" && typeof input.pinned !== "boolean")
  )
    return false;
  return true;
}
type Storage = {
  read(): GroupReceipt | null;
  write(value: GroupReceipt | null): void;
};
type Snapshot = { receipt: GroupReceipt | null; busy: boolean; error: string };
export const groupEnded = (operation?: NativeOperation): boolean => {
  const result = operation?.result as Record<string, unknown> | undefined;
  if (
    result?.action === "mute" &&
    operation?.state === "succeeded" &&
    result.profileProjection === "pending"
  )
    return false;
  if (
    result?.action === "pin" &&
    operation?.state === "succeeded" &&
    (result.remoteOutcome !== "accepted" ||
      result.profileProjection !== "complete")
  )
    return false;
  return (
    !!operation &&
    ["succeeded", "failed", "needs_review", "cancelled", "rejected"].includes(
      operation.state,
    )
  );
};
export const groupTarget = (input: GroupAction): string | null =>
  input.action === "create" ? null : input.groupId;
/** Each account/target retains its admission key. Recovery only reads status. */
export class GroupMutations {
  private value: Snapshot;
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private watching = false;
  private statusVersion = 0;
  private checkingVersion = 0;
  private client: NativeWeChatClient;
  private account: string;
  private storage: Storage;
  constructor(client: NativeWeChatClient, account: string, storage: Storage) {
    this.client = client;
    this.account = account;
    this.storage = storage;
    const receipt = storage.read();
    if (receipt?.operation && !this.matches(receipt.operation, receipt))
      delete receipt.operation;
    this.value = { receipt, busy: false, error: "" };
    const settledPreference =
      ["pin", "mute"].includes(receipt?.input.action ?? "") &&
      receipt?.operation?.state === "succeeded" &&
      groupEnded(receipt?.operation);
    if (
      this.value.receipt &&
      !this.value.receipt.notAdmitted &&
      !settledPreference
    )
      queueMicrotask(() => void this.check());
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
  private matches(operation: NativeOperation, receipt: GroupReceipt): boolean {
    const result = operation?.result as Record<string, unknown> | null;
    return (
      !!operation &&
      operation.accountId === this.account &&
      operation.kind === "group_mutation" &&
      typeof operation.id === "string" &&
      !!operation.id &&
      !!result &&
      result.action === receipt.input.action &&
      result.targetId === groupTarget(receipt.input) &&
      (receipt.input.action !== "pin" || result.pinned === receipt.input.pinned)
    );
  }
  private accept(operation: NativeOperation, key: string): void {
    const receipt = this.value.receipt;
    if (receipt?.key !== key || this.controller.signal.aborted) return;
    if (!this.matches(operation, receipt))
      throw new Error("Invalid group operation receipt");
    this.publish({ receipt: { ...receipt, operation }, error: "" });
  }
  async create(input: NativeGroupCreateRequest): Promise<void> {
    await this.run({ action: "create", ...input });
  }
  async run(input: GroupAction): Promise<void> {
    if (this.value.receipt || this.controller.signal.aborted) return;
    const request = structuredClone(input);
    if ("members" in request) request.members.sort();
    const key = crypto.randomUUID();
    this.publish({ receipt: { key, input: request }, busy: true, error: "" });
    const client = this.client,
      account = this.account,
      signal = this.controller.signal;
    try {
      let operation: NativeOperation;
      switch (request.action) {
        case "create":
          operation = await client.createGroup(
            account,
            {
              members: request.members,
              ...(request.name?.trim() ? { name: request.name.trim() } : {}),
            },
            key,
            signal,
          );
          break;
        case "rename":
          operation = await client.renameGroup(
            account,
            request.groupId,
            request.name.trim(),
            key,
            signal,
          );
          break;
        case "leave":
          operation = await client.leaveGroup(
            account,
            request.groupId,
            key,
            signal,
          );
          break;
        case "add_members":
          operation = await client.addGroupMembers(
            account,
            request.groupId,
            request.members,
            key,
            signal,
          );
          break;
        case "remove_members":
          operation = await client.removeGroupMembers(
            account,
            request.groupId,
            request.members,
            key,
            signal,
          );
          break;
        case "my_name":
          operation = await client.setMyGroupName(
            account,
            request.groupId,
            request.displayName,
            key,
            signal,
          );
          break;
        case "clear_announcement":
          operation = await client.clearGroupAnnouncement(
            account,
            request.groupId,
            key,
            signal,
          );
          break;
        case "mute":
          operation = await client.setThreadMuted(
            account,
            request.groupId,
            request.muted,
            key,
            signal,
          );
          break;
        case "pin":
          operation = await client.setThreadPinned(
            account,
            request.groupId,
            request.pinned,
            key,
            signal,
          );
          break;
      }
      this.accept(operation, key);
      void this.watch();
    } catch (error) {
      // These admission errors cannot have submitted a group operation. A lost
      // response, conflict or server error remains recoverable by the same key.
      const notAdmitted =
        error instanceof NativeApiError &&
        ([400, 401, 403, 404, 422].includes(error.status) ||
          (request.action === "pin" &&
            error.status === 409 &&
            [
              "ordinary_contact_required",
              "ordinary_group_required",
              "ordinary_thread_required",
              "thread_session_unavailable",
              "thread_pinning_unavailable",
            ].includes(error.code)));
      this.publish({
        receipt: {
          ...this.value.receipt!,
          notAdmitted,
          ...(error instanceof NativeApiError
            ? { admissionStatus: error.status }
            : {}),
        },
        error: readableError(error),
      });
    } finally {
      this.publish({ busy: false });
    }
  }
  async check(): Promise<void> {
    const receipt = this.value.receipt;
    if (!receipt || receipt.notAdmitted || this.controller.signal.aborted)
      return;
    this.publish({ busy: true, error: "" });
    const version = ++this.statusVersion;
    this.checkingVersion = version;
    try {
      const operation = receipt.operation
        ? await this.client.operation(
            this.account,
            receipt.operation.id,
            this.controller.signal,
          )
        : await this.client.groupMutationByKey(
            this.account,
            receipt.key,
            this.controller.signal,
          );
      if (version !== this.statusVersion) return;
      this.accept(operation, receipt.key);
      void this.watch();
    } catch (error) {
      if (version === this.statusVersion)
        this.publish({ error: readableError(error) });
    } finally {
      if (this.checkingVersion === version) {
        this.checkingVersion = 0;
        this.publish({ busy: false });
      }
    }
  }
  private async watch(): Promise<void> {
    if (this.watching) return;
    this.watching = true;
    try {
      while (!this.controller.signal.aborted) {
        const receipt = this.value.receipt,
          operation = receipt?.operation;
        if (!operation || groupEnded(operation)) return;
        await pause(800, this.controller.signal);
        if (this.controller.signal.aborted) return;
        if (this.checkingVersion) continue;
        const version = ++this.statusVersion;
        const current = await this.client.operation(
          this.account,
          operation.id,
          this.controller.signal,
        );
        if (version === this.statusVersion) this.accept(current, receipt!.key);
      }
    } catch (error) {
      this.publish({ error: readableError(error) });
    } finally {
      this.watching = false;
    }
  }
  async cancel(): Promise<void> {
    const receipt = this.value.receipt;
    if (
      !receipt?.operation ||
      groupEnded(receipt.operation) ||
      this.controller.signal.aborted
    )
      return;
    this.publish({ busy: true });
    try {
      this.accept(
        await this.client.cancelOperation(this.account, receipt.operation.id),
        receipt.key,
      );
    } catch (error) {
      this.publish({ error: readableError(error) });
      await this.check();
    } finally {
      this.publish({ busy: false });
    }
  }
  clear(): void {
    const receipt = this.value.receipt;
    if (
      receipt?.notAdmitted ||
      (groupEnded(receipt?.operation) &&
        receipt?.operation?.state !== "needs_review")
    )
      this.publish({ receipt: null, error: "", busy: false });
  }
  close(): void {
    this.controller.abort();
    this.listeners.clear();
  }
}
