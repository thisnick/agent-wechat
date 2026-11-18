// Upgrade compatibility: a consumer file written by 1.0.0-beta.13 must keep
// loading under every later release, and the group context it buffered must
// still reach the next mention. The file is frozen; the daemon messages that
// context is now read from are the current projection, pinned by wechat-core's
// native_compat_tests. See RELEASING.md, "Upgrade compatibility".
import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { dispatchNativeMessage, readNativeGroupHistory } from "./native-monitor.ts";
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

test("a consumer file written by 1.0.0-beta.13 migrates, keeping its pending read and group context position", t => {
  const { state } = restore(t);
  // Only unfinished work is kept; none of its messages was still waiting.
  assert.equal(state.items().length, 0); assert.deepEqual(state.heldOnOpen, []);
  assert.equal(state.readPending(daemon.answeredMention.threadId), true);
  assert.deepEqual(state.groupBoundary(daemon.mention.threadId), { cursor: "2", createdAt: daemon.text.createdAt });
  const bytes = readFileSync(state.file, "utf8");
  assert.equal(JSON.parse(bytes).schemaVersion, 2); assert.ok(!bytes.includes("Shall we go on Saturday?"));
});

test("a mention gets the 1.0.0-beta.13 group context, read from the current daemon messages", async t => {
  const value = restore(t);
  const runs: any[] = []; let custody: any;
  const core: any = { channel: {
    commands: { shouldHandleTextCommands: () => false, isControlCommandMessage: () => false, shouldComputeCommandAuthorized: () => false },
    pairing: { readAllowFromStore: async () => [] },
    inbound: { ingress: { resolveStable: async (params: any) => ({ ingress: { admission: params.policy.activation?.requireMention && !params.mentionFacts?.wasMentioned ? "skip" : "dispatch", reasonCode: "fixture" }, activationAccess: {}, commandAccess: {} }) } },
    routing: { resolveAgentRoute: () => ({ agentId: "groups", sessionKey: "agent:groups:agent-wechat:group:room@chatroom", accountId: "default" }) },
    session: { resolveStorePath: () => "/unused", recordInboundSession: async () => {} },
    reply: { resolveEnvelopeFormatOptions: () => ({}), formatAgentEnvelope: ({ body }: any) => body, finalizeInboundContext: (ctx: any) => ctx,
      dispatchReplyWithBufferedBlockDispatcher: async (params: any) => { runs.push(params); params.replyOptions.onPreparedAgentRunStart?.("run_compat");
        custody = structuredClone(value.state.get(daemon.mention.id)); } },
  } };
  // The daemon answers with its current projection: single reads and the chat's history, newest first.
  const current = new Map(Object.values(daemon).map(message => [message.id, message]));
  const history = Object.values(daemon).filter(message => message.threadId === daemon.mention.threadId)
    .sort((a, b) => Date.parse(b.createdAt!) - Date.parse(a.createdAt!));
  const client = { message: async (_account: string, id: string) => structuredClone(current.get(id)!),
    messages: async () => ({ items: structuredClone(history), nextCursor: null, coverage: {}, sourceGeneration: "fixture" }),
    mediaAssets: async () => ({ items: [] }) } as unknown as NativeWeChatClient;
  value.state.accept("10", daemon.mention, "live");
  const result = await dispatchNativeMessage({ ...value, client, core, message: daemon.mention,
    prepareAttachment: params => prepareNativeAttachment({ ...params, download: false }),
    createPipeline: (() => ({ onModelSelected: () => {} })) as any });
  assert.equal(result, "completed");
  const ctx = runs[0].ctx;
  assert.equal(ctx.WasMentioned, true); assert.equal(ctx.InboundHistory.length, buffered.length);
  for (const text of ["Shall we go on Saturday?", "Example Venue"]) assert.ok(ctx.BodyForAgent.includes(text), text);
  assert.deepEqual(custody.dispatchManifest.historyInputs.map((input: any) => input.id), buffered);
  assert.equal(value.state.groupBoundary(daemon.mention.threadId)?.messageId, daemon.mention.id);
  // The current version's write of the restored file must load again.
  assert.equal(new NativeConsumerState(value.directory, value.serverId, value.accountId).readPending(daemon.mention.threadId), true);
});

// 2.4.2 through 2.7.0 wrote the last version 1 files, so most upgrades start
// from this shape. The file is frozen; it was written by that release's writer.
const frozen270 = readFileSync(new URL("../test/fixtures/native-compat/consumer-state-2.7.0.json", import.meta.url), "utf8");
test("a consumer file written by 2.7.0 converts without losing its place, its pending reads or its group context", async t => {
  const directory = mkdtempSync(join(tmpdir(), "native-consumer-compat-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const file = join(directory, `${createHash("sha256").update("fixture-daemon\0a").digest("hex")}.json`);
  writeFileSync(file, frozen270); chmodSync(file, 0o600);
  const earlier = JSON.parse(frozen270).items as Record<string, { message: NativeMessage }>;
  const state = new NativeConsumerState(directory, "fixture-daemon", "a", { now: Date.parse("2026-11-17T13:00:00Z") });
  assert.equal(state.cursor, "13");
  // Only the recent live incoming message still waiting is kept; the interrupted turn is reported, not repeated.
  assert.deepEqual(state.items().map(item => [item.id, item.state, item.cursor, item.serverId]), [["queued", "received", "7", earlier.queued.message.serverId]]);
  assert.deepEqual(state.heldOnOpen, ["interrupted"]); assert.equal(state.skipped.old, 1);
  assert.equal(state.readPending("wxid_peer"), true); assert.equal(state.readPending("room@chatroom"), true);
  assert.deepEqual(state.groupBoundary("room@chatroom"), { cursor: "2", createdAt: earlier["group-context-1"].message.createdAt });
  const bytes = readFileSync(file, "utf8");
  assert.equal(JSON.parse(bytes).schemaVersion, 2);
  for (const { message } of Object.values(earlier)) if (message.body) assert.ok(!bytes.includes(message.body), message.id);
  assert.equal(readFileSync(`${file}.v1-backup`, "utf8"), frozen270);
  // The next mention gets the context 2.7.0 had buffered, read from the daemon's messages.
  const mention = { ...earlier["group-trigger"].message, id: "next-mention", serverId: "7000000000000000100", createdAt: "2026-11-17T12:59:00Z" };
  state.accept("14", mention, "live");
  const messages = [...Object.values(earlier).map(item => item.message), mention].filter(message => message.threadId === "room@chatroom");
  const client = { messages: async () => ({ items: [...messages].reverse(), nextCursor: null, coverage: {}, sourceGeneration: "fixture" }) } as unknown as NativeWeChatClient;
  assert.deepEqual((await readNativeGroupHistory({ client, state, message: mention, limit: 50, isCommand: () => false })).map(message => message.id), ["group-context-1", "group-context-2"]);
  // A later start reads the new format and does not report the interrupted turn again.
  const reopened = new NativeConsumerState(directory, "fixture-daemon", "a");
  assert.deepEqual(reopened.heldOnOpen, []); assert.equal(reopened.cursor, "14"); assert.equal(readFileSync(`${file}.v1-backup`, "utf8"), frozen270);
});
