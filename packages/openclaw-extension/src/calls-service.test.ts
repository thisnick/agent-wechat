import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { OpenClawPluginApi, OpenClawPluginToolContext, PluginRuntime } from "openclaw/plugin-sdk/core";
import { NativeCallError, type NativeMessage, type CallEvent, type CallOffer, type CallReadyScope, type CallView, type CallAudioTrack, type CallConferenceInvitation, type NativeCallMediaFrame, type NativeCallPcmObserver } from "@agent-wechat/shared";
import { resolveWeChatAccount } from "./types.ts";
import { NativeCallService, type CallControlPort, type CallMediaPort } from "./calls-service.ts";
import { type CallSummaryRequest, type PreparedCallVoice, type VoiceCallbacks } from "./calls-voice.ts";
import { resolveWeChatCallsConfig } from "./calls-config.ts";
import { callsConfigurationKey, resolveCallVoiceApiKey } from "./calls-secrets.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { assertCallingToolAccess, registerNativeCallTool } from "./calls-tool.ts";
import { CallPcmOutput } from "./calls-audio.ts";
import { CallAudioTrace } from "./calls-audio-trace.ts";

class Queue<T> implements AsyncIterableIterator<T> {
  values: T[] = []; waiter?: (value: IteratorResult<T>) => void; closed = false;
  push(value: T) { if (this.waiter) { const waiter = this.waiter; this.waiter = undefined; waiter({ done: false, value }); } else this.values.push(value); }
  next(): Promise<IteratorResult<T>> { const value = this.values.shift(); if (value) return Promise.resolve({ done: false, value }); if (this.closed) return Promise.resolve({ done: true, value: undefined }); return new Promise(resolve => { this.waiter = resolve; }); }
  close() { this.closed = true; this.waiter?.({ done: true, value: undefined }); this.waiter = undefined; }
  [Symbol.asyncIterator]() { return this; }
}
const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
const capabilities = { directAudio: true };
const offer: CallOffer = { callId: "offer", revision: 1, callerId: "allowed-peer", kind: "direct", callType: "voice" };
const accepted = (callId: string | null = null, ownerToken: string | null = null): Extract<CallEvent, { type: "command.result" }> => ({ type: "command.result", requestId: "synthetic", status: "accepted", callId, ownerToken, code: null });
const view = (callId: string, state: CallView["state"] = "connected", ownedByThisClient = true): CallView => ({ callId, revision: 2, kind: "direct", callType: "voice", direction: "incoming", state, ownedByThisClient });
function configuration() {
  return { channels: { "agent-wechat": { dmPolicy: "open", native: { mode: "external", accountId: "a", endpoint: "http://localhost:6174", token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" }, calls: {
    voice: { enabled: true, realtime: { provider: "fake" }, agentId: "voice-agent", inboundPolicy: "allowlist", inboundAllowFrom: ["allowed-peer"], outboundPolicy: "allowlist", outboundAllowTo: ["allowed-peer"], outboundToolAllowFrom: ["owner"],
    }, video: { enabled: false },
  } } } };
}
class Media implements CallMediaPort {
  input = new Queue<NativeCallMediaFrame>(); sent: Array<{ generation: number; samples: Int16Array }> = []; generation = 1; closed = false; stopped = false;
  observePcm?: NativeCallPcmObserver;
  receiveTracks: CallAudioTrack[] = [{ id: 2, generation: 1, direction: "receive", format }];
  tracks() { return { sendTrack: { id: 1, generation: this.generation, direction: "send" as const, format }, receiveTracks: structuredClone(this.receiveTracks) }; }
  frames() { return this.input; }
  sendPcm(samples: Int16Array, generation: number) { if (this.closed || this.stopped) throw Error("Closed"); this.sent.push({ generation, samples: samples.slice() }); }
  stopAudio() { this.stopped = true; this.input.values = []; }
  close() { this.closed = true; this.input.close(); }
}
class Control implements CallControlPort {
  eventsQueue = new Queue<CallEvent>(); media = new Media(); directAudio = true; closed = false; connects = 0; readied = 0; answers = 0; dials = 0; ends = 0; attaches = 0; clears = 0;
  offerOnReady = true;
  conferenceCapabilities = false;
  scopes: Array<CallReadyScope | undefined> = [];
  answerHook?: () => Promise<void>; dialHook?: () => Promise<void>; clearHook?: () => Promise<void>; readyHook?: () => Promise<void>;
  async connect(): Promise<Extract<CallEvent, { type: "hello" }>> { this.connects++; const hello = { type: "hello" as const, conferenceInvitations: [], protocolVersion: 2, clientId: "client", capabilities: { ...(this.directAudio ? capabilities : {}), ...(this.conferenceCapabilities ? { conferenceCreate: true, conferenceJoin: true, conferenceInvite: true, conferenceAudio: true } : {}) }, availability: { busy: false, ownedByThisClient: false }, currentPendingOffer: null, currentOffer: null, currentCall: null, audioFormat: format }; this.eventsQueue.push(hello); return hello; }
  events() { return this.eventsQueue; }
  async ready(_id: string, _ready: boolean, _format = format, scope?: CallReadyScope) {
    this.readied++; this.scopes.push(scope);
    await this.readyHook?.();
    if (scope?.kind === "offer" && this.offerOnReady) this.eventsQueue.push({ type: "call.offered", offer: { ...offer, callId: scope.callId, revision: scope.expectedRevision + 1 } });
    return accepted();
  }
  async answer(_request: string, callId: string) { this.answers++; await this.answerHook?.(); this.eventsQueue.push({ type: "call.state", call: view(callId) }); return accepted(callId, "private-lease"); }
  async dial() { this.dials++; await this.dialHook?.(); this.eventsQueue.push({ type: "call.state", call: { ...view("dial"), direction: "outgoing" } }); return accepted("dial", "private-lease"); }
  async end() { this.ends++; return accepted(); }
  async clearAudio() { this.clears++; await this.clearHook?.(); this.media.generation++; return accepted(); }
  async attachMedia(_callId: string, observePcm?: NativeCallPcmObserver) { this.attaches++; this.media.observePcm = observePcm; return this.media; }
  close() { this.closed = true; this.eventsQueue.close(); this.media.close(); }
}
function voiceFactory() {
  const state = { opens: 0, closes: 0, incoming: [] as Buffer[], timestamps: [] as number[],
    contexts: [] as Array<Parameters<PreparedCallVoice["open"]>[1]>,
    updates: [] as Array<{ snapshot: import("./calls-context.ts").CallContextSnapshot; change: import("./calls-context.ts").CallContextChange }>,
    callbacks: undefined as VoiceCallbacks | undefined, opening: undefined as Promise<void> | undefined,
    binding: { sessionKey: "agent:agent:wechat-call:peer", owner: false }, summaries: [] as CallSummaryRequest[], summaryText: undefined as string | undefined };
  const prepared: PreparedCallVoice = { sampleRate: 16000, continuous: false, resampler: () => ({ process: value => Buffer.from(value), flush: () => Buffer.alloc(0) }),
    async open(callbacks, context) { state.opens++; state.contexts.push(context); state.callbacks = callbacks; await state.opening; return { interruptOutput: () => callbacks.clear("output-budget"), updateCallContext: (snapshot, change) => { state.updates.push(structuredClone({ snapshot, change })); }, sendAudio: value => state.incoming.push(value), setMediaTimestamp: ms => state.timestamps.push(ms), close() { state.closes++; }, binding: () => state.binding }; },
    async summarize(request) { state.summaries.push(request); return state.summaryText; } };
  return { state, prepared };
}
async function until(predicate: () => boolean) { for (let i = 0; i < 100; i++) { if (predicate()) return; await delay(5); } assert.fail("Condition did not become true"); }
const cachedHost = (async () => ({ client: {
  account: async () => ({ id: "a", ownUsername: "contact-self" }),
  contact: async () => ({ remark: "Known caller" }),
  group: async () => ({ name: "Public test room", remark: "Private label" }),
  groupMember: async (_account: string, _group: string, contactId: string) => ({ member: { username: contactId, roomDisplayName: `Room ${contactId}` } }),
} })) as unknown as NonNullable<import("./calls-service.ts").CallServiceDependencies["host"]>;
async function fixture(logAudioTrace?: (line: string) => void, cfg = configuration(), warn?: (line: string) => void) { const client = new Control(); const voice = voiceFactory(); const account = resolveWeChatAccount(cfg as any)!; const service = new NativeCallService(account, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared, host: cachedHost, logAudioTrace, warn }); await service.start(); return { service, client, voice }; }

test("calling stays disabled independently of open messaging and requires its own complete configuration", async () => {
  assert.equal(resolveWeChatCallsConfig(undefined), undefined);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true } }), /configuration/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", inboundAllowFrom: ["*"] } }), /allowlist/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", toolPolicy: "safe-read-only" } }), /persistence/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", toolPolicy: "owner" } }), /persistence/);
  const consultation = { enabled: true, realtime: { provider: "p" }, agentId: "a" };
  assert.equal(resolveWeChatCallsConfig({ voice: consultation })!.voice!.toolPolicy, "none");
  assert.equal(resolveWeChatCallsConfig({ voice: consultation })!.voice!.maxDurationSeconds, 3600);
  assert.equal(resolveWeChatCallsConfig({ voice: { ...consultation, allowConsultSessionPersistence: true } })!.voice!.toolPolicy, "safe-read-only");
  assert.equal(resolveWeChatCallsConfig({ voice: { ...consultation, allowConsultSessionPersistence: true, toolPolicy: "none" } })!.voice!.toolPolicy, "none");
  const cfg = configuration(); delete (cfg.channels["agent-wechat"] as any).calls;
  const account = resolveWeChatAccount(cfg as any)!; assert.equal(account.dmPolicy, "open"); assert.equal(account.calls, undefined);
  const apiKey = "synthetic-openai-secret";
  const credentials = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "a", realtime: { provider: "openai", providers: { openai: {
    apiKey: { source: "env", provider: "voiceenv", id: "WECHAT_TEST_VOICE_KEY" },
  } } } } })!;
  assert.equal(await resolveCallVoiceApiKey(credentials.voice!, { secrets: { providers: { voiceenv: { source: "env", allowlist: ["WECHAT_TEST_VOICE_KEY"] } } } }, { WECHAT_TEST_VOICE_KEY: apiKey }), apiKey);
  assert.equal(JSON.stringify(credentials).includes("WECHAT_TEST_VOICE_KEY"), false);
  await assert.rejects(resolveCallVoiceApiKey(credentials.voice!, {}, {}), /API key could not be resolved/);
  const changed = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "a", realtime: { provider: "openai", providers: { openai: { apiKey } } } } })!;
  assert.notEqual(callsConfigurationKey(credentials), callsConfigurationKey(changed));
  assert.equal(JSON.stringify(changed).includes(apiKey), false);

  for (const calls of [undefined, { voice: { enabled: false }, video: { enabled: false } }, { voice: { enabled: false }, video: { enabled: true } }]) {
    const disabledCfg = configuration(); (disabledCfg.channels["agent-wechat"] as any).calls = calls;
    const disabledAccount = resolveWeChatAccount(disabledCfg as any)!;
    const control = new Control(); let prepared = 0;
    const disabled = new NativeCallService(disabledAccount, disabledCfg, {} as PluginRuntime, {
      client: control, prepareVoice: async () => { prepared++; throw Error("Disabled voice must not prepare a provider"); },
    });
    await disabled.start();
    control.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
    control.eventsQueue.push({ type: "call.offered", offer });
    await delay(1);
    assert.equal(disabled.status().state, calls?.video?.enabled ? "unavailable" : "disabled");
    assert.deepEqual([prepared, control.connects, control.readied, control.answers, control.ends, control.dials], [0, 0, 0, 0, 0, 0]);
    await disabled.stop();
  }
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
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callType: "video" } });
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
  const lines: string[] = [];
  const { service, client, voice } = await fixture(undefined, configuration(), line => lines.push(line)); client.readyHook = async () => { throw new NativeCallError("command_timeout", true); };
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => voice.state.closes === 1); assert.equal(client.readied, 1); assert.equal(client.answers, 0); assert.equal(service.status().state, "needs_review");
  assert.ok(lines.some(line => /^\[wechat-call\] incoming call not answered: stage=ready ms=\d+ code=command_timeout state=needs_review$/.test(line)));
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } }); await delay(10); assert.equal(client.readied, 1); await service.stop();
});
test("an incoming call whose voice session fails to open logs only its stage", async () => {
  const lines: string[] = [];
  const { service, client, voice } = await fixture(undefined, configuration(), line => lines.push(line));
  voice.prepared.open = async () => { throw Error("synthetic-private-provider-detail"); };
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => lines.length > 1);
  assert.deepEqual(lines.map(line => line.replace(/ms=\d+/, "ms=N")), [
    "[wechat-call] incoming call not answered: stage=voice ms=N code=none state=unavailable",
    "[wechat-call] calling stopped: state=unavailable reason=preflight-failed; it resumes once the daemon reports no call in progress",
  ]);
  assert.equal(lines.some(line => line.includes("synthetic-private")), false);
  assert.equal(client.readied, 0); assert.equal(client.answers, 0);
  await service.stop();
});
test("a control connection that closes unexpectedly is reported once", async () => {
  const lines: string[] = [];
  const { service, client } = await fixture(undefined, configuration(), line => lines.push(line));
  client.eventsQueue.close();
  await until(() => lines.length > 0); await delay(10);
  assert.deepEqual(lines, ["[wechat-call] calling stopped: state=unavailable reason=events-ended; it resumes once the daemon reports no call in progress"]);
  assert.equal(service.status().state, "unavailable");
  await service.stop();
});
test("after calling stops, a new connection resumes it once the daemon reports no call in progress", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const lines: string[] = [];
  const clients: Control[] = [];
  const voice = voiceFactory(); const cfg = configuration();
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, {
    createClient: () => { const client = new Control(); clients.push(client); return client; },
    prepareVoice: async () => voice.prepared, warn: line => lines.push(line),
  });
  await service.start(); assert.equal(service.status().state, "available");
  clients[0].eventsQueue.close(); // the daemon closed the control connection
  await until(() => lines.length > 0);
  assert.match(lines[0], /calling stopped: state=unavailable reason=events-ended; it resumes once/);
  t.mock.timers.tick(5_000); await delay(0);
  await until(() => service.status().state === "available");
  assert.equal(clients.length, 2); assert.equal(lines.at(-1), "[wechat-call] calling resumed: the daemon reports no call in progress");
  clients[1].eventsQueue.push({ type: "call.offered", offer: { ...offer, callId: "after-recovery" } });
  await until(() => clients[1].answers === 1);
  await service.stop();
});
test("calling does not resume while the daemon still reports a call in progress", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const clients: Control[] = [];
  const voice = voiceFactory(); const cfg = configuration();
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, {
    createClient: () => {
      const client = new Control(); clients.push(client);
      if (clients.length === 2) {
        const connect = client.connect.bind(client);
        client.connect = async () => ({ ...(await connect()), availability: { busy: true, ownedByThisClient: false } });
      }
      return client;
    },
    prepareVoice: async () => voice.prepared,
  });
  await service.start();
  clients[0].eventsQueue.close();
  await until(() => service.status().state === "unavailable");
  t.mock.timers.tick(5_000); await delay(0);
  await until(() => clients.length === 2 && clients[1].closed);
  assert.equal(service.status().state, "unavailable", "busy daemon: stay stopped");
  t.mock.timers.tick(10_000); await delay(0);
  await until(() => service.status().state === "available");
  assert.equal(clients.length, 3);
  await service.stop();
});
test("a withdrawn pre-answer offer releases the listener for the next call", async t => {
  const { service, client, voice } = await fixture();
  t.after(() => service.stop());
  client.offerOnReady = false;
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => client.readied === 1);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: true, ownedByThisClient: false } });
  client.eventsQueue.push({ type: "call.pending", offer: null });
  await delay(10);
  assert.equal(service.status().state, "preparing");
  assert.equal(voice.state.closes, 0);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: false, ownedByThisClient: false } });
  client.eventsQueue.push({ type: "call.pending", offer: null });
  await until(() => voice.state.closes === 1);
  assert.equal(service.status().state, "available");
  assert.equal(client.closed, false);
  assert.equal(client.answers, 0);

  client.offerOnReady = true;
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callId: "next-offer", phase: "waiting_for_ready" } });
  await until(() => client.attaches === 1);
  assert.equal(client.readied, 2);
  assert.equal(client.answers, 1);
  assert.equal(service.status().callId, "next-offer");
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
  const cfg = configuration();
  (cfg.channels["agent-wechat"].calls.voice.realtime as any).providers = { openai: { apiKey: "synthetic-original-key" } };
  const account = resolveWeChatAccount(cfg as any)!;
  const context = { config: cfg as any, agentId: "voice-agent", requesterSenderId: "owner", senderIsOwner: true, assertInvocationCurrent() {} };
  const key = nativeConfigurationKey(account.native!); const calls = callsConfigurationKey(account.calls!);
  assert.doesNotThrow(() => assertCallingToolAccess(context, key, calls));
  assert.throws(() => assertCallingToolAccess({ ...context, senderIsOwner: false }, key, calls), /not permitted/);
  assert.throws(() => assertCallingToolAccess({ ...context, requesterSenderId: "allowed-peer" }, key, calls), /not permitted/);
  // Any agent the tool is allowed for may place calls for the owner; the call runs as the calling agent.
  assert.doesNotThrow(() => assertCallingToolAccess({ ...context, agentId: "another" }, key, calls));
  (cfg.channels["agent-wechat"].calls.voice.realtime as any).providers.openai.apiKey = "synthetic-replaced-key";
  assert.throws(() => assertCallingToolAccess(context, key, calls), /not permitted/);

  await service.stop();
});
test("only the trusted outbound invocation or configured inbound route supplies a parent session", async t => {
  const cfg = configuration();
  const inboundKey = "agent:voice-agent:wechat:direct:synthetic-inbound";
  Object.assign(cfg.channels["agent-wechat"].calls.voice, { consultation: { inboundSessionKey: inboundKey } });
  const outbound = await fixture(undefined, cfg);
  const inbound = await fixture(undefined, cfg);
  t.after(async () => { await outbound.service.stop(); await inbound.service.stop(); });
  const trusted: OpenClawPluginToolContext<2> = {
    config: cfg as any, agentId: "voice-agent", requesterSenderId: "owner", senderIsOwner: true,
    sessionKey: "agent:voice-agent:wechat:direct:synthetic-owner", sessionId: "owner-session", assertInvocationCurrent() {},
  };
  let tool: { execute(id: string, input: unknown): Promise<unknown> } | undefined;
  const api = { registerTool(factory: { create(context: OpenClawPluginToolContext<2>): NonNullable<typeof tool>[] }) {
    [tool] = factory.create(trusted);
  } } as unknown as OpenClawPluginApi;
  registerNativeCallTool(api, () => outbound.service);
  await assert.rejects(tool!.execute("synthetic", { action: "dial", contactId: "allowed-peer", sessionKey: inboundKey }), /Invalid calling arguments/);
  assert.equal(outbound.client.dials, 0);
  await tool!.execute("synthetic", { action: "dial", contactId: "allowed-peer" });
  assert.deepEqual(outbound.voice.state.contexts[0].parentSession, { key: trusted.sessionKey, expectedSessionId: trusted.sessionId });
  await until(() => outbound.voice.state.contexts[0].connectedPeer?.() === "allowed-peer");
  inbound.client.eventsQueue.push({ type: "call.offered", offer });
  await until(() => inbound.client.answers === 1);
  assert.deepEqual(inbound.voice.state.contexts[0].parentSession, { key: inboundKey });
  await until(() => inbound.voice.state.contexts[0].connectedPeer?.() === offer.callerId);
});
test("conference targets cannot be dialed through the direct-only plugin", async () => {
  const cfg = configuration(); cfg.channels["agent-wechat"].calls.video.enabled = true; cfg.channels["agent-wechat"].calls.voice.outboundAllowTo.push("group@chatroom");
  const client = new Control(); const voice = voiceFactory(); const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared });
  await service.start(); await assert.rejects(service.dial("group@chatroom", { assertCurrent() {} }), /destination/);
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callType: "video", phase: "waiting_for_ready" } });
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callType: "video" } });
  await delay(10);
  assert.equal(client.dials, 0); assert.equal(voice.state.opens, 0); assert.equal(client.answers, 0); assert.equal(client.readied, 0); await service.stop();
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
test("a clear discards the paced backlog and fresh PCM waits for the new track", async t => {
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
  const old = Buffer.alloc(3840); const fresh = Buffer.alloc(1280);
  for (let i = 0; i < old.length; i += 2) old.writeInt16LE(100, i);
  for (let i = 0; i < fresh.length; i += 2) fresh.writeInt16LE(200, i);
  output.enqueue(old, { itemId: "interrupted" });
  output.start();
  assert.deepEqual(media.sent.map(frame => frame.samples.length), [640]);
  const clearing = output.clear();
  output.enqueue(fresh, { itemId: "fresh-response" });
  await delay(30); assert.equal(media.sent.length, 1); assert.equal(resamplers, 2);
  release(); await clearing; await until(() => media.sent.length === 2);
  assert.equal(failures, 0);
  for (const frame of media.sent.slice(1)) {
    assert.equal(frame.generation, 2);
    assert.deepEqual(frame.samples, new Int16Array(640).fill(200));
  }
});
test("response PCM stays ordered and resumes with small chunks after a timer stall", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  t.after(() => output.close());
  const burst = Buffer.alloc(640 * 8);
  for (let i = 0; i < burst.length / 2; i++) burst.writeInt16LE(i + 1, i * 2);
  output.enqueue(burst, { itemId: "burst" });
  output.start();
  assert.deepEqual(media.sent.map(frame => frame.samples.length), [640]);
  // Hold the event loop across several slots. A late timer must not send its debt.
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 65);
  await until(() => media.sent.length >= 2);
  assert.ok(media.sent[1].samples.length <= 640);
  const speech = () => media.sent.flatMap(frame => Array.from(frame.samples).filter(Boolean));
  await until(() => speech().length === burst.length / 2);
  assert.deepEqual(speech(), Array.from({ length: burst.length / 2 }, (_, i) => i + 1));
  assert.equal(failures, 0);
});
test("continuous PCM plays unlabeled audio and accepts new audio after each provider clear", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0; let clears = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => { clears++; }, () => { failures++; }, undefined, { continuous: true });
  t.after(() => output.close());
  const speech = () => media.sent.flatMap(frame => Array.from(frame.samples).filter(Boolean));
  output.start();
  output.enqueue(Buffer.alloc(640, 1));
  await until(() => speech().length === 320);
  await output.clear();
  output.enqueue(Buffer.alloc(640, 2));
  await until(() => speech().length === 640);
  assert.ok(speech().slice(320).every(value => value === 0x0202));
  assert.equal(clears, 1); assert.equal(failures, 0);
});
test("continuous PCM keeps only the newest three seconds when its backlog grows", t => {
  const media = new Media(); const voice = voiceFactory();
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => assert.fail("PCM send failed"), undefined, { continuous: true });
  t.after(() => output.close());
  output.start();
  output.enqueue(Buffer.alloc(32 * 1000, 1));
  for (let i = 0; i < 3; i++) output.enqueue(Buffer.alloc(32 * 1000, 2));
  for (let i = 0; i < 220; i++) { now += 20; t.mock.timers.tick(20); }
  const samples = media.sent.flatMap(frame => Array.from(frame.samples)).filter(Boolean);
  assert.equal(samples.filter(value => value === 0x0101).length, 0);
  assert.equal(samples.filter(value => value === 0x0202).length, 3000 * 16);
});
test("continuous PCM skips silence while a stall's backlog drains, without dropping speech", t => {
  const media = new Media(); const voice = voiceFactory();
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const skipped: number[] = [];
  const trace = { add: (key: string, count = 1) => { if (key === "outputSkippedSilenceSamples") skipped.push(count); }, gap() {}, measure() {} } as unknown as CallAudioTrace;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => assert.fail("PCM send failed"), trace, { continuous: true });
  t.after(() => output.close());
  output.start();
  // A stalled provider catches up with one second of speech, then sends silence.
  output.enqueue(Buffer.alloc(32 * 1000, 1));
  for (let i = 0; i < 25; i++) output.enqueue(Buffer.alloc(640));
  assert.equal(skipped.reduce((sum, count) => sum + count, 0), 25 * 320);
  for (let i = 0; i < 60; i++) { now += 20; t.mock.timers.tick(20); }
  const samples = media.sent.flatMap(frame => Array.from(frame.samples));
  assert.equal(samples.filter(value => value === 0x0101).length, 1000 * 16);
  // Once the backlog is short again, silence from the provider plays normally.
  const before = skipped.length;
  output.enqueue(Buffer.alloc(640));
  assert.equal(skipped.length, before);
});
test("playout starts with silence and reports only the time played from each reply item", t => {
  const media = new Media();
  const voice = voiceFactory();
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => assert.fail("PCM send failed"));
  t.after(() => output.close());
  output.start();
  assert.deepEqual(media.sent[0].samples, new Int16Array(640));
  assert.deepEqual(output.playbackState(), []);
  output.enqueue(Buffer.alloc(320, 1), { itemId: "one" });
  output.enqueue(Buffer.alloc(320, 2), { itemId: "two" });
  now = 120;
  t.mock.timers.tick(20);
  now = 145;
  assert.deepEqual(output.playbackState(), [{ itemId: "one", audioEndMs: 5 }, { itemId: "two", audioEndMs: 0 }]);
});
test("audio trace aggregates each boundary and a discarded PCM chunk still advances provider time", async t => {
  const previous = process.env.WX_CALL_AUDIO_TRACE;
  process.env.WX_CALL_AUDIO_TRACE = "1";
  t.after(() => { if (previous === undefined) delete process.env.WX_CALL_AUDIO_TRACE; else process.env.WX_CALL_AUDIO_TRACE = previous; });
  const lines: string[] = [];
  const { service, client, voice } = await fixture(line => lines.push(line));
  t.after(() => service.stop());
  await service.dial("allowed-peer", { assertCurrent() {} });
  await until(() => client.media.observePcm !== undefined);
  const receivedAtMs = performance.now();
  client.media.observePcm!({ receivedAtMs, samples: 320, queueDepth: 16, droppedSamples: 320 });
  client.media.input.push({ trackId: 2, generation: 1, payload: new Uint8Array(640), receivedAtMs, sampleOffset: 320 });
  const reply = Buffer.alloc(640, 1);
  voice.state.callbacks!.audio(reply, { itemId: "private-response-item" });
  await until(() => voice.state.incoming.length === 1 && client.media.sent.some(frame => frame.samples.includes(257)));
  assert.deepEqual(voice.state.timestamps, [20]);
  await service.stop();
  const prefix = "[wechat-call-audio] plugin ";
  assert.equal(lines[0], prefix + "service-ready state=available");
  const report = JSON.parse(lines[1].slice(prefix.length));
  assert.equal(report.final, true);
  assert.equal(report.counts.wsRxDroppedSamples, 320);
  assert.equal(report.counts.inputConsumedSamples, 320);
  assert.equal(report.counts.realtimeInputSamples, 320);
  assert.equal(report.counts.daemonOutputSamples - report.counts.outputSilenceSamples, 320);
  assert.equal(report.measures.sdkRxQueueDepth.max, 16);
  assert.equal(report.measures.sdkRxWaitMs.count, 1);
  assert.equal(typeof report.eventLoopDelayMs.count, "number");
  assert.doesNotMatch(lines.join("\n"), /allowed-peer|private-response-item|private-lease|synthetic-token/);
});
test("overlapping clears keep interrupted late chunks revoked and stale acknowledgements paused", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const releases: Array<() => void> = [];
  const output = new CallPcmOutput(media, format, voice.prepared,
    () => new Promise<void>(resolve => { releases.push(resolve); }), () => { failures++; });
  t.after(() => output.close());
  const pcm = (sample: number) => { const value = Buffer.alloc(640); for (let i = 0; i < value.length; i += 2) value.writeInt16LE(sample, i); return value; };
  output.enqueue(pcm(100), { itemId: "first-response" });
  output.start();
  const first = output.clear();
  output.enqueue(pcm(200), { itemId: "second-response" });
  const second = output.clear();
  output.enqueue(pcm(300), { itemId: "current-response" });
  output.enqueue(pcm(101), { itemId: "first-response" });
  output.enqueue(pcm(201), { itemId: "second-response" });
  media.generation = 2; releases[0](); await first;
  await delay(30); assert.equal(media.sent.length, 1);
  media.generation = 3; releases[1](); await second;
  await until(() => media.sent.length === 2);
  assert.equal(media.sent[1].generation, 3);
  assert.deepEqual(media.sent[1].samples.slice(0, 320), new Int16Array(320).fill(300));
  assert.ok(media.sent[1].samples.slice(320).every(value => value === 0));
  output.enqueue(pcm(102), { itemId: "first-response" });
  output.enqueue(pcm(202), { itemId: "second-response" });
  await delay(30);
  assert.equal(media.sent.filter(frame => frame.samples.some(Boolean)).length, 2);
  assert.equal(failures, 0);
});
test("an answered call is summarized in its session, remembered for the contact's next call and reported to the session that placed it", async t => {
  const dir = await mkdtemp(join(tmpdir(), "wechat-call-summary-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const cfg = configuration();
  Object.assign(cfg.channels["agent-wechat"].calls.voice, { realtime: { provider: "fake", recentContext: { enabled: true, maxMessages: 2 } },
    toolPolicy: "safe-read-only", allowConsultSessionPersistence: true });
  const events: Array<{ text: string; options: { sessionKey: string; contextKey?: string } }> = []; const wakes: Array<{ sessionKey?: string }> = [];
  const parentKey = "agent:voice-agent:wechat:direct:owner";
  const runtime = {
    system: { enqueueSystemEvent: (text: string, options: { sessionKey: string }) => { events.push({ text, options }); return true; }, requestHeartbeatNow: (options: { sessionKey?: string }) => { wakes.push(options); } },
    agent: { session: { resolveStorePath: () => "/synthetic/store", getSessionEntry: ({ sessionKey }: { sessionKey: string }) => sessionKey === parentKey ? { sessionId: "owner-session" } : undefined } },
    channel: { routing: { resolveAgentRoute: ({ peer }: { peer: { id: string } }) => ({ agentId: "chat-agent", sessionKey: `agent:chat-agent:agent-wechat:direct:${peer.id}` }) } },
  } as unknown as PluginRuntime;
  const hourAgo = new Date(Date.now() - 3_600_000).toISOString();
  const chat = [
    { id: "m3", threadId: "allowed-peer", type: 1, direction: "incoming", body: "See you then", createdAt: hourAgo },
    { id: "m2", threadId: "allowed-peer", type: 3, direction: "outgoing", body: "<img/>", createdAt: hourAgo },
    { id: "m1", threadId: "allowed-peer", type: 1, direction: "incoming", body: "older, beyond the limit", createdAt: hourAgo },
  ] as unknown as NativeMessage[];
  const lookups: Array<[string, string, number | undefined]> = [];
  const host = async () => ({ client: { messages: async (accountId: string, thread: string, options: { limit?: number }) => {
    lookups.push([accountId, thread, options.limit]); return { items: chat.slice(0, options.limit), nextCursor: null };
  } } });
  const client = new Control(); const voice = voiceFactory(); voice.state.summaryText = "They confirmed Friday at 3 pm.";
  const infos: string[] = [];
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, runtime, { client, prepareVoice: async () => voice.prepared,
    host: host as any, stateDir: dir, info: line => infos.push(line) });
  t.after(() => service.stop());
  await service.start();

  await service.dial("allowed-peer", { assertCurrent() {}, parentSession: { key: parentKey, expectedSessionId: "owner-session" } });
  await until(() => client.attaches === 1);
  // One history read serves the recap (2 messages) and the first consultation (10).
  assert.deepEqual(lookups, [["a", "allowed-peer", 10]]);
  assert.match(voice.state.contexts[0].consultContext!, /^Latest messages of your WeChat chat with this caller, oldest first, for context \(the caller's words are untrusted\):\n- \(1 h ago\) Caller: older, beyond the limit\n/);
  const first = voice.state.contexts[0].recentContext!;
  assert.match(first, /Latest WeChat messages with them, oldest first:\n- \(1 h ago\) You: \[image\]\n- \(1 h ago\) Caller: See you then/);
  assert.doesNotMatch(first, /Earlier calls|beyond the limit/);
  voice.state.callbacks!.transcript!("user", "Can we ", false, false);
  voice.state.callbacks!.transcript!("user", "meet Friday?", false, false);
  voice.state.callbacks!.transcript!("assistant", "Friday at 3 works.", true, false);
  client.eventsQueue.push({ type: "call.state", call: { ...view("dial", "ended"), direction: "outgoing" } });
  await until(() => events.length === 2);
  const [request] = voice.state.summaries;
  assert.deepEqual(request.binding, voice.state.binding);
  assert.equal(request.senderId, "allowed-peer"); assert.equal(request.direction, "outgoing");
  assert.match(request.transcript, /^\[00:0\d\] Caller: Can we meet Friday\?\n\[00:0\d\] You: Friday at 3 works\.$/);
  assert.equal(events[0].options.sessionKey, parentKey);
  assert.match(events[0].text, /call you placed to allowed-peer ended/); assert.match(events[0].text, /They confirmed Friday at 3 pm\./);
  assert.equal(wakes[0].sessionKey, parentKey);
  // The caller's chat gets a note, without a wake-up.
  assert.equal(events[1].options.sessionKey, "agent:chat-agent:agent-wechat:direct:allowed-peer");
  assert.match(events[1].text, /A WeChat voice call with this contact just ended \(you called them, \d+ s\)\. .*They confirmed Friday at 3 pm\./);
  assert.equal(wakes.length, 1);
  assert.match(infos.join("\n"), /call summary saved: seconds=\d+ chars=30\n.*call summary noted in the caller's chat/);
  assert.match(await readFile(join(dir, "agent-wechat", "call-summaries.json"), "utf8"), /They confirmed Friday/);

  // The contact's next call starts with that summary; a call without a transcript is not summarized.
  await until(() => service.status().state === "available");
  client.eventsQueue.push({ type: "call.offered", offer });
  await until(() => client.attaches === 2);
  assert.match(voice.state.contexts[1].recentContext!, /Earlier calls, newest first:\n- just now, you called them \(\d+ s\): They confirmed Friday at 3 pm\./);
  client.eventsQueue.push({ type: "call.state", call: view(offer.callId, "ended") });
  await until(() => service.status().state === "available"); await delay(10);
  assert.equal(voice.state.summaries.length, 1); assert.equal(events.length, 2);
});
test("a call placed from another agent's group session reports its summary there", async t => {
  const cfg = configuration();
  Object.assign(cfg.channels["agent-wechat"].calls.voice, { toolPolicy: "safe-read-only", allowConsultSessionPersistence: true });
  const groupKey = "agent:groups:agent-wechat:group:family@chatroom";
  const events: Array<{ text: string; sessionKey: string }> = []; const wakes: Array<{ sessionKey?: string; agentId?: string }> = []; const stores: string[] = [];
  const runtime = {
    system: { enqueueSystemEvent: (text: string, options: { sessionKey: string }) => { events.push({ text, sessionKey: options.sessionKey }); return true; },
      requestHeartbeatNow: (options: { sessionKey?: string; agentId?: string }) => { wakes.push(options); } },
    agent: { session: { resolveStorePath: (_store: unknown, { agentId }: { agentId: string }) => { stores.push(agentId); return `/synthetic/${agentId}`; },
      getSessionEntry: ({ sessionKey }: { sessionKey: string }) => sessionKey === groupKey ? { sessionId: "group-session" } : undefined } },
  } as unknown as PluginRuntime;
  const client = new Control(); const voice = voiceFactory(); voice.state.summaryText = "She will call back tonight.";
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, runtime, { client, prepareVoice: async () => voice.prepared });
  t.after(() => service.stop());
  await service.start();
  await service.dial("allowed-peer", { assertCurrent() {}, parentSession: { key: groupKey, expectedSessionId: "group-session" } });
  await until(() => client.attaches === 1);
  voice.state.callbacks!.transcript!("user", "I'll call back tonight.", true, false);
  client.eventsQueue.push({ type: "call.state", call: { ...view("dial", "ended"), direction: "outgoing" } });
  await until(() => events.length === 1);
  assert.equal(events[0].sessionKey, groupKey); assert.match(events[0].text, /She will call back tonight\./);
  assert.deepEqual(wakes, [{ sessionKey: groupKey, agentId: "groups", reason: "agent-wechat call ended", source: "other" }]);
  assert.ok(stores.includes("groups"), "the group session is read from its own agent's store");
});
test("terminal ownership revocation stops old audio callbacks and does not manufacture a hangup", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "uncertain") }); await until(() => voice.state.closes === 1);
  const sentBeforeLateCallback = client.media.sent.length;
  voice.state.callbacks!.audio(Buffer.alloc(640), { itemId: "late" }); await delay(10); assert.equal(client.media.sent.length, sentBeforeLateCallback); assert.equal(client.ends, 0);
  client.eventsQueue.push({ type: "call.state", call: view("other-call", "ended") });
  await delay(1); assert.equal(service.status().state, "needs_review");
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: false, ownedByThisClient: false } });
  await until(() => service.status().state === "available");
  assert.equal(client.dials, 1); assert.equal(client.ends, 0);
  await service.stop();
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
test("a slow hangup after a provider failure keeps calling available for the next call", async () => {
  const lines: string[] = [];
  const { service, client, voice } = await fixture(undefined, configuration(), line => lines.push(line));
  (client as any).end = async () => { client.ends++; await delay(2300); return accepted(); };
  await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  voice.state.callbacks!.failed("provider-closed:error");
  await delay(2500);
  assert.equal(client.ends, 1); assert.equal(client.closed, false); assert.notEqual(service.status().state, "needs_review");
  assert.deepEqual(lines, ["[wechat-call] ending call after a failure: cause=provider-closed:error answered=true"]);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  await until(() => service.status().state === "available");
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callId: "next" } });
  await until(() => client.answers === 1);
  await service.stop();
});
test("media attachment completing after end is retained without starting audio until terminal", async t => {
  const { service, client, voice } = await fixture();
  let release!: () => void; const held = new Promise<void>(resolve => { release = resolve; });
  const attach = client.attachMedia.bind(client);
  client.attachMedia = async (callId, observePcm) => { const media = await attach(callId, observePcm); await held; return media; };
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
test("response audio beyond eight seconds plays completely at the daemon clock", t => {
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const diagnostics: string[] = [];
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; }, undefined,
    { diagnostic: line => diagnostics.push(line) });
  t.after(() => { output.close(); t.mock.timers.reset(); });
  // A legal provider burst, longer than the former eight-second budget.
  for (let n = 1; n <= 9; n++) output.enqueue(Buffer.alloc(64000, n), { itemId: "long-reply" });
  output.start();
  for (let n = 0; n < 902; n++) { now += 20; t.mock.timers.tick(20); }
  const received = media.sent.flatMap(chunk => [...chunk.samples]);
  for (let n = 0; n < 9; n++) {
    assert.deepEqual(received.slice(n * 32000, (n + 1) * 32000), Array(32000).fill((n + 1) * 257));
  }
  assert.ok(media.sent.every(chunk => chunk.samples.length <= 640));
  assert.equal(failures, 0);
  assert.match(diagnostics[0], /mode=response capacityMs=600000 capacityBytes=19200000/);
});

test("ten-minute response overflow interrupts the SDK, accounts for playback and fences stale chunks", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0, interruptions = 0;
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const lines: string[] = [], diagnostics: string[] = [];
  const trace = new CallAudioTrace(line => lines.push(line), { daemonHz: 16000, realtimeHz: 16000 });
  let playback: ReturnType<CallPcmOutput["playbackState"]> = [];
  const output = new CallPcmOutput(media, format, voice.prepared, () => held, () => { failures++; }, trace, {
    onResponseOverflow() { interruptions++; playback = output.playbackState(); void output.clear("output-budget"); },
    diagnostic: line => diagnostics.push(line),
  });
  t.after(() => { output.close(); trace.close(); release(); });
  for (let n = 0; n < 300; n++) output.enqueue(Buffer.alloc(64000, 0x11), { itemId: "private-old-item" });
  output.enqueue(Buffer.alloc(640, 0x22), { itemId: "private-new-overflow-item" });
  assert.equal(interruptions, 1);
  assert.deepEqual(playback, [{ itemId: "private-old-item", audioEndMs: 0 }, { itemId: "private-new-overflow-item", audioEndMs: 0 }]);
  output.start();
  output.enqueue(Buffer.alloc(640, 0x33), { itemId: "private-old-item" });
  output.enqueue(Buffer.alloc(640, 0x44), { itemId: "private-new-overflow-item" });
  output.enqueue(Buffer.alloc(1280, 0x55), { itemId: "fresh-item" });
  assert.equal(media.sent.length, 0, "wait for daemon clear before sending a fresh generation");
  release(); await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(media.sent[0].samples, new Int16Array(640).fill(0x5555));
  assert.equal(failures, 0);
  output.close(); trace.close();
  const report = JSON.parse(lines[0].slice("[wechat-call-audio] plugin ".length));
  assert.equal(report.counts.outputBudgetInterruptions, 1);
  assert.equal(report.counts.outputStaleChunks, 2);
  assert.equal(report.measures.outputQueuedAudioMs.max, 600000);
  assert.match(diagnostics.join("\n"), /stage=buffer-limit reason=capacity queuedMs=600000/);
  assert.match(diagnostics.join("\n"), /stage=clear reason=output-budget/);
  assert.doesNotMatch(diagnostics.join("\n"), /private-/);
});

test("invalid chunks and an unavailable overflow interruption fail instead of silently truncating", () => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  for (const bad of [undefined, "oversized"]) {
    const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
    output.enqueue(Buffer.alloc(bad ? 64002 : 640), bad ? { itemId: bad } : undefined);
    output.close();
  }
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  for (let n = 0; n <= 300; n++) output.enqueue(Buffer.alloc(64000), { itemId: "overflow" });
  assert.equal(failures, 3); assert.equal(media.sent.length, 0); output.close();
});

const conferenceGroup = 'synthetic-group@chatroom';
const conferenceInvitation: CallConferenceInvitation = { invitationId: 'synthetic-invitation', revision: 1, groupId: conferenceGroup, callerId: 'another-member' };
class ConferenceControl extends Control {
  joins = 0; creates = 0; invited: string[][] = []; joinedRevision?: number;
  constructor() { super(); this.conferenceCapabilities = true; }
  conferenceView(callId = 'conference'): CallView { return { ...view(callId), kind: 'conference', groupId: conferenceGroup, participants: [
    { participantId: 'a', contactId: 'contact-a', membership: 'joined', microphoneEnabled: true },
    { participantId: 'b', contactId: 'contact-b', membership: 'joined', microphoneEnabled: true },
  ] }; }
  async joinConference(_requestId: string, invitationId: string, revision: number) {
    assert.equal(invitationId, conferenceInvitation.invitationId); this.joins++; this.joinedRevision = revision;
    this.eventsQueue.push({ type: 'call.state', call: this.conferenceView() }); return accepted('conference', 'private-conference-lease');
  }
  async startConference(_requestId: string, groupId: string, invitees: string[]) {
    assert.equal(groupId, conferenceGroup); this.creates++; this.invited.push([...invitees]);
    this.eventsQueue.push({ type: 'call.state', call: { ...this.conferenceView(), direction: 'outgoing' } }); return accepted('conference', 'private-conference-lease');
  }
  async invite(requestId: string, callId: string, contacts: string[]) {
    this.invited.push([...contacts]); this.eventsQueue.push({ type: 'call.invitation_result', requestId, callId, results: contacts.map(contactId => ({ contactId, status: 'submitted', verificationRequired: false })) }); return accepted();
  }
}
function conferenceConfiguration(autoJoin = false) {
  const cfg = configuration();
  Object.assign(cfg.channels['agent-wechat'].calls.voice, { conferences: { groups: { [conferenceGroup]: { autoJoin } } } });
  return cfg;
}
async function conferenceFixture(autoJoin = false) {
  const cfg = conferenceConfiguration(autoJoin); const client = new ConferenceControl(); const voice = voiceFactory();
  const runtime = { config: { current: () => cfg } } as unknown as PluginRuntime;
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, runtime, { client, prepareVoice: async () => voice.prepared, host: cachedHost });
  await service.start(); return { cfg, client, voice, service };
}
test('conference auto-join is off without an exact configured group and never answers its inviter as a direct peer', async () => {
  const { service, client, voice } = await fixture();
  client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] });
  await delay(10); assert.equal(client.readied, 0); assert.equal(voice.state.opens, 0); assert.equal(client.answers, 0); await service.stop();
  const manual = await conferenceFixture();
  manual.client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] });
  await delay(10); assert.equal(manual.client.joins, 0);
  await manual.service.joinConference(conferenceGroup, { assertCurrent() {} }); await until(() => manual.client.attaches === 1);
  assert.equal(manual.client.joins, 1); assert.equal(manual.client.answers, 0);
  assert.equal(manual.voice.state.contexts[0].senderId, conferenceGroup);
  assert.deepEqual(manual.voice.state.contexts[0].conference, { groupId: conferenceGroup, inviterId: conferenceInvitation.callerId, policyAtAdmission: "none" }); await manual.service.stop();
});
test('an admitted conference mixes simultaneous tracks once, updates mute and source lifecycle, and leaves without dismissing the room', async () => {
  const { service, client, voice } = await conferenceFixture(true);
  client.media.receiveTracks = [7, 8].map((id, index) => ({ id, generation: 1, direction: 'receive', participantId: index ? 'b' : 'a', format }));
  client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] }); await until(() => client.attaches === 1);
  const pcm = (value: number) => { const bytes = Buffer.alloc(640); for (let i = 0; i < 320; i++) bytes.writeInt16LE(value, i * 2); return bytes; };
  client.media.input.push({ trackId: 7, generation: 1, sampleOffset: 0, sequence: 0, payload: pcm(1000) });
  client.media.input.push({ trackId: 8, generation: 1, sampleOffset: 0, sequence: 0, payload: pcm(2000) });
  await until(() => voice.state.incoming.some(bytes => bytes.readInt16LE(0) === 3000));
  assert.equal(voice.state.incoming.filter(bytes => bytes.readInt16LE(0) === 3000).length, 1);
  client.eventsQueue.push({ type: 'participant.updated', callId: 'conference', revision: 3, participant: { participantId: 'b', contactId: 'contact-b', membership: 'joined', microphoneEnabled: false } });
  await until(() => service.status().participants?.find(member => member.participantId === 'b')?.microphoneEnabled === false);
  assert.ok(client.media.sent.length > 0, 'room uplink must run even while the voice model is silent');
  const started = voice.state.incoming.length;
  await service.end(); assert.equal(client.ends, 1); assert.equal(client.media.closed, false);
  client.eventsQueue.push({ type: 'call.state', call: { ...client.conferenceView(), revision: 4, state: 'ended' } });
  client.eventsQueue.push({ type: 'calls.availability', availability: { busy: false, ownedByThisClient: false } });
  await until(() => client.media.closed); await delay(40); assert.equal(voice.state.incoming.length, started);
  assert.equal(client.joins, 1, 'the old invitation must not auto-join again after leave'); await service.stop();
});
test('withdrawn conference invitations and revoked group policy during voice preparation submit no join', async () => {
  for (const revoke of [false, true]) {
    const { service, client, voice, cfg } = await conferenceFixture(true);
    let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
    client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] }); await until(() => voice.state.opens === 1);
    if (revoke) (cfg.channels['agent-wechat'].calls.voice as any).conferences.groups = {};
    else client.eventsQueue.push({ type: 'conference.invitations', invitations: [] });
    await delay(5); release(); await until(() => voice.state.closes === 1);
    assert.equal(client.joins, 0); assert.equal(client.creates, 0); assert.equal(client.readied, 0); await service.stop();
  }
});
test('conference start and later invitations recheck destination allowlists and do not inherit a private dialing-session context', async () => {
  const { service, client, voice } = await conferenceFixture();
  let checks = 0; const access = { assertCurrent() { checks++; }, parentSession: { key: 'agent:voice-agent:private-owner-dm' } };
  await assert.rejects(service.startConference(conferenceGroup, ['unlisted'], access), /permitted/);
  assert.equal(client.creates, 0);
  await service.startConference(conferenceGroup, ['allowed-peer'], access); await until(() => client.attaches === 1);
  assert.equal(voice.state.contexts[0].parentSession, undefined);
  assert.ok(checks >= 4); assert.equal(client.creates, 1);
  await assert.rejects(service.invite(['unlisted'], access), /permitted/);
  await service.invite(['allowed-peer'], access); await until(() => !!service.status().invitationResults);
  assert.equal(service.status().invitationResults?.[0].status, 'submitted');
  assert.deepEqual(client.invited, [['allowed-peer'], ['allowed-peer']]); await service.stop();
});


test('conference inviter allowlist rejects others, accepts the listed inviter, and fences revocation before submission', async () => {
  const first = await conferenceFixture(true);
  (first.cfg.channels['agent-wechat'].calls.voice as any).conferences.groups[conferenceGroup].autoJoinAllowFrom = ['trusted-inviter'];
  first.client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] });
  await delay(30); assert.equal(first.client.joins, 0); assert.equal(first.voice.state.opens, 0);
  first.client.eventsQueue.push({ type: 'conference.invitations', invitations: [{ ...conferenceInvitation, callerId: 'trusted-inviter' }] });
  await until(() => first.client.attaches === 1); assert.equal(first.client.joins, 1); await first.service.stop();
  for (const replacement of [[], ['different-inviter']]) {
    const next = await conferenceFixture(true);
    const group = (next.cfg.channels['agent-wechat'].calls.voice as any).conferences.groups[conferenceGroup];
    group.autoJoinAllowFrom = [conferenceInvitation.callerId];
    let release!: () => void; next.voice.state.opening = new Promise(resolve => { release = resolve; });
    next.client.eventsQueue.push({ type: 'conference.invitations', invitations: [conferenceInvitation] });
    await until(() => next.voice.state.opens === 1); group.autoJoinAllowFrom = replacement; release();
    await until(() => next.voice.state.closes === 1); assert.equal(next.client.joins, 0); assert.equal(next.client.readied, 0); await next.service.stop();
  }
});

test("incoming and outgoing identities greet once only after connected media is ready", async t => {
  for (const direction of ["incoming", "outgoing"] as const) {
    const { service, client, voice } = await fixture(); t.after(() => service.stop());
    let release!: () => void;
    const gate = new Promise<void>(resolve => { release = resolve; });
    client.attachMedia = async () => { client.attaches++; await gate; return client.media; };
    if (direction === "incoming") client.eventsQueue.push({ type: "call.offered", offer });
    else await service.dial("allowed-peer", { assertCurrent() {} });
    await until(() => client.attaches === 1);
    await delay(280); assert.equal(voice.state.updates.length, 0);
    assert.equal(voice.state.contexts[0].callContext?.direction, direction);
    assert.equal(voice.state.contexts[0].callContext?.selfContactId, "contact-self");
    assert.equal(voice.state.contexts[0].callContext?.conversationPartner?.contactId, "allowed-peer");
    assert.deepEqual(voice.state.contexts[0].callContext?.remote, { contactId: "allowed-peer", name: "Known caller" });
    release(); await until(() => voice.state.updates.length === 1);
    assert.deepEqual(voice.state.updates[0].change, { initial: true });
    client.eventsQueue.push({ type: "call.state", call: { ...view(direction === "incoming" ? "offer" : "dial"), direction } });
    await delay(280); assert.equal(voice.state.updates.length, 1);
    assert.equal(client.clears, 0, "greetings never clear model output");
  }
});

test("conference context keeps its inviter, coalesces joins/leaves and stops updates on hangup", async t => {
  const { service, client, voice } = await conferenceFixture(true); t.after(() => service.stop());
  client.eventsQueue.push({ type: "conference.invitations", invitations: [conferenceInvitation] });
  await until(() => voice.state.updates.length === 1);
  assert.deepEqual(voice.state.contexts[0].callContext?.conference, { groupId: conferenceGroup, name: "Public test room", inviter: { contactId: "another-member", name: "Room another-member" } });
  assert.deepEqual(voice.state.updates[0].snapshot.participants.map(p => p.name), ["Room contact-a", "Room contact-b"]);
  const participant = (id: string, membership: "joined" | "left" | "invited" = "joined") => ({ participantId: id, contactId: `contact-${id}`, membership });
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 3, participant: participant("c") });
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 3, participant: participant("a", "left") });
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 3, participant: participant("d", "invited") });
  // A canceled join inside the same window produces no greeting or farewell.
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 3, participant: participant("e") });
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 4, participant: participant("e", "left") });
  await until(() => voice.state.updates.length === 2);
  assert.deepEqual(voice.state.updates[1].change, { initial: false, joined: [{ contactId: "contact-c", name: "Room contact-c" }], left: [{ contactId: "contact-a", name: "Room contact-a" }] });
  client.eventsQueue.push({ type: "call.state", call: client.conferenceView() }); // stale snapshot
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 5, participant: { ...participant("b"), microphoneEnabled: false } });
  await delay(280); assert.equal(voice.state.updates.length, 2);
  client.eventsQueue.push({ type: "participant.updated", callId: "conference", revision: 6, participant: participant("a") });
  await delay(20); await service.end(); await delay(280);
  assert.equal(voice.state.updates.length, 2);
  assert.equal(client.clears, 0);
});

test("delayed conference names reschedule context and deferred provider submission retains the update", async t => {
  const cfg = conferenceConfiguration(true), client = new ConferenceControl(), voice = voiceFactory();
  const lines: string[] = []; let release!: () => void, admit = true;
  const pending = new Promise<void>(resolve => { release = resolve; });
  const host = (async () => ({ client: {
    group: async () => ({ name: "Test room" }),
    groupMember: async (_account: string, _group: string, who: string) => {
      if (who.startsWith("contact-")) await pending;
      return { member: { username: who, displayName: `Name ${who}` } };
    },
  } })) as unknown as NonNullable<import("./calls-service.ts").CallServiceDependencies["host"]>;
  const open = voice.prepared.open;
  voice.prepared.open = async (...args) => {
    const session = await open(...args), send = session.updateCallContext;
    session.updateCallContext = (snapshot, change) => {
      if (!admit) return false;
      send(snapshot, change); return true;
    };
    return session;
  };
  const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, { config: { current: () => cfg } } as unknown as PluginRuntime,
    { client, prepareVoice: async () => voice.prepared, host, info: line => lines.push(line) });
  await service.start(); t.after(() => service.stop());
  client.eventsQueue.push({ type: "conference.invitations", invitations: [conferenceInvitation] });
  await until(() => voice.state.updates.length === 1);
  assert.deepEqual(voice.state.updates[0].snapshot.participants.map(p => p.name), ["contact-a", "contact-b"]);
  admit = false; release(); await delay(300); assert.equal(voice.state.updates.length, 1);
  admit = true; await until(() => voice.state.updates.length === 2);
  assert.deepEqual(voice.state.updates[1].change, { initial: false, joined: [], left: [], metadataOnly: true });
  assert.deepEqual(voice.state.updates[1].snapshot.participants.map(p => p.name), ["Name contact-a", "Name contact-b"]);
  assert.ok(lines.some(line => /stage=lookup lookup=\d+ named=true ms=\d+/.test(line)));
  assert.equal(lines.some(line => /contact-a|contact-b|Name |another-member|Test room/.test(line)), false, "default diagnostics never contain names or contact IDs");
});

test("manual conference admission fences a changed inviter even at the same invitation revision", async t => {
  const { service, client, voice } = await conferenceFixture(); t.after(() => service.stop());
  let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
  client.eventsQueue.push({ type: "conference.invitations", invitations: [conferenceInvitation] }); await delay(5);
  const joining = service.joinConference(conferenceGroup, { assertCurrent() {} });
  await until(() => voice.state.opens === 1);
  client.eventsQueue.push({ type: "conference.invitations", invitations: [{ ...conferenceInvitation, callerId: "changed-inviter" }] });
  await delay(5); release(); await assert.rejects(joining, /not admitted/); assert.equal(client.joins, 0);
});

test("owner consultation controls its conference without a chat-requester grant and cannot affect a later call", async t => {
  const { cfg, service, client, voice } = await conferenceFixture(true); t.after(() => service.stop());
  voice.state.binding = { sessionKey: "agent:voice-agent:wechat-conference:synthetic:owner", owner: true };
  client.eventsQueue.push({ type: "conference.invitations", invitations: [conferenceInvitation] });
  await until(() => client.attaches === 1);
  const context: OpenClawPluginToolContext<2> = { config: cfg as any, agentId: "voice-agent", agentAccountId: "default",
    requesterSenderId: conferenceGroup, senderIsOwner: true, sessionKey: voice.state.binding.sessionKey, assertInvocationCurrent() {} };
  let create!: (context: OpenClawPluginToolContext<2>) => Array<{ execute(id: string, args: unknown): Promise<unknown> }>;
  registerNativeCallTool({ registerTool(factory: any) { create = factory.create; } } as unknown as OpenClawPluginApi, () => service);
  const [tool] = create(context);
  assert.equal(cfg.channels["agent-wechat"].calls.voice.outboundToolAllowFrom.includes(conferenceGroup), false);
  await tool.execute("status", { action: "status" });
  await assert.rejects(tool.execute("invite", { action: "invite", contactIds: ["unlisted"] }), /admitted|destination|permitted/);
  assert.equal(client.invited.length, 0, "destination allowlists remain in force");
  voice.state.binding.owner = false;
  await assert.rejects(tool.execute("revoked", { action: "hangup" }), /consultation/); assert.equal(client.ends, 0);
  voice.state.binding.owner = true;
  const [foreign] = create({ ...context, requesterSenderId: "foreign" });
  await assert.rejects(foreign.execute("foreign", { action: "hangup" }), /consultation/);
  const [nonOwner] = create({ ...context, senderIsOwner: false });
  await assert.rejects(nonOwner.execute("non-owner", { action: "hangup" }), /not permitted/);
  const [wrongAccount] = create({ ...context, agentAccountId: "another-account" });
  await assert.rejects(wrongAccount.execute("wrong-account", { action: "hangup" }), /consultation/);
  await tool.execute("end", { action: "hangup" }); assert.equal(client.ends, 1);
  client.eventsQueue.push({ type: "call.state", call: { ...client.conferenceView(), revision: 9, state: "ended" } });
  await until(() => service.status().state === "available"); client.media = new Media();
  await service.startConference(conferenceGroup, ["allowed-peer"], { assertCurrent() {} }); await until(() => client.attaches === 2);
  await assert.rejects(tool.execute("stale", { action: "hangup" }), /consultation/);
  assert.equal(client.ends, 1, "an old invocation cannot hang up the next call even with the same session and synthetic call ID");
});

test("a direct owner consultation can hang up without an outbound chat-requester grant", async t => {
  const { service, client, voice } = await fixture(); t.after(() => service.stop());
  voice.state.binding = { owner: true, sessionKey: "agent:voice-agent:wechat-call:allowed-peer" };
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => client.attaches === 1);
  const cfg = configuration(); let tool!: { execute(id: string, input: unknown): Promise<unknown> };
  const context = { config: cfg as any, agentId: "voice-agent", requesterSenderId: "allowed-peer", senderIsOwner: true,
    sessionKey: voice.state.binding.sessionKey, assertInvocationCurrent() {} };
  registerNativeCallTool({ registerTool(factory: any) { [tool] = factory.create(context); } } as unknown as OpenClawPluginApi, () => service);
  await tool.execute("end", { action: "hangup" }); assert.equal(client.ends, 1);
});


test("call metadata resolves our own account and exact owner matches without changing the tool policy", async t => {
  const cfg = configuration(); (cfg as any).commands = { ownerAllowFrom: ["agent-wechat:allowed-peer", "agent-wechat:*"] };
  const { service, client, voice } = await fixture(undefined, cfg); t.after(() => service.stop());
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => client.attaches === 1);
  assert.equal(voice.state.contexts[0].callContext?.remote?.configuredOwner, true);
  assert.equal(voice.state.contexts[0].callContext?.selfContactId, "contact-self");
  assert.equal(voice.state.contexts[0].policyAtAdmission, "none", "relationship metadata does not grant tools");
});
