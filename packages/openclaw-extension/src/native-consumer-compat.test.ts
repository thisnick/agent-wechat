// Upgrade compatibility: a consumer file written by 1.0.0-beta.13 must keep
// loading and dispatching under every later release. The file is frozen; the
// daemon messages it is checked against are the current projection, pinned by
// wechat-core's native_compat_tests. See RELEASING.md, "Upgrade compatibility".
import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { dispatchNativeMessage } from "./native-monitor.ts";
import { prepareNativeAttachment } from "./native-media.ts";
import { resolveWeChatAccount } from "./types.ts";

const frozen = readFileSync(new URL("../test/fixtures/native-compat/consumer-state-1.0.0-beta.13.json", import.meta.url), "utf8");
const daemon: Record<string, NativeMessage> = JSON.parse(readFileSync(new URL("../../native-client-rust/crates/wechat-core/tests/fixtures/native-compat/messages.json", import.meta.url), "utf8"));
const buffered = ["text", "quoteText", "quoteImage", "quoteLink", "link", "sticker", "image"].map(name => daemon[name].id);

function restore(t: any) {
  const directory = mkdtempSync(join(tmpdir(), "native-consumer-compat-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const { serverId, accountId } = JSON.parse(frozen);
  const file = join(directory, `${createHash("sha256").update(`${serverId}\0${accountId}`).digest("hex")}.json`);
  writeFileSync(file, frozen); chmodSync(file, 0o600);
  const cfg: any = { channels: { "agent-wechat": { backend: "native", dmPolicy: "open", allowFrom: ["*"], groupPolicy: "open", groupAllowFrom: ["*"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId, token: "synthetic-native-token-".repeat(3), consumerStateDir: directory } } } };
  return { directory, serverId, accountId, cfg, account: resolveWeChatAccount(cfg)!, state: new NativeConsumerState(directory, serverId, accountId) };
}

test("a consumer file written by 1.0.0-beta.13 loads with its records intact", t => {
  const { state } = restore(t);
  assert.equal(state.items().length, 9);
  const answered = state.get(daemon.answeredMention.id)!;
  assert.equal(answered.state, "completed"); assert.equal(answered.readPending, true); assert.ok(answered.dispatchManifest?.runId);
  const held = state.get(daemon.heldMention.id)!;
  assert.equal(held.state, "needs_review"); assert.ok(held.dispatchManifest?.runId);
  state.accept("10", daemon.mention, "live");
  assert.deepEqual(state.groupHistory(daemon.mention.threadId, daemon.mention.id, 50).map(item => item.message.id), buffered);
});

test("a mention dispatches 1.0.0-beta.13 group context against the current daemon messages", async t => {
  const value = restore(t);
  const runs: any[] = [];
  const core: any = { channel: {
    commands: { shouldHandleTextCommands: () => false, isControlCommandMessage: () => false, shouldComputeCommandAuthorized: () => false },
    pairing: { readAllowFromStore: async () => [] },
    inbound: { ingress: { resolveStable: async (params: any) => ({ ingress: { admission: params.policy.activation?.requireMention && !params.mentionFacts?.wasMentioned ? "skip" : "dispatch", reasonCode: "fixture" }, activationAccess: {}, commandAccess: {} }) } },
    routing: { resolveAgentRoute: () => ({ agentId: "groups", sessionKey: "agent:groups:agent-wechat:group:room@chatroom", accountId: "default" }) },
    session: { resolveStorePath: () => "/unused", recordInboundSession: async () => {} },
    reply: { resolveEnvelopeFormatOptions: () => ({}), formatAgentEnvelope: ({ body }: any) => body, finalizeInboundContext: (ctx: any) => ctx,
      dispatchReplyWithBufferedBlockDispatcher: async (params: any) => { runs.push(params); params.replyOptions.onPreparedAgentRunStart?.("run_compat"); } },
  } };
  // Every stored message is re-read before dispatch; the daemon answers with its current projection.
  const current = new Map(Object.values(daemon).map(message => [message.id, message]));
  const client = { message: async (_account: string, id: string) => structuredClone(current.get(id)!),
    mediaAssets: async () => ({ items: [] }) } as unknown as NativeWeChatClient;
  value.state.accept("10", daemon.mention, "live");
  const result = await dispatchNativeMessage({ ...value, client, core, message: daemon.mention,
    prepareAttachment: params => prepareNativeAttachment({ ...params, download: false }),
    createPipeline: (() => ({ onModelSelected: () => {} })) as any });
  assert.equal(result, "completed", "stored 1.0.0-beta.13 messages no longer match the daemon's current message JSON");
  const ctx = runs[0].ctx;
  assert.equal(ctx.WasMentioned, true); assert.equal(ctx.InboundHistory.length, buffered.length);
  for (const text of ["Shall we go on Saturday?", "Example Venue"]) assert.ok(ctx.BodyForAgent.includes(text), text);
  assert.deepEqual(value.state.get(daemon.mention.id)!.dispatchManifest!.historyInputs!.map(input => input.id), buffered);
  assert.deepEqual(value.state.groupHistory(daemon.mention.threadId, daemon.mention.id, 50), []);
  // The current version's write of the restored file must load again.
  assert.equal(new NativeConsumerState(value.directory, value.serverId, value.accountId).get(daemon.mention.id)?.state, "completed");
});
