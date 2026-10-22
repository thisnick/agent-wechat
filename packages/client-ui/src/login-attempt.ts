import {
  NativeApiError,
  type NativeOperation,
  type NativeWeChatClient,
} from "@agent-wechat/shared";

export type SavedLogin = { key: string; operationId?: string };
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

/** Persist only request/operation IDs, so a reload can resume the same QR login. */
export class LoginAttempt {
  private client: NativeWeChatClient;
  private account: string;
  private storage: LoginStorage;
  constructor(
    client: NativeWeChatClient,
    account: string,
    storage: LoginStorage,
  ) {
    this.client = client;
    this.account = account;
    this.storage = storage;
  }
  async open(): Promise<NativeOperation> {
    let saved = this.storage.read();
    if (saved?.operationId) {
      try {
        const operation = await this.client.operation(
          this.account,
          saved.operationId,
        );
        if (operation.accountId !== this.account || operation.kind !== "login")
          throw new Error("Invalid login operation identity");
        if (!terminal.has(operation.state) || operation.state === "succeeded")
          return operation;
      } catch (error) {
        if (!(error instanceof NativeApiError) || error.status !== 404)
          throw error;
      }
      this.storage.write(null);
      saved = null;
    }
    const key = saved?.key ?? crypto.randomUUID();
    // Save before submission: a lost response must retry the same admission key.
    this.storage.write({ key });
    const operation = await this.client.login(
      this.account,
      {
        strategy: "qr",
        timeoutSeconds: 900,
        qrRefresh: { mode: "auto", maxRefreshes: 3 },
      },
      key,
    );
    this.storage.write({ key, operationId: operation.id });
    return operation;
  }
  complete(operationId: string): void {
    if (this.storage.read()?.operationId === operationId)
      this.storage.write(null);
  }
}
