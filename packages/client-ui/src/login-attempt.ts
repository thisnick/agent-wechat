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
    let operation: NativeOperation;
    try {
      operation = await this.client.login(
        this.account,
        {
          strategy: pending.strategy,
          timeoutSeconds: 900,
          qrRefresh: { mode: "auto", maxRefreshes: 3 },
        },
        pending.key,
      );
    } catch (error) {
      // A busy account refuses the submission outright: the hub checks the key
      // first and admits nothing, so the next attempt may use a fresh one.
      // Clear only this attempt's own entry: another window may have saved an
      // accepted operation for the same account meanwhile.
      const current = this.storage.read();
      if (accountBusy(error) && current?.key === pending.key && !current.operationId)
        this.storage.write(null);
      throw error;
    }
    this.storage.write({ ...pending, operationId: operation.id });
    this.strategy = pending.strategy;
    return operation;
  }
  complete(operationId: string): void {
    if (this.storage.read()?.operationId === operationId)
      this.storage.write(null);
  }
}

/** The hub refused a sign-in because other work holds the account. */
export function accountBusy(error: unknown): boolean {
  return error instanceof NativeApiError && error.code === "operation_in_progress";
}

/** How {@link openWhenFree} ended. */
export type OpenedLogin =
  | { operation: NativeOperation; alreadySignedIn?: undefined }
  | { operation?: undefined; alreadySignedIn: true };

/**
 * Starts or resumes a sign-in, waiting out a busy account. The hub refuses a
 * new sign-in with `operation_in_progress` while other work holds the account,
 * such as the first sync for a few seconds after a sign-in succeeds. After each
 * refusal this checks whether the account is already signed in, and otherwise
 * waits `delayMs` and tries again. The refusal is passed on after `attempts`.
 */
export async function openWhenFree(
  login: LoginAttempt,
  strategy: LoginStrategyChoice,
  options: {
    /** True when the hub already reports the account as signed in. */
    signedIn: () => Promise<boolean>;
    /** Called before each wait, so the screen can say it is waiting. */
    waiting?: () => void;
    signal?: AbortSignal;
    attempts?: number;
    delayMs?: number;
    /** Waits between attempts; it should return early once `signal` aborts. */
    wait?: (milliseconds: number, signal: AbortSignal) => Promise<void>;
  },
): Promise<OpenedLogin> {
  const { signedIn, waiting, attempts = 30, delayMs = 2000 } = options;
  const signal = options.signal ?? new AbortController().signal;
  const wait =
    options.wait ??
    ((milliseconds: number) => new Promise<void>((done) => setTimeout(done, milliseconds)));
  for (let refusal = 1; ; refusal++) {
    signal.throwIfAborted();
    try {
      return { operation: await login.open(strategy) };
    } catch (error) {
      if (!accountBusy(error) || refusal >= attempts) throw error;
    }
    if (await signedIn()) return { alreadySignedIn: true };
    signal.throwIfAborted();
    waiting?.();
    await wait(delayMs, signal);
  }
}
