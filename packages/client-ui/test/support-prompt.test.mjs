import assert from "node:assert/strict";
import test from "node:test";
import {
  BLOCKING_OVERLAY_SELECTOR,
  buildIssueUrl,
  DAYS_BEFORE_ASKING,
  detectClientPlatform,
  DISMISSED_KEY,
  dismissSupportPrompt,
  GITHUB_ISSUE_TEMPLATE,
  GITHUB_REPO_URL,
  hasBlockingOverlay,
  isSupportPromptDismissed,
  localDateKey,
  readSupportPromptPreview,
  recordSupportMessageSent,
  SENDS_BEFORE_ASKING,
  USAGE_KEY,
  waitForIdle,
} from "../src/support-prompt.ts";

function memoryStorage({ writable = true, readable = true } = {}) {
  const map = new Map();
  return {
    map,
    getItem(key) {
      if (!readable) throw new Error("SecurityError");
      return map.get(key) ?? null;
    },
    setItem(key, value) {
      if (!writable) throw new Error("QuotaExceededError");
      map.set(key, String(value));
    },
  };
}

test("requires both 3 distinct local days and 15 sent messages before becoming eligible", () => {
  const storage = memoryStorage();
  const day1 = new Date(2026, 10, 1, 10, 0, 0);
  const day2 = new Date(2026, 10, 2, 10, 0, 0);
  const day3 = new Date(2026, 10, 3, 10, 0, 0);

  // 15 sends across only 2 days is not eligible.
  for (let i = 0; i < 10; i++) {
    assert.deepEqual(recordSupportMessageSent({ storage, now: day1 }), {
      eligible: false,
      writable: true,
    });
  }
  for (let i = 0; i < 6; i++) {
    assert.deepEqual(recordSupportMessageSent({ storage, now: day2 }), {
      eligible: false,
      writable: true,
    });
  }

  // First send on day 3 crosses both thresholds.
  assert.deepEqual(recordSupportMessageSent({ storage, now: day3 }), {
    eligible: true,
    writable: true,
  });

  // Also test the opposite order: 3 days with fewer than 15 sends.
  const storage2 = memoryStorage();
  assert.equal(recordSupportMessageSent({ storage: storage2, now: day1 }).eligible, false);
  assert.equal(recordSupportMessageSent({ storage: storage2, now: day2 }).eligible, false);
  for (let i = 0; i < SENDS_BEFORE_ASKING - 3; i++) {
    assert.equal(recordSupportMessageSent({ storage: storage2, now: day3 }).eligible, false);
  }
  assert.equal(recordSupportMessageSent({ storage: storage2, now: day3 }).eligible, true);
});

test("keeps usage and dismissal in separate keys and never re-prompts once dismissed", () => {
  const storage = memoryStorage();
  const day1 = new Date(2026, 10, 1);
  const day2 = new Date(2026, 10, 2);
  const day3 = new Date(2026, 10, 3);
  storage.setItem(
    USAGE_KEY,
    JSON.stringify({ days: [localDateKey(day1), localDateKey(day2)], sends: SENDS_BEFORE_ASKING }),
  );
  assert.equal(isSupportPromptDismissed(storage), false);

  dismissSupportPrompt(storage);
  assert.equal(storage.getItem(DISMISSED_KEY), "true");
  assert.equal(isSupportPromptDismissed(storage), true);
  assert.deepEqual(recordSupportMessageSent({ storage, now: day3 }), {
    eligible: false,
    writable: true,
  });
});

test("fails closed when storage cannot be read or written", () => {
  const unwritable = memoryStorage({ writable: false });
  assert.deepEqual(recordSupportMessageSent({ storage: unwritable }), {
    eligible: false,
    writable: false,
  });

  const unreadable = memoryStorage({ readable: false });
  assert.equal(isSupportPromptDismissed(unreadable), true);
  assert.deepEqual(recordSupportMessageSent({ storage: unreadable }), {
    eligible: false,
    writable: false,
  });
  assert.equal(isSupportPromptDismissed(null), true);
});

test("recovers gracefully from malformed usage storage and caps stored days", () => {
  const storage = memoryStorage();
  storage.setItem(USAGE_KEY, "{not-valid-json");
  assert.deepEqual(recordSupportMessageSent({ storage, now: new Date(2026, 10, 1) }), {
    eligible: false,
    writable: true,
  });

  for (let d = 1; d <= 7; d++) {
    recordSupportMessageSent({ storage, now: new Date(2026, 10, d) });
  }
  const saved = JSON.parse(storage.getItem(USAGE_KEY));
  assert.equal(saved.days.length, DAYS_BEFORE_ASKING);
});

test("waitForIdle resets on activity, waits for window focus and non-iframe focus, and supports abort", async () => {
  const events = new EventTarget();
  let focused = true;
  let activeElement = null;
  const win = {
    addEventListener: events.addEventListener.bind(events),
    removeEventListener: events.removeEventListener.bind(events),
  };
  const doc = {
    hasFocus: () => focused,
    get activeElement() {
      return activeElement;
    },
  };

  // Activity pushes back resolution.
  const idlePromise = waitForIdle({ idleMs: 35, window: win, document: doc });
  let resolved = false;
  void idlePromise.then(() => {
    resolved = true;
  });
  await new Promise((r) => setTimeout(r, 20));
  events.dispatchEvent(new Event("keydown"));
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(resolved, false);
  assert.equal(await idlePromise, true);

  // Unfocused window or active iframe does not resolve until refocused outside frame.
  focused = false;
  let focusResolved = false;
  const focusPromise = waitForIdle({ idleMs: 20, window: win, document: doc });
  void focusPromise.then(() => {
    focusResolved = true;
  });
  await new Promise((r) => setTimeout(r, 35));
  assert.equal(focusResolved, false);

  focused = true;
  activeElement = { matches: (selector) => selector.includes("iframe") };
  events.dispatchEvent(new Event("focus"));
  await new Promise((r) => setTimeout(r, 35));
  assert.equal(focusResolved, false);

  activeElement = null;
  events.dispatchEvent(new Event("pointerdown"));
  assert.equal(await focusPromise, true);

  // Aborting resolves to false immediately.
  const controller = new AbortController();
  const abortPromise = waitForIdle({
    idleMs: 10_000,
    window: win,
    document: doc,
    signal: controller.signal,
  });
  controller.abort();
  assert.equal(await abortPromise, false);
});

test("detectClientPlatform maps common navigator signatures cleanly", () => {
  assert.equal(detectClientPlatform({ userAgentData: { platform: "macOS" } }), "macOS");
  assert.equal(detectClientPlatform({ userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }), "Linux");
  assert.equal(detectClientPlatform({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }), "Windows");
  assert.equal(detectClientPlatform({ userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0)" }), "iOS");
  assert.equal(detectClientPlatform({ userAgent: "Mozilla/5.0 (Linux; Android 15)" }), "Android");
  assert.equal(detectClientPlatform({}), "");
});

test("buildIssueUrl pre-fills non-sensitive version and platform for desktop and web UI", () => {
  const desktopLocal = new URL(
    buildIssueUrl({
      desktop: true,
      desktopVersion: "0.7.0-beta.0",
      hubVersion: { version: "0.7.0-beta.0", target: "aarch64-apple-darwin" },
      clientPlatform: "macOS",
    }),
  );
  assert.equal(`${desktopLocal.origin}${desktopLocal.pathname}`, `${GITHUB_REPO_URL}/issues/new`);
  assert.equal(desktopLocal.searchParams.get("template"), GITHUB_ISSUE_TEMPLATE);
  const envLocal = desktopLocal.searchParams.get("environment") ?? "";
  assert.match(envLocal, /agent-wechat version \(`wx version`\): 0\.7\.0-beta\.0/);
  assert.match(envLocal, /Installed with .*: desktop app/);
  assert.match(envLocal, /OS and architecture: macOS \(aarch64-apple-darwin\)/);

  const desktopRemote = new URL(
    buildIssueUrl({
      desktop: true,
      desktopVersion: "0.7.1",
      hubVersion: { version: "0.7.0", target: "x86_64-unknown-linux-gnu" },
      clientPlatform: "macOS",
    }),
  );
  const envRemote = desktopRemote.searchParams.get("environment") ?? "";
  assert.match(envRemote, /desktop 0\.7\.1 \(hub 0\.7\.0\)/);
  assert.match(envRemote, /macOS \(hub: x86_64-unknown-linux-gnu\)/);

  const web = new URL(
    buildIssueUrl({
      desktop: false,
      hubVersion: { version: "0.7.0-beta.0", target: "x86_64-unknown-linux-gnu" },
      clientPlatform: "Linux",
    }),
  );
  const envWeb = web.searchParams.get("environment") ?? "";
  assert.match(envWeb, /Installed with .*: web UI/);
  assert.match(envWeb, /OS and architecture: Linux \(x86_64-unknown-linux-gnu\)/);
});

test("hasBlockingOverlay and simulator preview hook respect boundaries", () => {
  assert.match(BLOCKING_OVERLAY_SELECTOR, /dialog\[open\]/);
  assert.match(BLOCKING_OVERLAY_SELECTOR, /\.call-preview-popover/);
  assert.match(BLOCKING_OVERLAY_SELECTOR, /\.desktop-update-notice/);
  let queried = "";
  assert.equal(
    hasBlockingOverlay({
      hasFocus: () => true,
      querySelector: (selector) => {
        queried = selector;
        return null;
      },
    }),
    false,
  );
  assert.equal(queried, BLOCKING_OVERLAY_SELECTOR);
  assert.equal(hasBlockingOverlay({ hasFocus: () => true, querySelector: () => ({}) }), true);

  const prev = globalThis.__agentWechatSupportPromptPreview;
  try {
    globalThis.__agentWechatSupportPromptPreview = "ask";
    assert.equal(readSupportPromptPreview(false), null);
    assert.equal(readSupportPromptPreview(true), "ask");
    globalThis.__agentWechatSupportPromptPreview = "invalid";
    assert.equal(readSupportPromptPreview(true), null);
  } finally {
    if (prev === undefined) delete globalThis.__agentWechatSupportPromptPreview;
    else globalThis.__agentWechatSupportPromptPreview = prev;
  }
});
