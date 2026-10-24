import test from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import type { NativeOperation, NativeWeChatClient } from "@agent-wechat/shared";
import { createNativeLoginTool, nativeLoginWait, registerNativeLoginTool } from "./native-login.ts";
import { resolveWeChatAccount } from "./types.ts";

function fixture() {
  let cfg: any = { channels: { "agent-wechat": { native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: randomUUID(), token: "synthetic-login-token-".repeat(3), consumerStateDir: "/fixture/consumer" } } } };
  const account = resolveWeChatAccount(cfg)!;
  const calls: any[][] = [];
  let state = "qr_ready", starts = 0, operationId = "operation-0", override: Partial<NativeOperation> = {};
  const png = readFileSync(new URL("../test/fixtures/native-model.png", import.meta.url));
  const operation = (): NativeOperation => ({ id: operationId, accountId: account.native!.accountId, kind: "login", state, result: { qrAvailable: true, qrGeneration: 2 }, createdAt: "", updatedAt: "", ...override });
  const client = {
    login: async (id: string, options: unknown, key: string) => { calls.push(["login", id, options, key]); operationId = `operation-${++starts}`; return operation(); },
    operation: async (id: string, op: string) => { calls.push(["operation", id, op]); return operation(); },
    qr: async (...args: unknown[]) => { calls.push(["qr", ...args]); return png; },
    account: async (id: string) => { calls.push(["account", id]); return { id, authenticated: true, connection: "online" }; },
    disconnect: async (id: string) => { calls.push(["disconnect", id]); return { disconnected: true }; },
    logout: async (id: string, key: string) => { calls.push(["logout", id, key]); return { id: "logout-op", state: "queued" }; },
    forgetCredentials: async (id: string, key: string, confirm: true) => { calls.push(["forget", id, key, confirm]); return { id: "forget-op", state: "succeeded" }; },
  } as unknown as NativeWeChatClient;
  const host: any = async () => { calls.push(["host"]); return { client }; };
  const context: any = { getRuntimeConfig: () => cfg, assertInvocationCurrent: () => {} };
  const tool = createNativeLoginTool(account, { host, context });
  const run = (args: unknown, signal?: AbortSignal): Promise<any> => tool.execute("call", args, signal);
  return { account, calls, png, client, context, tool, run, host, config: () => cfg, setConfig: (value: any) => { cfg = value; },
    setState: (value: string) => { state = value; }, setOperation: (value: Partial<NativeOperation>) => { override = value; } };
}

test("wechat_login is explicitly registered and discoverable from other channels", () => {
  const registrations: any[] = [];
  registerNativeLoginTool({ registerTool: (...args: any[]) => registrations.push(args) } as any);
  const [factory, options] = registrations[0];
  assert.equal(factory.contextVersion, 2); assert.deepEqual(options, { names: ["wechat_login"] });
  const f = fixture();
  const tools = factory.create({ ...f.context, messageChannel: "telegram" });
  assert.equal(tools.length, 1); assert.equal(tools[0].name, "wechat_login"); assert.equal(tools[0].catalogMode, "direct-only");
  assert.deepEqual(factory.create({ config: {} }), []);
  f.config().channels["agent-wechat"].enabled = false;
  assert.deepEqual(factory.create(f.context), []);
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  assert.equal(manifest.contracts.tools.filter((name: string) => name === "wechat_login").length, 1);
});

test("start returns the daemon QR image and reuses the same pending login", async () => {
  const f = fixture();
  for (let attempt = 0; attempt < 2; attempt++) {
    const result = await f.run({ action: "start", timeoutMs: 5000 });
    assert.deepEqual(result.details, { connected: false, state: "qr_ready", operationId: "operation-1" });
    assert.match(result.content[0].text, /Scan/);
    assert.deepEqual(result.content[1], { type: "image", mimeType: "image/png", data: f.png.toString("base64") });
  }
  const logins = f.calls.filter(call => call[0] === "login");
  assert.equal(logins.length, 1); assert.equal(logins[0][1], f.account.native!.accountId);
  assert.deepEqual(logins[0][2], { allowQr: true, forceQr: false, timeoutSeconds: 10, computerName: "agent-wechat" });
  assert.match(logins[0][3], /^[0-9a-f-]{36}$/);
  assert.ok(f.calls.filter(call => call[0] === "qr").every(call => call[2] === "operation-1" && call[3] === 2));
});

test("wait follows the existing operation through success and terminal failures without submitting login", async () => {
  const f = fixture();
  assert.equal((await f.run({ action: "wait" })).details.state, "idle");
  assert.equal(f.calls.length, 0);
  await f.run({ action: "start" });
  for (const state of ["succeeded", "failed", "cancelled", "expired", "rejected", "needs_login"]) {
    f.setState(state);
    const result = await f.run({ action: "wait" });
    assert.equal(result.details.state, state); assert.equal(result.details.connected, state === "succeeded");
    assert.equal(result.details.operationId, "operation-1"); assert.equal(result.content.length, 1);
  }
  assert.equal(f.calls.filter(call => call[0] === "login").length, 1);
});

test("phone approval takes precedence over an old QR and force uses QR for a new operation", async () => {
  const f = fixture();
  f.setState("awaiting_approval");
  const approval = await f.run({ action: "start" });
  assert.match(approval.content[0].text, /Approve/); assert.equal(approval.content.length, 1);
  assert.equal(f.calls.some(call => call[0] === "qr"), false);
  const pending = await f.run({ action: "start", force: true });
  assert.equal(pending.details.operationId, "operation-1");
  assert.equal(f.calls.filter(call => call[0] === "login").length, 1);
  f.setState("expired");
  const next = await f.run({ action: "start", force: true });
  assert.equal(next.details.operationId, "operation-2");
  assert.equal(f.calls.filter(call => call[0] === "login")[1][2].forceQr, true);
});

test("status, disconnect, logout and confirmed forgetting use only the configured account", async () => {
  const f = fixture();
  const status = await f.run({ action: "status" });
  assert.equal(status.details.authenticated, true);
  for (const action of ["disconnect", "logout", "forget"]) {
    await f.run({ action: "start" });
    const result = await f.run({ action, ...(action === "forget" ? { confirm: true } : {}) });
    assert.ok(result.details);
    const call = f.calls.find(call => call[0] === action)!;
    assert.equal(call[1], f.account.native!.accountId);
    if (action !== "disconnect") assert.match(call[2], /^[0-9a-f-]{36}$/);
    if (action === "forget") assert.equal(call[3], true);
    assert.equal((await f.run({ action: "wait" })).details.state, "idle");
  }
});

test("invalid login inputs and unconfirmed forgetting fail before opening the daemon", async () => {
  const f = fixture();
  for (const args of [null, [], {}, { action: "invalid" }, { action: "start", accountId: "foreign" }, { action: "start", force: "true" },
    { action: "start", force: null }, { action: "start", timeoutMs: 0 }, { action: "wait", timeoutMs: 300001 }, { action: "start", timeoutMs: 1.5 },
    { action: "forget" }, { action: "forget", confirm: false }, { action: "forget", confirm: "true" }]) await assert.rejects(f.run(args));
  assert.equal(f.calls.length, 0);
});

test("aborted or stale login invocations stop before an authentication side effect", async () => {
  const f = fixture();
  const abort = new AbortController(); abort.abort();
  await assert.rejects(f.run({ action: "start" }, abort.signal)); assert.equal(f.calls.length, 0);
  f.context.assertInvocationCurrent = () => { throw Error("stale turn"); };
  await assert.rejects(f.run({ action: "start" }), /stale turn/); assert.equal(f.calls.length, 0);
  let checks = 0;
  f.context.assertInvocationCurrent = () => { if (++checks >= 3) throw Error("stale after host"); };
  await assert.rejects(f.run({ action: "start" }), /stale after host/);
  assert.equal(f.calls.some(call => call[0] === "login"), false);
});

test("cancelling a wait leaves the submitted login operation available for the next call", async () => {
  const f = fixture(); await f.run({ action: "start" });
  f.setState("awaiting_approval");
  await assert.rejects(f.run({ action: "wait", timeoutMs: 1000 }, AbortSignal.timeout(10)), /abort/i);
  f.setState("succeeded"); assert.equal((await f.run({ action: "wait" })).details.connected, true);
  assert.equal(f.calls.filter(call => call[0] === "login").length, 1);
});

test("a bounded pending wait returns its operation state without replacing the login", async () => {
  const f = fixture(); await f.run({ action: "start" }); f.setState("awaiting_approval");
  const result = await f.run({ action: "wait", timeoutMs: 1000 });
  assert.deepEqual(result.details, { connected: false, state: "awaiting_approval", operationId: "operation-1" });
  assert.match(result.content[0].text, /remains pending/);
  assert.equal(f.calls.filter(call => call[0] === "login").length, 1);
});

test("login operation IDs, account identities and operation kinds cannot be substituted", async () => {
  for (const override of [{ accountId: "foreign" }, { kind: "logout" }]) {
    const f = fixture(); f.setOperation(override);
    await assert.rejects(f.run({ action: "start" }), /identity/);
    assert.equal(f.calls.some(call => call[0] === "qr"), false);
  }
  const f = fixture(); await f.run({ action: "start" }); f.setOperation({ id: "foreign-operation" });
  await assert.rejects(f.run({ action: "wait" }), /identity/);
});

test("runtime configuration changes invalidate retained tools and cannot reuse another daemon's login", async () => {
  const f = fixture(); await f.run({ action: "start" });
  f.config().channels["agent-wechat"].native.endpoint = "http://127.0.0.1:6274";
  await assert.rejects(f.run({ action: "logout" }), /configuration changed/);
  const current = resolveWeChatAccount(f.config())!;
  assert.equal((await nativeLoginWait(current)).state, "idle");
  assert.equal(f.calls.some(call => call[0] === "logout"), false);
});
