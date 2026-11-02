import type { NativeOperation } from "@agent-wechat/shared";
import { GroupMutations, groupEnded } from "./group-creation.ts";
import { subscribeLocale, t } from "./i18n.ts";
import { readableError, type HubSession } from "./session.ts";

type Field = "pinned" | "muted";
export type PreferenceState<K extends Field> = Record<K, boolean | null> & {
  supported: boolean;
  disabled: boolean;
  busy: boolean;
  saving: boolean;
  preview?: boolean;
  uncertain: boolean;
  error: string;
  announcement: string;
};

function confirmedPreference(
  operation: NativeOperation | undefined,
  field: Field,
): boolean {
  const result = operation?.result as Record<string, unknown> | undefined;
  return (
    operation?.state === "succeeded" &&
    result?.remoteOutcome === "accepted" &&
    (result.profileProjection === "complete" ||
      (field === "muted" && result.profileProjection === "partial"))
  );
}

/** One view of canonical state and the shared account/target mutation receipt. */
export class ThreadPreference<K extends Field> {
  private value: PreferenceState<K>;
  private field: K;
  private action: "pin" | "mute";
  private listeners = new Set<() => void>();
  private stopped = false;
  private syncing = false;
  private submitting = false;
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
    field: K,
  ) {
    this.field = field;
    this.action = field === "pinned" ? "pin" : "mute";
    this.session = session;
    this.mutations = mutations;
    this.account = account;
    this.thread = thread;
    // Activation already obtains a fresh ordered inventory before enabling
    // controls. Completed receipts must not add one status request per row.
    const receipt = mutations.snapshot().receipt;
    if (
      receipt?.input.action === this.action &&
      confirmedPreference(receipt.operation, this.field)
    )
      this.synced = receipt.key;
    this.value = this.read();
    this.stop = [
      session.subscribe(this.update),
      mutations.subscribe(this.update),
      subscribeLocale(this.update),
    ];
    this.update();
  }
  snapshot = (): PreferenceState<K> => this.value;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private read(): PreferenceState<K> {
    const session = this.session.snapshot();
    const operation = this.mutations.snapshot();
    const receipt = operation.receipt;
    const change = receipt?.input.action === this.action ? receipt : undefined;
    const result = change?.operation?.result as
      | Record<string, unknown>
      | undefined;
    const thread = session.threads.find((item) => item.id === this.thread);
    const enabled =
      typeof thread?.[this.field] === "boolean" ? thread[this.field]! : null;
    const targets =
      this.field === "pinned"
        ? session.capabilities.threadPinningTargets
        : session.capabilities.threadNotificationTargets;
    const supported =
      this.thread !== "filehelper" &&
      Array.isArray(targets) &&
      targets.includes(this.thread.endsWith("@chatroom") ? "group" : "direct");
    const uncertain =
      !!change &&
      !change.notAdmitted &&
      ((!change.operation && !operation.busy) ||
        change.operation?.state === "needs_review" ||
        (result?.remoteOutcome === "unknown" &&
          ["succeeded", "failed", "rejected", "cancelled"].includes(
            change.operation?.state ?? "",
          )) ||
        (!!operation.error && !groupEnded(change.operation)));
    const pending =
      !!receipt &&
      !receipt.notAdmitted &&
      (!groupEnded(receipt.operation) ||
        receipt.operation?.state === "needs_review");
    const confirmed = confirmedPreference(change?.operation, this.field);
    const needsSync = confirmed && this.synced !== change!.key;
    const busy =
      operation.busy ||
      this.syncing ||
      (pending && !uncertain) ||
      (needsSync && !this.syncError);
    const saving = busy && (!!change || this.syncing);
    const isPin = this.field === "pinned";
    const noun = isPin ? "pin" : "notification";
    let error = this.syncError;
    if (change?.notAdmitted) {
      error =
        change.admissionStatus === 403
          ? isPin
            ? t(
                "This access key can’t change pins. Use a key with account management permission.",
                "此访问密钥无法更改置顶状态。请使用具有账号管理权限的密钥。",
              )
            : t(
                "This access key can’t change notifications. Use a key with account management permission.",
                "此访问密钥无法更改通知设置。请使用具有账号管理权限的密钥。",
              )
          : change.admissionStatus === 409
          ? isPin
            ? t(
                "Pinning is unavailable. Refresh this chat and check again.",
                "置顶功能不可用。请刷新此会话并重试。",
              )
            : t(
                "Notifications are unavailable. Refresh this chat and check again.",
                "通知设置不可用。请刷新此会话并重试。",
              )
          : isPin
          ? t(
              "Couldn’t pin this chat. Refresh and try again.",
              "无法置顶此会话。请刷新并重试。",
            )
          : t(
              "Couldn’t change notifications. Refresh and try again.",
              "无法更改通知设置。请刷新并重试。",
            );
    } else if (
      change?.operation &&
      ["rejected", "failed", "cancelled"].includes(change.operation.state)
    )
      error = t(
        `${
          isPin ? "Pin" : "Notification"
        } change wasn’t applied. Refresh and try again.`,
        isPin
          ? "置顶更改未生效。请刷新并重试。"
          : "通知更改未生效。请刷新并重试。",
      );
    else if (operation.error && change && !uncertain && !confirmed)
      error = operation.error;
    const announcement = saving
      ? t(
          `Saving ${noun}`,
          isPin ? "正在保存置顶" : "正在保存通知设置",
        )
      : uncertain
      ? t(
          `${isPin ? "Pin" : "Notification"} change needs a status check`,
          isPin ? "置顶更改需要检查状态" : "通知更改需要检查状态",
        )
      : error ||
        (enabled === null
          ? t(
              `${isPin ? "Pin" : "Notification"} state unavailable`,
              isPin ? "置顶状态不可用" : "通知状态不可用",
            )
          : enabled
          ? isPin
            ? t("Chat pinned", "已置顶聊天")
            : t("Notifications muted", "已静音通知")
          : isPin
          ? t("Chat unpinned", "已取消置顶")
          : t("Notifications unmuted", "已取消静音"));
    return {
      [this.field]: enabled,
      preview:
        isPin &&
        change?.input.action === "pin" &&
        saving &&
        (this.submitting || !!change.operation) &&
        !uncertain &&
        !error &&
        !change.notAdmitted
          ? change.input.pinned
          : undefined,
      supported,
      busy,
      saving,
      uncertain,
      error,
      announcement,
      disabled:
        this.stopped ||
        session.account !== this.account ||
        !session.connected ||
        !supported ||
        enabled === null ||
        busy ||
        pending ||
        uncertain ||
        needsSync ||
        change?.admissionStatus === 403,
    } as PreferenceState<K>;
  }
  private update = (): void => {
    if (this.stopped) return;
    const next = this.read();
    if (
      (Object.keys(next) as (keyof PreferenceState<K>)[]).some(
        (key) => next[key] !== this.value[key],
      )
    ) {
      this.value = next;
      for (const listener of this.listeners) listener();
    }
    const receipt = this.mutations.snapshot().receipt;
    if (
      receipt?.input.action === this.action &&
      confirmedPreference(receipt.operation, this.field) &&
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
        await this.session.refreshConversations(
          this.account,
          false,
          this.field === "muted" && this.thread.endsWith("@chatroom")
            ? this.thread
            : undefined,
        );
        if (key) this.synced = key;
      } catch (error) {
        this.syncError = t(
          `Couldn’t refresh ${
            this.field === "pinned" ? "pins" : "notifications"
          }. ${readableError(error)}`,
          `无法刷新${
            this.field === "pinned" ? "置顶状态" : "通知设置"
          }。${readableError(error)}`,
        );
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
    const enabled = !this.value[this.field];
    this.syncError = "";
    this.mutations.clear();
    this.submitting = true;
    try {
      await this.mutations.run(
        this.field === "pinned"
          ? { action: "pin", groupId: this.thread, pinned: enabled }
          : { action: "mute", groupId: this.thread, muted: enabled },
      );
    } finally {
      this.submitting = false;
      this.update();
    }
    if (this.reconciling) await this.reconciling;
  }
  async check(): Promise<void> {
    if (this.stopped) return;
    this.syncError = "";
    const receipt = this.mutations.snapshot().receipt;
    if (receipt?.input.action === this.action && receipt.notAdmitted)
      this.mutations.clear();
    else if (receipt?.input.action === this.action)
      await this.mutations.check();
    await this.reconcile(
      receipt?.input.action === this.action &&
        confirmedPreference(
          this.mutations.snapshot().receipt?.operation,
          this.field,
        )
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

export type PinState = PreferenceState<"pinned">;
export const confirmedPin = (operation?: NativeOperation) =>
  confirmedPreference(operation, "pinned");
type PreferenceArgs = [
  session: HubSession,
  mutations: GroupMutations,
  account: string,
  thread: string,
];
export class ThreadPinning extends ThreadPreference<"pinned"> {
  constructor(...args: PreferenceArgs) {
    super(...args, "pinned");
  }
}
export class ThreadMuting extends ThreadPreference<"muted"> {
  constructor(...args: PreferenceArgs) {
    super(...args, "muted");
  }
}
