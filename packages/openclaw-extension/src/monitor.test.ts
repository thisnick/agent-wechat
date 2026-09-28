import test, { after, type TestContext } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { setTimeout as delay } from "node:timers/promises";
import { build } from "esbuild";
import { MonitorStateStore } from "./monitor-state.ts";
import type { ResolvedWeChatAccount } from "./types.ts";

// Bundle the real monitor and shared HTTP client just as the shipped extension does.
// Only the reply-pipeline SDK boundary is stubbed; HTTP and model calls are fakes.
const require = createRequire(import.meta.url);
const result = await build({
  stdin: {
    contents: 'export { startWeChatMonitor } from "./monitor.ts"; export { setWeChatRuntime } from "./runtime.ts";',
    resolveDir: dirname(fileURLToPath(import.meta.url)),
    loader: "ts",
  },
  bundle: true, format: "esm", platform: "node", write: false,
  plugins: [{ name: "test-sdk-boundary", setup(builder) {
    builder.onResolve({ filter: /^openclaw\// }, args => args.path.endsWith("/channel-reply-pipeline")
      ? { path: args.path, namespace: "fake-pipeline" }
      : { path: require.resolve(args.path), external: true });
    builder.onLoad({ filter: /.*/, namespace: "fake-pipeline" }, () => ({ contents: "export const createChannelReplyPipeline = () => ({});" }));
  } }],
});
const bundleDir = await mkdtemp(join(tmpdir(), "wechat-monitor-bundle-"));
after(() => rm(bundleDir, { recursive: true, force: true }));
const bundlePath = join(bundleDir, "monitor.mjs");
await writeFile(bundlePath, result.outputFiles[0].text);
const { startWeChatMonitor, setWeChatRuntime } = await import(pathToFileURL(bundlePath).href);

async function until(check: () => boolean) {
  for (let i = 0; i < 400; i++) {
    if (check()) return;
    await delay(5);
  }
  assert.fail("condition was not reached");
}

async function harness(t: TestContext, options: { loggedOut?: boolean; abortOnDispatch?: boolean } = {}) {
  const directory = await mkdtemp(join(tmpdir(), "wechat-monitor-test-"));
  const controller = new AbortController();
  const store = new MonitorStateStore(directory, "test");
  await store.save(new Map([["group@chatroom", 1], ["private", 1]]), new Map());
  const account: ResolvedWeChatAccount = {
    accountId: "test", enabled: true, serverUrl: "http://unused.invalid", dmPolicy: "open", allowFrom: [],
    groupPolicy: "open", groupAllowFrom: [], groups: {}, pollIntervalMs: 5, authPollIntervalMs: 60000, mediaMaxMb: 10,
  };
  const delivered: string[] = [];
  const errors: string[] = [];
  let polls = 0;
  let mediaRequests = 0;
  let mediaAborted = false;
  let authChecks = 0;
  const response = (value: unknown) => new Response(JSON.stringify(value), { status: 200 });
  t.mock.method(globalThis, "fetch", async (input: string, init: RequestInit) => {
    const url = new URL(input);
    if (url.pathname === "/api/status/auth") {
      authChecks++;
      return response({ status: options.loggedOut ? "logged_out" : "logged_in" });
    }
    if (url.pathname === "/api/chats") {
      polls++;
      return response([
        { id: "group@chatroom", name: "Group", lastMsgLocalId: 2, unreadCount: 1 },
        { id: "private", name: "Peer", lastMsgLocalId: delivered.length ? 3 : 2, unreadCount: delivered.length ? 0 : 1 },
      ]);
    }
    if (url.pathname.endsWith("/open")) return response({ ok: true });
    if (url.pathname.includes("/media/")) {
      mediaRequests++;
      assert.ok(init.signal);
      return new Promise<Response>((_resolve, reject) => {
        const abort = () => { mediaAborted = true; reject(init.signal!.reason); };
        if (init.signal!.aborted) abort();
        else init.signal!.addEventListener("abort", abort, { once: true });
      });
    }
    if (url.pathname.startsWith("/api/messages/")) {
      const group = decodeURIComponent(url.pathname).includes("@chatroom");
      return response([{ localId: group ? 2 : delivered.length ? 3 : 2, type: group ? 3 : 1,
        content: group ? "" : "test", sender: "peer", senderName: "Peer", timestamp: "2026-01-01T00:00:00Z", isSelf: false }]);
    }
    throw new Error(`Unexpected request: ${url.pathname}`);
  });
  setWeChatRuntime({
    state: { resolveStateDir: () => directory }, config: { current: () => ({}) }, system: { enqueueSystemEvent() {} },
    channel: {
      pairing: { readAllowFromStore: async () => [] },
      commands: { shouldHandleTextCommands: () => false, shouldComputeCommandAuthorized: () => false },
      routing: { resolveAgentRoute: () => ({ agentId: "main", sessionKey: "test:private", accountId: "test" }) },
      session: { resolveStorePath: () => directory, readSessionUpdatedAt: () => undefined, recordInboundSession: async () => {} },
      reply: {
        resolveEnvelopeFormatOptions: () => ({}), formatAgentEnvelope: ({ body }: any) => body,
        finalizeInboundContext: (ctx: any) => ctx,
        dispatchReplyWithBufferedBlockDispatcher: async ({ ctx }: any) => {
          delivered.push(ctx.MessageSid);
          if (options.abortOnDispatch) controller.abort();
        },
      },
    },
  });
  const running = startWeChatMonitor({ account, abortSignal: controller.signal, setStatus() {}, log: { error: (e: string) => errors.push(e) } });
  t.after(async () => {
    controller.abort();
    await running;
    await rm(directory, { recursive: true, force: true });
  });
  return { controller, running, delivered, errors, store,
    get polls() { return polls; }, get mediaRequests() { return mediaRequests; },
    get mediaAborted() { return mediaAborted; }, get authChecks() { return authChecks; } };
}

test("a pending group media HTTP request does not block new private messages or duplicate the group batch", { timeout: 10000 }, async t => {
  const h = await harness(t);
  await until(() => h.delivered.length >= 2);
  assert.deepEqual(h.delivered, ["wechat:private:2", "wechat:private:3"]);
  assert.equal(h.mediaRequests, 1);
  assert.ok(h.polls >= 2);
  h.controller.abort();
  await h.running;
  assert.equal(h.mediaAborted, true);
  const state = await h.store.load((_v): _v is never => false);
  assert.equal(state.lastSeen["group@chatroom"], 1, "cancelled preparation preserves cursor");
  assert.equal(state.lastSeen.private, 3);
  assert.deepEqual(h.errors, []);
});

test("a fully delivered batch is saved even when shutdown begins during dispatch", { timeout: 10000 }, async t => {
  const h = await harness(t, { abortOnDispatch: true });
  await h.running;
  assert.deepEqual(h.delivered, ["wechat:private:2"]);
  const state = await h.store.load((_v): _v is never => false);
  assert.equal(state.lastSeen.private, 2);
});

test("logged-out monitor does not read cached chats between auth probes", { timeout: 10000 }, async t => {
  const h = await harness(t, { loggedOut: true });
  await until(() => h.authChecks === 1);
  await delay(40);
  h.controller.abort();
  await h.running;
  assert.equal(h.polls, 0);
  assert.deepEqual(h.delivered, []);
});
