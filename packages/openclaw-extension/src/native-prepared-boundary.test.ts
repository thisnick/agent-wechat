import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage } from "@agent-wechat/shared";
import { NativeConsumerState, type NativeSuppliedAsset } from "./native-consumer-state.ts";
import { invalidateNativePreparedInputs, nativeDispatchManifest, observeNativeModelCall, observeNativePreparedInput, registerNativePreparedBoundary, trackNativePreparedText } from "./native-prepared-boundary.ts";

test("location metadata binds prepared input without changing older message hashes", () => {
  const message = { id: "m", accountId: "a", threadId: "peer", type: 1, body: "existing input" } as NativeMessage;
  const params = { message, inputBody: message.body!, sessionKey: "session", agentId: "agent", channelAccountId: "default", history: [{ ...message, id: "older" }] };
  const manifest = nativeDispatchManifest(params);
  const previousHash = (m: NativeMessage) => createHash("sha256").update(JSON.stringify([m.id, m.version, m.senderId, m.type, m.body, m.mentions, m.serverId, m.appMessageType, m.quote, m.link])).digest("hex");
  assert.equal(manifest.newInputs[0].contentSha256, previousHash(message));
  assert.equal(manifest.historyInputs![0].contentSha256, previousHash(params.history[0]));
  const location = { latitude: 12.5, longitude: -45.25, name: null, address: null, scale: null, mapType: null, poiId: null, coordinateSystem: "unknown" as const };
  const pin = { ...message, type: 48, location };
  const first = nativeDispatchManifest({ ...params, message: pin });
  const changed = nativeDispatchManifest({ ...params, message: { ...pin, location: { ...location, longitude: -46 } } });
  assert.notEqual(first.newInputs[0].contentSha256, changed.newInputs[0].contentSha256);
});

function fixture(t: any, options: { access?: boolean; policy?: "never" | "on_model_dispatch" } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "native-boundary-"));
  const stop = registerNativePreparedBoundary({ config: { plugins: { entries: { "agent-wechat": { hooks: { allowConversationAccess: options.access !== false } } } } }, on: () => {} } as any);
  t.after(() => { stop(); rmSync(directory, { recursive: true, force: true }); });
  const state = new NativeConsumerState(directory, "server", "a");
  const message = { id: "m", accountId: "a", threadId: "peer", type: 1, body: "exact input" } as NativeMessage;
  state.accept("1", message, "live");
  const manifest = nativeDispatchManifest({ message, inputBody: message.body!, sessionKey: "session", agentId: "agent", channelAccountId: "default" });
  state.update("m", { state: "dispatching", dispatchManifest: manifest });
  trackNativePreparedText(manifest, state.preparedCallback(["m"], options.policy ?? "on_model_dispatch"));
  const input: any = { runId: manifest.runId, sessionId: "transcript", provider: "fixture", model: "local", prompt: "[Tue 2026-10-20 12:00 UTC] exact input", historyMessages: [{ role: "custom", customType: "openclaw.runtime-context", details: { source: "openclaw-runtime-context", runtimeContextCarrier: true, fragments: [{ kind: "conversation-data", text: `Conversation info: ⟦openclaw:ctx⟧\n\`\`\`json\n${JSON.stringify({ message_id: manifest.messageSid, chat_id: "wechat:peer" })}\n\`\`\`` }] } }] };
  const context: any = { runId: manifest.runId, sessionId: "transcript", sessionKey: "session", agentId: "agent", accountId: "default", channel: "agent-wechat", chatId: "wechat:peer" };
  const model: any = { runId: manifest.runId, callId: "call", sessionId: "transcript", sessionKey: "session", provider: "fixture", model: "local" };
  return { state, manifest, input, context, model, stop };
}
test("prepared asset manifests admit exactly 512 MiB files and retain 128 MiB non-file limits", () => {
  const message = { id: "m", accountId: "a", threadId: "peer", type: 49, appMessageType: 6 } as NativeMessage;
  for (const kind of ["file", "image", "audio", "video"] as const) {
    const maxBytes = (kind === "file" ? 512 : 128) * 1024 * 1024;
    const asset: NativeSuppliedAsset = { id: "asset", sha256: "0".repeat(64), byteLength: maxBytes, path: "/managed/attachment.bin", mime: "application/octet-stream",
      kind, role: "file_reference", runtimeMedia: false, contextRole: "trigger" };
    const params = { message, inputBody: "[Local file: </managed/attachment.bin>]", sessionKey: "session", agentId: "agent", channelAccountId: "default", assets: [asset] };
    const manifest = nativeDispatchManifest(params);
    assert.equal(manifest.assets[0].byteLength, maxBytes); assert.ok(Object.isFrozen(manifest.assets[0]));
    assert.throws(() => nativeDispatchManifest({ ...params, assets: [{ ...asset, byteLength: maxBytes + 1 }] }), /Invalid native supplied asset manifest/);
    if (kind === "file") assert.throws(() => nativeDispatchManifest({ ...params, assets: [{ ...asset, role: "model_image", runtimeMedia: true }] }), /Invalid native supplied asset manifest/);
  }
});
test("released host needs both exact prepared input and correlated model start", t => {
  const f = fixture(t); assert.ok(Object.isFrozen(f.manifest)); assert.ok(Object.isFrozen(f.manifest.messageIds));
  observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.runId, undefined);
  observeNativePreparedInput(f.input, f.context); assert.equal(f.state.get("m")?.runId, undefined);
  observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.runId, f.manifest.runId); assert.equal(f.state.get("m")?.readPending, true);
});
test("foreign channel/account/session cannot reuse an issued run identity", t => {
  const f = fixture(t);
  for (const change of [{ channel: "other" }, { accountId: "other" }, { sessionKey: "other" }, { agentId: "other" }, { chatId: "wechat:other" }]) {
    observeNativePreparedInput(f.input, { ...f.context, ...change }); observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.runId, undefined);
  }
  observeNativePreparedInput(f.input, f.context); observeNativeModelCall({ ...f.model, sessionId: "other" }, f.context); assert.equal(f.state.get("m")?.runId, undefined);
});
test("omitted rewritten merged and user-forged input facts leave read unchanged", t => {
  const f = fixture(t);
  const other = structuredClone(f.input.historyMessages[0]);
  other.details.fragments[0].text = other.details.fragments[0].text.replace(f.manifest.messageSid, "wechat-native:a:other");
  for (const input of [{ ...f.input, historyMessages: [...f.input.historyMessages, other] }, { ...f.input, prompt: "other input" }, { ...f.input, prompt: f.input.prompt + "\nqueued input" }, { ...f.input, historyMessages: [] }, { ...f.input, historyMessages: [{ ...f.input.historyMessages[0], role: "user" }] }, { ...f.input, historyMessages: [...f.input.historyMessages, ...f.input.historyMessages] }]) {
    observeNativePreparedInput(input, f.context); observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.readPending, undefined);
  }
});
test("missing host grant and shutdown cannot produce a read receipt", t => {
  const f = fixture(t, { access: false }); observeNativePreparedInput(f.input, f.context); observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.readPending, undefined);
  f.stop(); observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.runId, undefined);
});
test("never read policy retains the exact model receipt without local read projection", t => {
  const f = fixture(t, { policy: "never" }); observeNativePreparedInput(f.input, f.context); observeNativeModelCall(f.model, f.context);
  assert.equal(f.state.get("m")?.runId, f.manifest.runId); assert.equal(f.state.get("m")?.readPending, false);
});
test("recall removes a pending model proof only for the matching account and selected input", t => {
  const f = fixture(t); observeNativePreparedInput(f.input, f.context);
  invalidateNativePreparedInputs("other-account", ["m"]); invalidateNativePreparedInputs("a", ["unrelated"]);
  observeNativeModelCall(f.model, f.context); assert.equal(f.state.get("m")?.runId, f.manifest.runId);
  const second = fixture(t); observeNativePreparedInput(second.input, second.context);
  invalidateNativePreparedInputs("a", ["m"]);
  observeNativeModelCall(second.model, second.context); assert.equal(second.state.get("m")?.runId, undefined);
  assert.equal(second.state.get("m")?.readPending, undefined);
});
