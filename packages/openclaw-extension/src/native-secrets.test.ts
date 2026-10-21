import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { bindNativeSecretContext, nativeConfigurationKey, resolveNativeToken } from "./native-secrets.ts";
import { resolveWeChatAccount, type WeChatNativeConfig } from "./types.ts";

const token = "synthetic-native-token-".repeat(3);
const config = (value: WeChatNativeConfig["token"]): WeChatNativeConfig => ({ mode: "external", accountId: "a",
  endpoint: "http://127.0.0.1:6174", consumerStateDir: "/unused", token: value });

test("native tokens resolve explicit environment references through the pinned OpenClaw SDK", async () => {
  for (const value of [{ source: "env", provider: "default", id: "AGENT_WECHAT_TOKEN" } as const, "${AGENT_WECHAT_TOKEN}"]) {
    assert.equal(await resolveNativeToken(config(value), { AGENT_WECHAT_TOKEN: token }), token);
    await assert.rejects(resolveNativeToken(config(value), {}), /could not be resolved/);
  }
});
test("explicit SecretRefs never fall back to an ambient token and errors conceal values and identifiers", async () => {
  const native = config({ source: "env", provider: "default", id: "ABSENT_PRIVATE_IDENTIFIER" });
  await assert.rejects(resolveNativeToken(native, { AGENT_WECHAT_TOKEN: token }), error => {
    assert.ok(error instanceof Error); assert.doesNotMatch(error.message, /ABSENT_PRIVATE_IDENTIFIER|synthetic/); return true;
  });
  for (const invalid of ["short", token + "\nsecond-line", "x".repeat(1025), "界".repeat(40)]) {
    await assert.rejects(resolveNativeToken(config(invalid)), /printable ASCII/);
  }
  await assert.rejects(resolveNativeToken({ ...config(token), tokenFile: "/private/old-token" } as any), /tokenFile has been removed/);
});
test("provider configuration is bound for plugin and channel settings without serializing the root config", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-secret-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "provider-secret"); await writeFile(path, token + "\n", { mode: 0o600 });
  const providers = { providers: { fixture: { source: "file", path, mode: "singleValue" } } };
  for (const location of ["plugin", "channel"]) {
    const native = config({ source: "file", provider: "fixture", id: "value" });
    const root = { secrets: providers, ...(location === "channel" ? { channels: { "agent-wechat": { native } } }
      : { plugins: { entries: { "agent-wechat": { config: { native } } } } }) };
    const account = resolveWeChatAccount(root)!;
    assert.equal(await resolveNativeToken(account.native!), token);
    assert.doesNotMatch(JSON.stringify(account.native), /provider-secret|providers/);
    assert.doesNotMatch(nativeConfigurationKey(account.native!), /synthetic|provider-secret/);
  }
});
test("configured environment provider restrictions remain authoritative and provider changes require a host generation", async () => {
  const native = config({ source: "env", provider: "fixture", id: "AGENT_WECHAT_TOKEN" });
  const allowed = bindNativeSecretContext(native, { secrets: { providers: { fixture: { source: "env", allowlist: ["AGENT_WECHAT_TOKEN"] } } } }, "channels.agent-wechat.native");
  const denied = bindNativeSecretContext(native, { secrets: { providers: { fixture: { source: "env", allowlist: [] } } } }, "channels.agent-wechat.native");
  assert.equal(await resolveNativeToken(allowed, { AGENT_WECHAT_TOKEN: token }), token);
  await assert.rejects(resolveNativeToken(denied, { AGENT_WECHAT_TOKEN: token }), /could not be resolved/);
  assert.notEqual(nativeConfigurationKey(allowed), nativeConfigurationKey(denied));
});
