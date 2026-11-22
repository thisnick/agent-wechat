import type { NativeIdentity } from "@agent-wechat/shared";

export const GITHUB_REPO = "thisnick/agent-wechat";
export const GITHUB_REPO_URL = `https://github.com/${GITHUB_REPO}`;
export const GITHUB_ISSUE_TEMPLATE = "bug_report.yml";

// Separate keys so a usage update written by another tab can never undo a dismissal.
export const USAGE_KEY = "agentwechat:support-prompt:usage";
export const DISMISSED_KEY = "agentwechat:support-prompt:dismissed";

export const DAYS_BEFORE_ASKING = 3;
export const SENDS_BEFORE_ASKING = 15;
export const IDLE_MS = 30_000;
export const THANKS_CLOSE_MS = 4_000;

export const ACTIVITY_EVENTS = ["pointerdown", "keydown", "wheel", "focus"] as const;

export type SupportPromptStep =
  | "ask"
  | "star"
  | "issue"
  | "thanks-star"
  | "thanks-already"
  | "thanks-issue";

export type UsageRecord = {
  days: string[];
  sends: number;
};

export type SupportEnvironment = {
  desktop: boolean;
  desktopVersion?: string | null;
  hubVersion?: Pick<NativeIdentity, "version" | "target"> | null;
  clientPlatform?: string;
};

type StorageLike = Pick<Storage, "getItem" | "setItem">;

type NavigatorLike = {
  userAgent?: string;
  platform?: string;
  userAgentData?: { platform?: string };
};

type WindowLike = {
  addEventListener: (
    type: string,
    listener: EventListener,
    options?: AddEventListenerOptions | boolean,
  ) => void;
  removeEventListener: (
    type: string,
    listener: EventListener,
    options?: EventListenerOptions | boolean,
  ) => void;
};

type DocumentLike = {
  hasFocus: () => boolean;
  activeElement?: { matches?: (selector: string) => boolean } | null;
  querySelector?: (selector: string) => Element | null;
};

const VALID_PREVIEW_STEPS = new Set<SupportPromptStep>([
  "ask",
  "star",
  "issue",
  "thanks-star",
  "thanks-already",
  "thanks-issue",
]);

function defaultStorage(): StorageLike | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

/** Local calendar day key (`YYYY-MM-DD`) so active days follow the user's clock. */
export function localDateKey(now: Date = new Date()): string {
  const year = now.getFullYear();
  const month = String(now.getMonth() + 1).padStart(2, "0");
  const day = String(now.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function parseUsage(raw: string | null): UsageRecord {
  if (!raw) return { days: [], sends: 0 };
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return { days: [], sends: 0 };
    const record = parsed as Record<string, unknown>;
    const days = Array.isArray(record.days)
      ? [...new Set(record.days.filter((day): day is string => typeof day === "string" && /^\d{4}-\d{2}-\d{2}$/.test(day)))]
          .slice(-DAYS_BEFORE_ASKING)
      : [];
    const sends =
      typeof record.sends === "number" && Number.isFinite(record.sends) && record.sends > 0
        ? Math.floor(record.sends)
        : 0;
    return { days, sends };
  } catch {
    return { days: [], sends: 0 };
  }
}

/** Whether the user permanently dismissed or completed the prompt. Returns `true` if storage cannot be read. */
export function isSupportPromptDismissed(storage: StorageLike | null = defaultStorage()): boolean {
  if (!storage) return true;
  try {
    return storage.getItem(DISMISSED_KEY) === "true";
  } catch {
    return true;
  }
}

/** Permanently dismisses the prompt in storage. */
export function dismissSupportPrompt(storage: StorageLike | null = defaultStorage()): void {
  if (!storage) return;
  try {
    storage.setItem(DISMISSED_KEY, "true");
  } catch {
    // Storage may be unavailable or full; ignore.
  }
}

/**
 * Records one accepted outgoing message and reports whether the 3-day / 15-send
 * engagement threshold is met. Fails closed when storage cannot be read or written
 * so a prompt is never shown if dismissal could not be persisted.
 */
export function recordSupportMessageSent(options?: {
  storage?: StorageLike | null;
  now?: Date;
}): { eligible: boolean; writable: boolean } {
  const storage = options?.storage === undefined ? defaultStorage() : options.storage;
  if (!storage) return { eligible: false, writable: false };
  try {
    if (storage.getItem(DISMISSED_KEY) === "true") {
      return { eligible: false, writable: true };
    }
    const current = parseUsage(storage.getItem(USAGE_KEY));
    const today = localDateKey(options?.now);
    const days = current.days.includes(today)
      ? current.days
      : [...current.days, today].slice(-DAYS_BEFORE_ASKING);
    const sends = current.sends + 1;
    storage.setItem(USAGE_KEY, JSON.stringify({ days, sends }));
    return {
      eligible: days.length >= DAYS_BEFORE_ASKING && sends >= SENDS_BEFORE_ASKING,
      writable: true,
    };
  } catch {
    return { eligible: false, writable: false };
  }
}

/**
 * Resolves to `true` once the focused window has had no user input for `idleMs`.
 * Resolves to `false` if cancelled through `signal`.
 */
export function waitForIdle(options?: {
  idleMs?: number;
  window?: WindowLike;
  document?: DocumentLike;
  signal?: AbortSignal;
}): Promise<boolean> {
  const idleMs = options?.idleMs ?? IDLE_MS;
  const win: WindowLike | undefined = options?.window ?? globalThis.window;
  const doc: DocumentLike | undefined = options?.document ?? globalThis.document;
  const signal = options?.signal;

  if (!win || !doc || signal?.aborted) return Promise.resolve(false);

  return new Promise<boolean>((resolve) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let settled = false;

    const cleanup = () => {
      clearTimeout(timer);
      for (const type of ACTIVITY_EVENTS) {
        win.removeEventListener(type, restart, true);
      }
      signal?.removeEventListener("abort", onAbort);
    };

    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    };

    const onAbort = () => finish(false);

    const restart: EventListener = () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        // Input inside another app or an embedded frame never reaches this window;
        // the next focus or activity event restarts the timer.
        if (!doc.hasFocus() || doc.activeElement?.matches?.("iframe, webview")) return;
        finish(true);
      }, idleMs);
    };

    signal?.addEventListener("abort", onAbort, { once: true });
    for (const type of ACTIVITY_EVENTS) {
      win.addEventListener(type, restart, { capture: true, passive: true });
    }
    restart(new Event("init"));
  });
}

/** Detects a short, non-sensitive OS label from the browser or desktop webview navigator. */
export function detectClientPlatform(
  nav: NavigatorLike | undefined = typeof navigator !== "undefined" ? (navigator as NavigatorLike) : undefined,
): string {
  const hint = nav?.userAgentData?.platform?.trim();
  const source = `${hint ?? ""} ${nav?.platform ?? ""} ${nav?.userAgent ?? ""}`;
  if (/iPhone|iPad|iPod|iOS/i.test(source)) return "iOS";
  if (/Android/i.test(source)) return "Android";
  if (/CrOS|Chrome OS/i.test(source)) return "ChromeOS";
  if (/Mac|Darwin/i.test(source)) return "macOS";
  if (/Win/i.test(source)) return "Windows";
  if (/Linux/i.test(source)) return "Linux";
  return hint || "";
}

function targetMatchesPlatform(platform: string, target: string): boolean {
  const lowerPlatform = platform.toLowerCase();
  const lowerTarget = target.toLowerCase();
  if (lowerPlatform === "macos") return lowerTarget.includes("darwin") || lowerTarget.includes("apple");
  if (lowerPlatform === "windows") return lowerTarget.includes("windows");
  if (lowerPlatform === "linux") return lowerTarget.includes("linux");
  return false;
}

/**
 * Builds the GitHub Bug Report URL pre-filled with non-sensitive version and platform
 * metadata only. Never includes account IDs, endpoints, paths, or message data.
 */
export function buildIssueUrl(env: SupportEnvironment): string {
  const desktopVer = env.desktop ? env.desktopVersion?.trim() || "" : "";
  const hubVer = env.hubVersion?.version?.trim() || "";
  const hubTarget = env.hubVersion?.target?.trim() || "";
  const clientPlatform = (env.clientPlatform ?? detectClientPlatform()).trim();

  let versionLine = "";
  if (desktopVer && hubVer && desktopVer !== hubVer) {
    versionLine = `desktop ${desktopVer} (hub ${hubVer})`;
  } else {
    versionLine = desktopVer || hubVer;
  }

  const installedWith = env.desktop ? "desktop app" : "web UI";

  let platformLine = "";
  if (clientPlatform && hubTarget) {
    platformLine = targetMatchesPlatform(clientPlatform, hubTarget)
      ? `${clientPlatform} (${hubTarget})`
      : `${clientPlatform} (hub: ${hubTarget})`;
  } else {
    platformLine = clientPlatform || hubTarget;
  }

  const environment = [
    `- agent-wechat version (\`wx version\`): ${versionLine}`,
    `- Installed with (npm, release binary, Docker, desktop app, OpenClaw managed mode, source): ${installedWith}`,
    `- OS and architecture: ${platformLine}`,
    `- Logged in with (\`wx auth login\`, or \`--official\`): `,
  ].join("\n");

  const params = new URLSearchParams({
    template: GITHUB_ISSUE_TEMPLATE,
    environment,
  });
  return `${GITHUB_REPO_URL}/issues/new?${params.toString()}`;
}

export const BLOCKING_OVERLAY_SELECTOR =
  '[role="dialog"][aria-modal="true"], dialog[open], .overlay-backdrop, .desktop-update-notice, .call-preview-popover';

/** Whether a modal dialog, native `<dialog>`, popover, backdrop, or desktop update banner is currently on screen. */
export function hasBlockingOverlay(doc: DocumentLike | undefined = globalThis.document): boolean {
  if (!doc?.querySelector) return false;
  return doc.querySelector(BLOCKING_OVERLAY_SELECTOR) !== null;
}

/**
 * Simulator-only visual preview hook used by Argos scenes. Ignored in all real hubs.
 */
export function readSupportPromptPreview(simulator: boolean): SupportPromptStep | null {
  if (!simulator) return null;
  const candidate = (globalThis as Record<string, unknown>).__agentWechatSupportPromptPreview;
  if (typeof candidate === "string" && VALID_PREVIEW_STEPS.has(candidate as SupportPromptStep)) {
    return candidate as SupportPromptStep;
  }
  return null;
}
