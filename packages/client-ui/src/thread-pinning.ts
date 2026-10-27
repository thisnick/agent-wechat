import type { NativeOperation } from "@agent-wechat/shared";
import { GroupMutations, groupEnded } from "./group-creation.ts";
import { readableError, type HubSession } from "./session.ts";

export type PinState = {
  pinned: boolean | null;
  supported: boolean;
  disabled: boolean;
  busy: boolean;
  uncertain: boolean;
  error: string;
  announcement: string;
};

export function confirmedPin(operation?: NativeOperation): boolean {
  const result = operation?.result as Record<string, unknown> | undefined;
  return (
    operation?.state === "succeeded" &&
    result?.remoteOutcome === "accepted" &&
    result.profileProjection === "complete"
  );
}

/** One view of canonical state and the shared account/target mutation receipt. */
export class ThreadPinning {
  private value: PinState;
  private listeners = new Set<() => void>();
  private stopped = false;
  private syncing = false;
  private syncError = "";
  private synced = "";
  private reconciling?: Promise<void>;
  private stop: (() => void)[];
  private session: HubSession;
  private mutations: GroupMutations;
  private account: string;
  private thread: string;
  constructor(
    session: HubSession,
    mutations: GroupMutations,
    account: string,
    thread: string,
  ) {
    this.session = session;
    this.mutations = mutations;
    this.account = account;
    this.thread = thread;
    // Activation already obtains a fresh ordered inventory before enabling
    // controls. Completed receipts must not add one status request per row.
    const receipt = mutations.snapshot().receipt;
    if (receipt?.input.action === "pin" && confirmedPin(receipt.operation))
      this.synced = receipt.key;
    this.value = this.read();
    this.stop = [
      session.subscribe(this.update),
      mutations.subscribe(this.update),
    ];
    this.update();
  }
  snapshot = (): PinState => this.value;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private read(): PinState {
    const session = this.session.snapshot();
    const operation = this.mutations.snapshot();
    const receipt = operation.receipt;
    const pin = receipt?.input.action === "pin" ? receipt : undefined;
    const result = pin?.operation?.result as
      | Record<string, unknown>
      | undefined;
    const thread = session.threads.find((item) => item.id === this.thread);
    const pinned = typeof thread?.pinned === "boolean" ? thread.pinned : null;
    const supported =
      this.thread !== "filehelper" &&
      Array.isArray(session.capabilities.threadPinningTargets) &&
      session.capabilities.threadPinningTargets.includes(
        this.thread.endsWith("@chatroom") ? "group" : "direct",
      );
    const uncertain =
      !!pin &&
      !pin.notAdmitted &&
      ((!pin.operation && !operation.busy) ||
        pin.operation?.state === "needs_review" ||
        (result?.remoteOutcome === "unknown" &&
          ["succeeded", "failed", "rejected", "cancelled"].includes(
            pin.operation?.state ?? "",
          )) ||
        (!!operation.error && !groupEnded(pin.operation)));
    const pending =
      !!receipt &&
      !receipt.notAdmitted &&
      (!groupEnded(receipt.operation) ||
        receipt.operation?.state === "needs_review");
    const confirmed = confirmedPin(pin?.operation);
    const needsSync = confirmed && this.synced !== pin!.key;
    const busy =
      operation.busy ||
      this.syncing ||
      (pending && !uncertain) ||
      (needsSync && !this.syncError);
    let error = this.syncError;
    if (pin?.notAdmitted) {
      error =
        pin.admissionStatus === 403
          ? "This access key can’t change pins. Use a key with account management permission."
          : pin.admissionStatus === 409
          ? "Pinning is unavailable. Refresh this chat and check again."
          : "Couldn’t pin this chat. Refresh and try again.";
    } else if (
      pin?.operation &&
      ["rejected", "failed", "cancelled"].includes(pin.operation.state)
    )
      error = "Pin change wasn’t applied. Refresh and try again.";
    else if (operation.error && pin && !uncertain && !confirmed)
      error = operation.error;
    const announcement = busy
      ? "Saving pin"
      : uncertain
      ? "Pin change needs a status check"
      : error ||
        (pinned === null
          ? "Pin state unavailable"
          : pinned
          ? "Chat pinned"
          : "Chat unpinned");
    return {
      pinned,
      supported,
      busy,
      uncertain,
      error,
      announcement,
      disabled:
        this.stopped ||
        session.account !== this.account ||
        !session.connected ||
        !supported ||
        pinned === null ||
        busy ||
        pending ||
        uncertain ||
        needsSync ||
        pin?.admissionStatus === 403,
    };
  }
  private update = (): void => {
    if (this.stopped) return;
    const next = this.read();
    if (
      (Object.keys(next) as (keyof PinState)[]).some(
        (key) => next[key] !== this.value[key],
      )
    ) {
      this.value = next;
      for (const listener of this.listeners) listener();
    }
    const receipt = this.mutations.snapshot().receipt;
    if (
      receipt?.input.action === "pin" &&
      confirmedPin(receipt.operation) &&
      this.synced !== receipt.key &&
      !this.reconciling &&
      !this.syncError
    )
      void this.reconcile(receipt.key);
  };
  private reconcile(key?: string): Promise<void> {
    if (this.reconciling) return this.reconciling;
    this.syncing = true;
    this.syncError = "";
    // Assign the flight before publishing: both controls can subscribe at once.
    const work = Promise.resolve().then(async () => {
      try {
        await this.session.refreshConversations(this.account);
        if (key) this.synced = key;
      } catch (error) {
        this.syncError = `Couldn’t refresh pins. ${readableError(error)}`;
      } finally {
        this.syncing = false;
        this.reconciling = undefined;
        this.update();
      }
    });
    this.reconciling = work;
    this.update();
    return work;
  }
  async toggle(): Promise<void> {
    if (this.value.disabled) return;
    const pinned = !this.value.pinned;
    this.syncError = "";
    this.mutations.clear();
    await this.mutations.run({ action: "pin", groupId: this.thread, pinned });
    if (this.reconciling) await this.reconciling;
  }
  async check(): Promise<void> {
    if (this.stopped) return;
    this.syncError = "";
    const receipt = this.mutations.snapshot().receipt;
    if (receipt?.input.action === "pin" && receipt.notAdmitted)
      this.mutations.clear();
    else if (receipt?.input.action === "pin") await this.mutations.check();
    await this.reconcile(
      receipt?.input.action === "pin" &&
        confirmedPin(this.mutations.snapshot().receipt?.operation)
        ? receipt.key
        : undefined,
    );
  }
  close(): void {
    this.stopped = true;
    for (const stop of this.stop) stop();
    this.listeners.clear();
  }
}
