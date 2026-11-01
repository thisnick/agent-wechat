import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { NativeCallError, type CallEvent, type CallOffer, type CallReadyScope, type CallView, type NativeCallMediaFrame } from "@agent-wechat/shared";
import { resolveWeChatAccount } from "./types.ts";
import { NativeCallService, type CallControlPort, type CallMediaPort } from "./calls-service.ts";
import { type PreparedCallVoice, type VoiceCallbacks } from "./calls-voice.ts";
import { resolveWeChatCallsConfig } from "./calls-config.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { assertCallingToolAccess } from "./calls-tool.ts";
import { CallPcmOutput } from "./calls-audio.ts";

class Queue<T> implements AsyncIterableIterator<T> {
  values: T[] = []; waiter?: (value: IteratorResult<T>) => void; closed = false;
  push(value: T) { if (this.waiter) { const waiter = this.waiter; this.waiter = undefined; waiter({ done: false, value }); } else this.values.push(value); }
  next(): Promise<IteratorResult<T>> { const value = this.values.shift(); if (value) return Promise.resolve({ done: false, value }); if (this.closed) return Promise.resolve({ done: true, value: undefined }); return new Promise(resolve => { this.waiter = resolve; }); }
  close() { this.closed = true; this.waiter?.({ done: true, value: undefined }); this.waiter = undefined; }
  [Symbol.asyncIterator]() { return this; }
}
const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
const capabilities = { directAudio: true, directVideo: false, conferenceAudio: false, conferenceVideo: false };
const offer: CallOffer = { callId: "offer", revision: 1, callerId: "allowed-peer", kind: "direct", offeredMedia: { audio: true, video: false } };
const accepted = (callId: string | null = null, ownerToken: string | null = null): Extract<CallEvent, { type: "command.result" }> => ({ type: "command.result", requestId: "synthetic", status: "accepted", callId, ownerToken, code: null });
const view = (callId: string, state: CallView["state"] = "connected", ownedByThisClient = true): CallView => ({ callId, revision: 2, kind: "direct", direction: "incoming", state, ownedByThisClient });
function configuration() {
  return { channels: { "agent-wechat": { dmPolicy: "open", native: { mode: "external", accountId: "a", endpoint: "http://localhost:6174", token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" }, calls: {
    enabled: true, provider: "fake", agentId: "voice-agent", inboundPolicy: "allowlist", inboundAllowFrom: ["allowed-peer"], outboundPolicy: "allowlist", outboundAllowTo: ["allowed-peer"], outboundToolAllowFrom: ["owner"],
  } } } };
}
class Media implements CallMediaPort {
  input = new Queue<NativeCallMediaFrame>(); sent: Array<{ generation: number; samples: Int16Array }> = []; generation = 1; closed = false; stopped = false;
  tracks() { return { sendTrack: { id: 1, generation: this.generation, direction: "send" as const, format }, receiveTrack: { id: 2, generation: 1, direction: "receive" as const, format } }; }
  frames() { return this.input; }
  sendPcm(header: { generation: number }, samples: Int16Array) { if (this.closed || this.stopped) throw Error("Closed"); this.sent.push({ generation: header.generation, samples: samples.slice() }); }
  stopAudio() { this.stopped = true; this.input.values = []; }
  close() { this.closed = true; this.input.close(); }
}
class Control implements CallControlPort {
  eventsQueue = new Queue<CallEvent>(); media = new Media(); directAudio = true; closed = false; readied = 0; answers = 0; dials = 0; ends = 0; attaches = 0; clears = 0;
  scopes: Array<CallReadyScope | undefined> = [];
  answerHook?: () => Promise<void>; dialHook?: () => Promise<void>; clearHook?: () => Promise<void>; readyHook?: () => Promise<void>;
  async connect(): Promise<Extract<CallEvent, { type: "hello" }>> { const hello = { type: "hello" as const, protocolVersion: 1, clientId: "client", capabilities: { ...capabilities, directAudio: this.directAudio }, availability: { busy: false, ownedByThisClient: false }, currentPendingOffer: null, currentOffer: null, currentCall: null, audioFormat: format }; this.eventsQueue.push(hello); return hello; }
  events() { return this.eventsQueue; }
  async ready(_id: string, _ready: boolean, _format = format, scope?: CallReadyScope) {
    this.readied++; this.scopes.push(scope);
    await this.readyHook?.();
    if (scope?.kind === "offer") this.eventsQueue.push({ type: "call.offered", offer: { ...offer, callId: scope.callId, revision: scope.expectedRevision + 1 } });
    return accepted();
  }
  async answer(_request: string, callId: string) { this.answers++; await this.answerHook?.(); this.eventsQueue.push({ type: "call.state", call: view(callId) }); return accepted(callId, "private-lease"); }
  async dial() { this.dials++; await this.dialHook?.(); this.eventsQueue.push({ type: "call.state", call: { ...view("dial"), direction: "outgoing" } }); return accepted("dial", "private-lease"); }
  async end() { this.ends++; return accepted(); }
  async clearAudio() { this.clears++; await this.clearHook?.(); this.media.generation++; return accepted(); }
  async attachMedia() { this.attaches++; return this.media; }
  close() { this.closed = true; this.eventsQueue.close(); this.media.close(); }
}
function voiceFactory() {
  const state = { opens: 0, closes: 0, incoming: [] as Buffer[], callbacks: undefined as VoiceCallbacks | undefined, opening: undefined as Promise<void> | undefined };
  const prepared: PreparedCallVoice = { sampleRate: 16000, resampler: () => ({ process: value => Buffer.from(value), flush: () => Buffer.alloc(0) }),
    async open(callbacks) { state.opens++; state.callbacks = callbacks; await state.opening; return { sendAudio: value => state.incoming.push(value), setMediaTimestamp() {}, close() { state.closes++; } }; } };
  return { state, prepared };
}
async function until(predicate: () => boolean) { for (let i = 0; i < 100; i++) { if (predicate()) return; await delay(5); } assert.fail("Condition did not become true"); }
async function fixture() { const client = new Control(); const voice = voiceFactory(); const cfg = configuration(); const account = resolveWeChatAccount(cfg as any)!; const service = new NativeCallService(account, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared }); await service.start(); return { service, client, voice }; }

test("calling stays disabled independently of open messaging and requires its own complete configuration", () => {
  assert.equal(resolveWeChatCallsConfig(undefined), undefined);
  assert.throws(() => resolveWeChatCallsConfig({ enabled: true }), /configuration/);
  assert.throws(() => resolveWeChatCallsConfig({ enabled: true, provider: "p", agentId: "a", inboundAllowFrom: ["*"] }), /allowlist/);
  assert.throws(() => resolveWeChatCallsConfig({ enabled: true, provider: "p", agentId: "a", toolPolicy: "safe-read-only" }), /persistence/);
  const cfg = configuration(); delete (cfg.channels["agent-wechat"] as any).calls;
  const account = resolveWeChatAccount(cfg as any)!; assert.equal(account.dmPolicy, "open"); assert.equal(account.calls, undefined);
});
test("provider and daemon capability failures never advertise readiness or claim", async () => {
  const cfg = configuration(); const client = new Control();
  const failed = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => { throw Error("provider missing"); } });
  await failed.start(); assert.equal(client.readied, 0); assert.equal(failed.status().state, "unavailable"); await failed.stop();
  const voice = voiceFactory(); client.directAudio = false;
  const unsupported = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared });
  await unsupported.start(); assert.equal(client.readied, 0); assert.equal(voice.state.opens, 0); assert.equal(client.closed, true); await unsupported.stop();
});
test("inbound policy ignores unlisted callers and video; allowed voice preflights before its single claim", async () => {
  const { service, client, voice } = await fixture();
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callerId: "unlisted" } });
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, offeredMedia: { audio: true, video: true } } });
  await delay(10); assert.equal(client.answers, 0); assert.equal(voice.state.opens, 0);
  client.answerHook = async () => { assert.equal(voice.state.opens, 1); };
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "call.offered", offer }); await delay(10); assert.equal(client.answers, 1); await service.stop();
});
test("pending calls are observed without global readiness; only a preflighted allowed offer permits ACK", async () => {
  const { service, client, voice } = await fixture(); assert.equal(client.readied, 0);
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callerId: "unlisted", phase: "waiting_for_ready" } });
  await delay(10); assert.equal(client.readied, 0); assert.equal(voice.state.opens, 0);
  let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => voice.state.opens === 1); assert.equal(client.readied, 0); assert.equal(client.answers, 0);
  release(); await until(() => client.attaches === 1);
  assert.deepEqual(client.scopes, [{ kind: "offer", callId: "offer", expectedRevision: 1 }]); assert.equal(client.answers, 1); await service.stop();
});
test("uncertain exact-offer readiness never retries or guesses that ACK failed", async () => {
  const { service, client, voice } = await fixture(); client.readyHook = async () => { throw new NativeCallError("command_timeout", true); };
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => voice.state.closes === 1); assert.equal(client.readied, 1); assert.equal(client.answers, 0); assert.equal(service.status().state, "needs_review");
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } }); await delay(10); assert.equal(client.readied, 1); await service.stop();
});
test("competing plugin clients accept one winner, close the losing provider and never retry", async () => {
  const first = await fixture(); const second = await fixture(); let won = false;
  const compete = async () => { if (won) throw new NativeCallError("already_claimed"); won = true; };
  first.client.answerHook = compete; second.client.answerHook = compete;
  first.client.eventsQueue.push({ type: "call.offered", offer }); second.client.eventsQueue.push({ type: "call.offered", offer });
  await until(() => first.client.answers + second.client.answers === 2); await delay(15);
  assert.equal(first.client.attaches + second.client.attaches, 1); assert.equal(first.voice.state.closes + second.voice.state.closes, 1);
  assert.equal(first.service.status().owned, true); assert.equal(second.service.status().owned, false);
  await first.service.stop(); await second.service.stop();
});
test("lost offer during provider preparation cannot issue a delayed answer", async () => {
  const { service, client, voice } = await fixture(); let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => voice.state.opens === 1);
  client.eventsQueue.push({ type: "call.state", call: view(offer.callId, "connected", false) }); await delay(10); release();
  await until(() => voice.state.closes > 0); assert.equal(client.answers, 0); assert.equal(service.status().state, "busy"); await service.stop();
});
test("outgoing target and tool requester permissions are separate from chat and caller permissions", async () => {
  const { service, client } = await fixture(); await assert.rejects(service.dial("unlisted", { assertCurrent() {} }), /destination/); assert.equal(client.dials, 0);
  const cfg = configuration(); const account = resolveWeChatAccount(cfg as any)!;
  const context = { config: cfg as any, agentId: "voice-agent", requesterSenderId: "owner", senderIsOwner: true, assertInvocationCurrent() {} };
  const key = nativeConfigurationKey(account.native!); const calls = JSON.stringify(account.calls);
  assert.doesNotThrow(() => assertCallingToolAccess(context, key, calls));
  assert.throws(() => assertCallingToolAccess({ ...context, senderIsOwner: false }, key, calls), /not permitted/);
  assert.throws(() => assertCallingToolAccess({ ...context, requesterSenderId: "allowed-peer" }, key, calls), /not permitted/);
  assert.throws(() => assertCallingToolAccess({ ...context, agentId: "another" }, key, calls), /not permitted/);
  await service.stop();
});
test("conference targets cannot be dialed through the direct-only plugin", async () => {
  const cfg = configuration(); cfg.channels["agent-wechat"].calls.outboundAllowTo.push("group@chatroom");
  const client = new Control(); const voice = voiceFactory(); const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared });
  await service.start(); await assert.rejects(service.dial("group@chatroom", { assertCurrent() {} }), /destination/);
  assert.equal(client.dials, 0); assert.equal(voice.state.opens, 0); await service.stop();
});
test("outgoing preflight rechecks invocation authority and releases its provider without dialing", async () => {
  const { service, client, voice } = await fixture(); let checks = 0;
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() { if (++checks === 2) throw Error("revoked"); } }));
  assert.equal(client.dials, 0); assert.equal(voice.state.closes, 1); assert.equal(service.status().state, "unavailable"); await service.stop();
});
test("aborting provider preparation fences a late session and cannot send a call", async () => {
  const { service, client, voice } = await fixture(); let release!: () => void;
  voice.state.opening = new Promise(resolve => { release = resolve; }); const abort = new AbortController();
  const dialing = service.dial("allowed-peer", { assertCurrent() {}, signal: abort.signal });
  await until(() => voice.state.opens === 1); abort.abort(); release(); await assert.rejects(dialing);
  await until(() => voice.state.closes === 1); assert.equal(client.dials, 0); assert.equal(client.closed, true); await service.stop();
});
test("unknown dial result is retained as needs_review and cannot cause automatic redial", async () => {
  const { service, client, voice } = await fixture(); client.dialHook = async () => { throw new NativeCallError("command_timeout", true); };
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() {} }), /needs review/);
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() {} }), /not available/);
  assert.deepEqual(client.scopes, [{ kind: "outgoing" }]);
  assert.equal(client.dials, 1); assert.equal(service.status().state, "needs_review"); assert.equal(voice.state.closes, 1); await service.stop();
});
test("new response PCM waits for clear acknowledgement and uses a fresh resampler on the new track", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0; let resamplers = 0;
  voice.prepared.resampler = () => {
    resamplers++; let previous = 0;
    return { process(value) {
      const converted = Buffer.from(value); converted.writeInt16LE(converted.readInt16LE(0) + previous, 0);
      previous = value.readInt16LE(value.length - 2); return converted;
    }, flush: () => Buffer.alloc(0) };
  };
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => { await held; media.generation = 2; }, () => { failures++; });
  t.after(() => output.close());
  const old = Buffer.alloc(640); const fresh = Buffer.alloc(1280);
  for (let i = 0; i < old.length; i += 2) old.writeInt16LE(100, i);
  for (let i = 0; i < fresh.length; i += 2) fresh.writeInt16LE(200, i);
  output.enqueue(old, { itemId: "interrupted" });
  const clearing = output.clear();
  output.enqueue(fresh, { itemId: "fresh-response" });
  await delay(30); assert.equal(media.sent.length, 0); assert.equal(resamplers, 2);
  release(); await clearing; await until(() => media.sent.length === 2);
  assert.equal(failures, 0);
  for (const frame of media.sent) {
    assert.equal(frame.generation, 2);
    assert.deepEqual(frame.samples, new Int16Array(320).fill(200));
  }
});
test("overlapping clears keep interrupted late chunks revoked and stale acknowledgements paused", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const releases: Array<() => void> = [];
  const output = new CallPcmOutput(media, format, voice.prepared,
    () => new Promise<void>(resolve => { releases.push(resolve); }), () => { failures++; });
  t.after(() => output.close());
  const pcm = (sample: number) => { const value = Buffer.alloc(640); for (let i = 0; i < value.length; i += 2) value.writeInt16LE(sample, i); return value; };
  output.enqueue(pcm(100), { itemId: "first-response" });
  const first = output.clear();
  output.enqueue(pcm(200), { itemId: "second-response" });
  const second = output.clear();
  output.enqueue(pcm(300), { itemId: "current-response" });
  output.enqueue(pcm(101), { itemId: "first-response" });
  output.enqueue(pcm(201), { itemId: "second-response" });
  media.generation = 2; releases[0](); await first;
  await delay(30); assert.equal(media.sent.length, 0);
  media.generation = 3; releases[1](); await second;
  await until(() => media.sent.length === 1);
  assert.equal(media.sent[0].generation, 3);
  assert.deepEqual(media.sent[0].samples, new Int16Array(320).fill(300));
  output.enqueue(pcm(102), { itemId: "first-response" });
  output.enqueue(pcm(202), { itemId: "second-response" });
  await delay(30); assert.equal(media.sent.length, 1); assert.equal(failures, 0);
});
test("terminal ownership revocation stops old audio callbacks and does not manufacture a hangup", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "uncertain") }); await until(() => voice.state.closes === 1);
  voice.state.callbacks!.audio(Buffer.alloc(640), { itemId: "late" }); await delay(10); assert.equal(client.media.sent.length, 0); assert.equal(client.ends, 0); await service.stop();
});
test("minimal availability revocation also tears down the former owner's provider", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: true, ownedByThisClient: false } });
  await until(() => voice.state.closes === 1); assert.equal(service.status().owned, false); assert.equal(client.ends, 0); await service.stop();
});
test("provider failure requests one end while retaining the owner until terminal", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  voice.state.callbacks!.failed(); await until(() => voice.state.closes === 1); assert.equal(client.ends, 1);
  assert.equal(client.media.closed, false); assert.equal(client.closed, false); assert.equal(service.status().owned, true);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  await until(() => service.status().state === "available"); await service.stop(); assert.equal(client.ends, 1); assert.equal(client.closed, true);
});
test("media attachment completing after end is retained without starting audio until terminal", async t => {
  const { service, client, voice } = await fixture();
  let release!: () => void; const held = new Promise<void>(resolve => { release = resolve; });
  const attach = client.attachMedia.bind(client);
  client.attachMedia = async () => { const media = await attach(); await held; return media; };
  t.after(async () => { release(); await service.stop(); });
  await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  await service.end(); assert.equal(client.media.closed, false); assert.equal(client.media.stopped, false);
  release(); await until(() => client.media.stopped);
  assert.equal(client.media.closed, false); assert.equal(client.closed, false); assert.equal(service.status().owned, true);
  voice.state.callbacks!.audio(Buffer.alloc(640), { itemId: "late-attachment" }); voice.state.callbacks!.failed();
  await delay(20); assert.equal(client.media.sent.length, 0); assert.equal(client.ends, 1);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  await until(() => service.status().state === "available"); assert.equal(client.media.closed, true);
});
test("a missing terminal reply reaches needs_review once and repeated ending cannot extend the deadline", async t => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  t.after(async () => { t.mock.timers.reset(); await service.stop(); });
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await service.end(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(client.ends, 1); assert.equal(voice.state.closes, 1); assert.equal(client.media.closed, false);
  t.mock.timers.tick(20_000);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ending") });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(service.status().owned, true); assert.equal(client.closed, false);
  t.mock.timers.tick(10_001); await new Promise(resolve => setImmediate(resolve));
  assert.equal(service.status().state, "needs_review"); assert.equal(service.status().owned, false);
  assert.equal(client.closed, true); assert.equal(client.media.closed, true); assert.equal(client.ends, 1);
});
test("PCM queue rejects oversized or unidentified output rather than growing indefinitely", async () => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  output.enqueue(Buffer.alloc(640), undefined); output.enqueue(Buffer.alloc(640), { itemId: "late" }); assert.equal(failures, 1); assert.equal(media.sent.length, 0);
  const bounded = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  for (let i = 0; i < 4; i++) bounded.enqueue(Buffer.alloc(64000), { itemId: "first" });
  bounded.enqueue(Buffer.alloc(640), { itemId: "overflow" }); assert.equal(failures, 2); bounded.close();
});
