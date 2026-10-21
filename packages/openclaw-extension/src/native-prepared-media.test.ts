import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage } from "@agent-wechat/shared";
import { NativeConsumerState, type NativeSuppliedAsset } from "./native-consumer-state.ts";
import { nativeDispatchManifest, observeNativeAgentEnd, observeNativeModelCall, observeNativePreparedInput, registerNativePreparedBoundary, trackNativePreparedInput } from "./native-prepared-boundary.ts";
const bytes = readFileSync(new URL("../test/fixtures/native-model.png", import.meta.url));
const hash = createHash("sha256").update(bytes).digest("hex");
function fixture(t: any, kind: "image" | "file" | "video" = "image", group = false) {
  const directory = mkdtempSync(join(tmpdir(), "native-media-boundary-"));
  const stop = registerNativePreparedBoundary({ config: { plugins: { entries: { "agent-wechat": { hooks: { allowConversationAccess: true } } } } }, on: () => {} } as any);
  t.after(() => { stop(); rmSync(directory, { recursive: true, force: true }); });
  const threadId = group ? "room@chatroom" : "u";
  const message = { id: "m", accountId: "a", threadId, senderId: "sender", version: "v1", type: kind === "image" ? 3 : kind === "file" ? 49 : 43, body: null } as NativeMessage;
  const asset: NativeSuppliedAsset = { id: "asset", sha256: hash, byteLength: bytes.length, mime: kind === "image" ? "image/png" : kind === "video" ? "video/mp4" : "text/plain",
    path: "/managed/owned.png", kind, role: kind === "image" ? "model_image" : "file_reference", runtimeMedia: kind !== "file", contextRole: "trigger" };
  const body = kind === "image" ? "<media:image>" : `[Local file: ${asset.path}]`;
  const manifest = nativeDispatchManifest({ message, inputBody: body, sessionKey: "session", agentId: "main", channelAccountId: "default", policy: { admitted: true }, assets: [asset] });
  const state = new NativeConsumerState(directory, "server", "a"); state.accept("1", message, "live"); state.update("m", { state: "dispatching", dispatchManifest: manifest });
  let receipts = 0; const record = state.preparedCallback(["m"], "on_model_dispatch"); trackNativePreparedInput(manifest, run => { receipts++; return record(run); });
  const header = asset.runtimeMedia ? `[media attached: /tmp/stage/input-owned.png (${asset.mime}) | media://inbound/owned.png]\n` : "";
  const prepared = header + body;
  const ctx: any = { runId: manifest.runId, sessionId: "transcript", sessionKey: "session", agentId: "main", accountId: "default", channel: "agent-wechat", chatId: group ? threadId : "wechat:u", modelProviderId: "fixture", modelId: "local" };
  const input: any = { runId: manifest.runId, sessionId: "transcript", provider: "fixture", model: "local", prompt: `[Tue 2026-10-20 18:00 PDT] ${group ? 'Conversation info: ⟦openclaw:ctx⟧\n```json\n{"sender":{"id":"sender"}}\n```\n\n' : ""}${prepared}`, imagesCount: kind === "image" ? 1 : 0,
    historyMessages: [{ role: "custom", customType: "openclaw.runtime-context", details: { source: "openclaw-runtime-context", runtimeContextCarrier: true, fragments: [{ kind: "conversation-data", text: `Conversation info: ⟦openclaw:ctx⟧\n\`\`\`json\n${JSON.stringify({ message_id: manifest.messageSid, chat_id: `wechat:${threadId}` })}\n\`\`\`` }] } }] };
  const model = { runId: manifest.runId, callId: "call", sessionId: "transcript", sessionKey: "session", provider: "fixture", model: "local" };
  const end: any = { runId: manifest.runId, success: true, messages: [{ role: "user", idempotencyKey: manifest.hostSourceTurnId,
    content: [{ type: "text", text: prepared }, ...(kind === "image" ? [{ type: "image", mimeType: "image/png", data: bytes.toString("base64") }] : [])] }] };
  const start = () => { observeNativePreparedInput(input, ctx); observeNativeModelCall(model, ctx); };
  return { directory, state, manifest, message, asset, body, ctx, input, model, end, start, receipts: () => receipts };
}
test("media manifest freezes exact message version, policy, asset and context roles", t => {
  const f = fixture(t); assert.equal(f.manifest.schemaVersion, 2); assert.equal(f.manifest.newInputs[0].version, "v1"); assert.equal(f.manifest.newInputs[0].role, "trigger");
  assert.ok(Object.isFrozen(f.manifest.assets)); assert.ok(Object.isFrozen(f.manifest.assets[0])); assert.ok(Object.isFrozen(f.manifest.newInputs[0]));
  assert.equal(f.manifest.assets[0].sha256, hash); assert.equal(f.manifest.manifestSha256.length, 64); assert.equal(f.manifest.policySha256.length, 64);
  assert.throws(() => { (f.asset as any).sha256 = "bad"; (f.manifest.assets[0] as any).sha256 = "bad"; }, /read only/);
  assert.equal(f.manifest.assets[0].sha256, hash);
});
test("image count and model start alone cannot mark media read", t => {
  const f = fixture(t); f.start(); assert.equal(f.state.get("m")?.runId, undefined);
  for (const change of [{ messages: [] }, { messages: [{ ...f.end.messages[0], content: [{ type: "text", text: f.end.messages[0].content[0].text }] }] },
    { messages: [{ ...f.end.messages[0], idempotencyKey: "older-identical-source" }] },
    { messages: [{ ...f.end.messages[0], content: [f.end.messages[0].content[0], { type: "image", mimeType: "image/png", data: Buffer.from("wrong").toString("base64") }] }] }]) {
    observeNativeAgentEnd({ ...f.end, ...change }, f.ctx); assert.equal(f.state.get("m")?.readPending, undefined);
  }
  observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.state.get("m")?.readPending, true); assert.equal(f.receipts(), 1);
  f.state.update("m", { readPending: false }); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 1); assert.equal(f.state.get("m")?.readPending, false);
});
test("omitted image and mismatched run/session/provider snapshots remain unread", t => {
  const f = fixture(t); observeNativePreparedInput({ ...f.input, imagesCount: 0 }, f.ctx); observeNativeModelCall(f.model, f.ctx); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 0);
  f.start(); for (const change of [{ runId: "other" }, { sessionId: "other" }, { channel: "other" }, { accountId: "other" }, { modelId: "other" }]) observeNativeAgentEnd(f.end, { ...f.ctx, ...change });
  assert.equal(f.receipts(), 0);
});
test("available file reference requires the exact frozen body and source turn", t => {
  const f = fixture(t, "file"); f.start(); observeNativeAgentEnd({ ...f.end, messages: [{ ...f.end.messages[0], content: "[File unavailable]" }] }, f.ctx); assert.equal(f.receipts(), 0);
  observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.state.get("m")?.readPending, true);
});
test("unknown video additions cannot replace an exact supplied reference", t => {
  const f = fixture(t, "video"); observeNativePreparedInput({ ...f.input, prompt: f.input.prompt + "\n[unproven description]" }, f.ctx); observeNativeModelCall(f.model, f.ctx); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 0);
  f.start(); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 1);
});
test("receipt survives reopen while later provider failure does not undo handoff", t => {
  const f = fixture(t); f.start(); observeNativeAgentEnd({ ...f.end, success: false }, f.ctx);
  const restored = new NativeConsumerState(f.directory, "server", "a"); assert.equal(restored.get("m")?.runId, f.manifest.runId); assert.equal(restored.get("m")?.readPending, true);
});
test("failed durable media receipt cannot expose an in-memory read mutation", t => {
  const f = fixture(t); f.start(); chmodSync(f.directory, 0o755);
  assert.throws(() => observeNativeAgentEnd(f.end, f.ctx), /private/); assert.equal(f.state.get("m")?.readPending, undefined);
  chmodSync(f.directory, 0o700); const restored = new NativeConsumerState(f.directory, "server", "a"); assert.equal(restored.get("m")?.state, "needs_review"); assert.equal(restored.get("m")?.runId, undefined);
});


test("pinned group sender context and raw room identity bind the exact media turn", t => {
  const f = fixture(t, "image", true);
  observeNativePreparedInput({ ...f.input, prompt: f.input.prompt.replace('"sender"}}', '"other"}}') }, f.ctx); observeNativeModelCall(f.model, f.ctx); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 0);
  observeNativePreparedInput(f.input, { ...f.ctx, chatId: "other@chatroom" }); observeNativeModelCall(f.model, f.ctx); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.receipts(), 0);
  f.start(); observeNativeAgentEnd(f.end, f.ctx); assert.equal(f.state.get("m")?.runId, f.manifest.runId); assert.equal(f.receipts(), 1);
});
