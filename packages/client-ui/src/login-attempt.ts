import {
  NativeApiError,
  type NativeOperation,
  type NativeWeChatClient,
} from "@agent-wechat/shared";

/**
 * `auto` tries the saved sign-in first and may show a QR code; `qr` skips the
 * saved sign-in and always shows one, for the same account and profile.
 */
export type LoginStrategyChoice = "auto" | "qr";
/** The strategy is saved with its key: retrying a key must repeat the same request. */
export type SavedLogin = {
  key: string;
  operationId?: string;
  strategy?: LoginStrategyChoice;
};
export type LoginStorage = {
  read: () => SavedLogin | null;
  write: (value: SavedLogin | null) => void;
};
const terminal = new Set([
  "succeeded",
  "failed",
  "expired",
  "rejected",
  "cancelled",
  "needs_login",
]);

/** Persist request/operation IDs so reloads resume the same sign-in attempt. */
export class LoginAttempt {
  private client: NativeWeChatClient;
  private account: string;
  private storage: LoginStorage;
  /**
   * The strategy of the operation the last `open()` returned. It can differ
   * from the one asked for when an earlier attempt is resumed or repeated.
   */
  strategy: LoginStrategyChoice = "auto";
  constructor(
    client: NativeWeChatClient,
    account: string,
    storage: LoginStorage,
  ) {
    this.client = client;
    this.account = account;
    this.storage = storage;
  }
  async open(strategy: LoginStrategyChoice = "auto"): Promise<NativeOperation> {
    let saved = this.storage.read();
    if (saved?.operationId) {
      try {
        const operation = await this.client.operation(
          this.account,
          saved.operationId,
        );
        if (operation.accountId !== this.account || operation.kind !== "login")
          throw new Error("Invalid login operation identity");
        if (!terminal.has(operation.state) || operation.state === "succeeded") {
          this.strategy = saved.strategy ?? "auto";
          return operation;
        }
      } catch (error) {
        if (!(error instanceof NativeApiError) || error.status !== 404)
          throw error;
      }
      this.storage.write(null);
      saved = null;
    }
    // A saved key without an operation is a submission whose response was
    // lost. Repeat it unchanged: the hub refuses a reused key with new options.
    const pending = saved?.key
      ? { key: saved.key, strategy: saved.strategy ?? "auto" }
      : { key: crypto.randomUUID(), strategy };
    // Save before submission: a lost response must retry the same admission key.
    this.storage.write(pending);
    const operation = await this.client.login(
      this.account,
      {
        strategy: pending.strategy,
        timeoutSeconds: 900,
        qrRefresh: { mode: "auto", maxRefreshes: 3 },
      },
      pending.key,
    );
    this.storage.write({ ...pending, operationId: operation.id });
    this.strategy = pending.strategy;
    return operation;
  }
  complete(operationId: string): void {
    if (this.storage.read()?.operationId === operationId)
      this.storage.write(null);
  }
}
