import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { nativeDispatchManifest, observeNativeModelCall, observeNativePreparedInput, registerNativePreparedBoundary, trackNativePreparedText } from "./native-prepared-boundary.ts";

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
