import test from "node:test";
import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { link, mkdir, mkdtemp, readFile, rm, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { callsConfigSchema, resolveWeChatCallsConfig } from "./calls-config.ts";
import { prepareOpenClawCallVoice, type VoiceCallbacks } from "./calls-voice.ts";

const pcm = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
function config(consult = false) { return resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "openai" }, agentId: "agent", toolPolicy: consult ? "safe-read-only" : "none", allowConsultSessionPersistence: consult } })!.voice!; }
const callbacks: VoiceCallbacks = { audio() {}, clear() {}, failed() {} };
type FakeDispatch = { ctx: any; cfg: unknown; replyOptions: any; dispatcherOptions: any };
/** Stands in for OpenClaw's normal session runner. `answer` starts the run,
 * throws, or returns the final text; `deferred` leaves the run unfinished. */
function fakeReply(answer: (dispatch: FakeDispatch) => Promise<string | { text: string; deferred: true }>) {
  const dispatches: FakeDispatch[] = [];
  const reply = {
    finalizeInboundContext: (ctx: unknown) => ctx,
    async dispatchReplyWithBufferedBlockDispatcher(params: FakeDispatch) {
      dispatches.push(params);
      const result = await answer(params);
      await params.dispatcherOptions.deliver({ text: "synthetic tool progress" }, { kind: "tool" });
      await params.dispatcherOptions.deliver({ text: typeof result === "string" ? result : result.text }, { kind: "final" });
      if (typeof result === "string") params.replyOptions.onAgentRunTerminalOutcome("completed");
    },
  };
  return { dispatches, reply };
}
const READ_ONLY_TOOLS = ["read", "web_search", "web_fetch", "x_search", "memory_search", "memory_get"];

test("calling schemas agree across runtime and installed plugin manifests", async () => {
  const manifest = JSON.parse(await readFile(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.deepEqual(manifest.configSchema.properties.calls, callsConfigSchema);
  assert.deepEqual(manifest.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.deepEqual(pkg.openclaw.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.equal(manifest.toolMetadata.wechat_call.optional, true);
  const credentialPath = "calls.voice.realtime.providers.openai.apiKey";
  assert.ok(manifest.configContracts.secretInputs.paths.some((item: any) => item.path === credentialPath && item.expected === "string"));
  assert.equal(manifest.uiHints[credentialPath].sensitive, true);

});

test("installed host bridge and streaming resampler work with an account-free fake provider", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audio] = await Promise.all([import(voicePath), import(audioPath)]);
  let providerRequest: any; let bridgeParams: any; const input: Buffer[] = []; let closed = 0; let connected = false;
  const fake = { id: "openai", createBridge(request: any) { providerRequest = request; return {
    connect: async () => { connected = true; request.onReady(); }, isConnected: () => connected, outputAudioMode: "response",
    sendAudio: (value: Buffer) => input.push(value), setMediaTimestamp() {}, acknowledgeMark() {}, submitToolResult() {},
    close: () => { closed++; connected = false; },
  }; } };
  let resolved = 0; let selection: any;
  const customized = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "agent", realtime: {
    provider: "openai", model: "synthetic-realtime", voice: "synthetic-voice", instructions: "Use short sentences.", consultPolicy: "always",
    providers: { openai: { apiKey: "synthetic-openai-secret" } },
  } } })!.voice!;
  const cfg = { agents: { list: [
    { id: "main", default: true, identity: { name: "Synthetic default" } },
    { id: "agent", identity: { name: "Synthetic selected", theme: "Dry and precise" } },
  ] } };
  const prepared = await prepareOpenClawCallVoice(customized, cfg, {} as PluginRuntime, {
    voiceSdk: { ...sdk, createRealtimeVoiceBridgeSession: (params: unknown) => { bridgeParams = params; return sdk.createRealtimeVoiceBridgeSession(params); }, resolveConfiguredRealtimeVoiceProvider: (params: any) => { selection = params; resolved++; return { provider: fake, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }; } }, audioSdk: audio,
  });
  assert.deepEqual(selection.providerConfigOverrides, { model: "synthetic-realtime", voice: "synthetic-voice" });
  assert.equal(selection.surface, "gateway-relay");
  assert.equal(prepared.continuous, false);
  assert.equal(selection.providerConfigs.openai.apiKey, "synthetic-openai-secret");
  assert.equal(JSON.stringify(customized).includes("synthetic-openai-secret"), false);
  assert.equal(resolved, 1); let heard = 0; let clears = 0;
  const session = await prepared.open({ audio: (_value, metadata) => { heard++; assert.equal(metadata?.itemId, "item"); }, clear: () => { clears++; }, failed() { assert.fail("Unexpected provider failure"); } }, { senderId: "synthetic-peer", signal: new AbortController().signal });
  assert.equal(providerRequest.instructions, bridgeParams.instructions);
  assert.equal(providerRequest.agentId, "agent");
  assert.match(bridgeParams.instructions, /Synthetic selected/);
  assert.match(bridgeParams.instructions, /Dry and precise/);
  assert.match(bridgeParams.instructions, /Use short sentences/);
  assert.match(bridgeParams.instructions, /does not grant account-owner authority/);
  assert.match(bridgeParams.instructions, /conversation-only/);
  assert.doesNotMatch(bridgeParams.instructions, /Synthetic default|openclaw_agent_consult|synthetic-openai-secret/);
  session.sendAudio(Buffer.alloc(960)); assert.equal(input.length, 1);
  providerRequest.onAudio(Buffer.alloc(960), { itemId: "item" }); providerRequest.onClearAudio("barge-in"); assert.equal(heard, 1); assert.equal(clears, 1);
  assert.deepEqual(providerRequest.tools, []); assert.equal(providerRequest.runAgentConsult, undefined); assert.equal(typeof bridgeParams.onTranscript, "function");
  providerRequest.onTranscript("user", "Synthetic discarded transcript", true);
  await session.close(); providerRequest.onAudio(Buffer.alloc(960), { itemId: "late" }); assert.equal(heard, 1); assert.equal(closed, 1);
  const generated = Buffer.alloc(3200); for (let i = 0; i < 1600; i++) generated.writeInt16LE(Math.round(4000 * Math.sin(i * 2 * Math.PI * 440 / 16000)), i * 2);
  const whole = prepared.resampler(16000, 24000); const expected = Buffer.concat([whole.process(generated), whole.flush()]);
  const stream = prepared.resampler(16000, 24000); const pieces: Buffer[] = [];
  for (let i = 0; i < generated.length; i += 83) pieces.push(stream.process(generated.subarray(i, i + 83)));
  pieces.push(stream.flush()); assert.deepEqual(Buffer.concat(pieces), expected); assert.equal(expected.length, 4800);
});

test("a continuous model streams without the consult tool and delegates through the agent's normal route", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audio] = await Promise.all([import(voicePath), import(audioPath)]);
  const live = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: false, handlesInputAudioBargeIn: true,
    supportsToolCalls: false, handlesAgentConsult: true };
  let request: any; let selection: any; let outputAudioMode = "continuous";
  const fake = { id: "openai", createBridge(params: any) { request = params; let connected = false; return {
    connect: async () => { connected = true; params.onReady(); }, isConnected: () => connected, outputAudioMode, pacesInputAudio: true,
    sendAudio() {}, setMediaTimestamp() {}, close: () => { connected = false; },
  }; } };
  const { dispatches, reply } = fakeReply(async ({ replyOptions }) => { replyOptions.onAgentRunStart(); return "Synthetic delegated result"; });
  const runtime = { channel: { reply }, agent: { session: { resolveStorePath: () => "/synthetic/store", getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  const dependencies = { voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: (params: any) => {
    selection = params; return { provider: fake, providerConfig: {}, capabilities: live };
  } }, audioSdk: audio };
  const signal = new AbortController().signal;
  const failed = () => assert.fail("Unexpected provider failure");

  const prepared = await prepareOpenClawCallVoice(config(true), {}, runtime, dependencies);
  assert.equal(prepared.continuous, true);
  assert.equal(selection.surface, "gateway-relay");
  const heard: Array<string | undefined> = []; let clears = 0;
  const session = await prepared.open({ audio: (_value, metadata) => { heard.push(metadata?.itemId); }, clear: () => { clears++; }, failed }, { senderId: "synthetic-peer", signal });
  assert.deepEqual(request.tools, []);
  assert.equal(request.interruptResponseOnInputAudio, false);
  assert.match(request.instructions, /Delegate relevant lookups/);
  assert.doesNotMatch(request.instructions, /openclaw_agent_consult/);
  // Delegation sends the request with recent transcript, beyond a tool question's limit.
  const prompt = `<realtime_delegation><input>Read synthetic data</input><transcript_delta>${"caller: synthetic words. ".repeat(250)}</transcript_delta></realtime_delegation>`;
  assert.ok(prompt.length > 4000);
  assert.deepEqual(await request.runAgentConsult({ prompt, signal }), { text: "Synthetic delegated result" });
  assert.equal(dispatches.at(-1)!.ctx.Body, prompt);
  assert.deepEqual(dispatches.at(-1)!.replyOptions.toolsAllow, READ_ONLY_TOOLS);
  request.onAudio(Buffer.alloc(960)); request.onClearAudio("barge-in"); request.onAudio(Buffer.alloc(960));
  assert.deepEqual(heard, [undefined, undefined]); assert.equal(clears, 1);
  await session.close();

  // GPT-Live needs a delegation handler even when consultation is off; it refuses.
  const conversationOnly = await prepareOpenClawCallVoice(config(), {}, runtime, dependencies);
  const quiet = await conversationOnly.open({ ...callbacks, failed }, { senderId: "synthetic-peer", signal });
  assert.doesNotMatch(request.instructions, /Delegate/);
  await assert.rejects(request.runAgentConsult({ prompt: "Read synthetic data", signal }));
  assert.equal(dispatches.length, 1);
  await quiet.close();

  const always = config(true); always.realtime.consultPolicy = "always";
  await assert.rejects(prepareOpenClawCallVoice(always, {}, runtime, dependencies), /capabilities unavailable/);
  outputAudioMode = "response";
  await assert.rejects(prepared.open({ ...callbacks, failed }, { senderId: "synthetic-peer", signal }), /provider unavailable/);
});

test("opted-in default-agent voice profile uses only bounded identity and persona files", async t => {
  const dir = await mkdtemp(join(tmpdir(), "wechat-call-voice-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const workspace = join(dir, "workspace");
  await mkdir(workspace);
  const identity = join(workspace, "IDENTITY.md");
  const privateFile = join(dir, "unrelated.md");
  await Promise.all([
    writeFile(identity, "- Name: Synthetic Main Voice"),
    writeFile(join(workspace, "SOUL.md"), "Speak with quiet curiosity.\n" + "x".repeat(8000)),
    writeFile(join(workspace, "USER.md"), "private-user-sentinel"),
    writeFile(join(workspace, "AGENTS.md"), "private-workspace-sentinel"),
    writeFile(privateFile, "private-hardlink-sentinel"),
  ]);
  const voice = { enabled: true, agentId: "main", realtime: { provider: "openai" } };
  const base = resolveWeChatCallsConfig({ voice })!.voice!;
  const optedIn = resolveWeChatCallsConfig({ voice: { ...voice, realtime: { ...voice.realtime, agentContext: { enabled: true } } } })!.voice!;
  assert.equal(base.realtime.agentContext.enabled, false);
  assert.equal(base.realtime.consultPolicy, "auto");
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, realtime: {
    ...voice.realtime, agentContext: { enabled: true, files: ["USER.md"] } as any,
  } } }), /Invalid calling configuration/);
  let instructions = "";
  const voiceSdk = {
    createRealtimeVoiceBridgeSession(args: any) {
      instructions = args.instructions;
      return { bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(),
        sendAudio() {}, setMediaTimestamp() {}, close() {} };
    },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {},
      capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }),
    resolveRealtimeVoiceAgentConsultTools: () => [],
    resolveRealtimeVoiceAgentConsultToolsAllow: () => [],
  };
  // Legacy main resolves the host default workspace without an agents.list entry.
  const cfg = { agents: { defaults: { workspace } } };
  const prepare = (value: typeof base) => prepareOpenClawCallVoice(value, cfg, {} as PluginRuntime,
    { voiceSdk, audioSdk: { createStreamingPcmResampler() {} } });
  async function prompt(prepared: Awaited<ReturnType<typeof prepare>>) {
    const session = await prepared.open(callbacks, { senderId: "peer", signal: new AbortController().signal });
    await session.close();
    return instructions;
  }
  assert.doesNotMatch(await prompt(await prepare(base)), /Synthetic Main Voice|quiet curiosity/);
  const prepared = await prepare(optedIn);
  const profile = await prompt(prepared);
  assert.match(profile, /IDENTITY\.md\):\n- Name: Synthetic Main Voice/);
  assert.match(profile, /SOUL\.md\):\nSpeak with quiet curiosity/);
  assert.match(profile, /\[truncated\]/);
  assert.ok(profile.length < 7200);
  assert.doesNotMatch(profile, /USER\.md|AGENTS\.md|private-/);
  await unlink(identity);
  await link(privateFile, identity);
  assert.doesNotMatch(await prompt(prepared), /private-hardlink-sentinel|Synthetic Main Voice/);
});

test("missing SDK and incompatible provider capabilities fail before creating a bridge", async () => {
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: {}, audioSdk: {} }), /SDK unavailable/);
  let created = 0;
  const sdk = { createRealtimeVoiceBridgeSession() { created++; }, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai" }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [], supportsBargeIn: true } }), resolveRealtimeVoiceAgentConsultTools() {}, resolveRealtimeVoiceAgentConsultToolsAllow() {} };
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } }), /capabilities unavailable/);
  assert.equal(created, 0);
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: { ...sdk,
    resolveConfiguredRealtimeVoiceProvider() { throw Error("synthetic-private-provider-detail"); },
  }, audioSdk: { createStreamingPcmResampler() {} } }), error => error instanceof Error && error.message === "Calling realtime provider configuration unavailable");

});

test("opted-in consultation is isolated, non-owner, policy-limited and deduplicated", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  let bridgeArgs: any; const results: unknown[] = []; const spoken: string[] = []; let closed = 0;
  const instance = new AsyncLocalStorage<{ active: boolean }>();
  const runInBackgroundContext = <T>(run: () => T): T => {
    const lease = { active: true };
    const result = instance.run(lease, run);
    if (result instanceof Promise) return result.finally(() => { lease.active = false; }) as T;
    lease.active = false;
    return result;
  };
  const voiceContext = { senderId: "synthetic-peer", signal: new AbortController().signal, runInBackgroundContext };
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {},
      close() { closed++; }, submitToolResult: (_id: string, value: unknown) => { results.push(value); },
      sendUserMessage(text: string) { spoken.push(text); },
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
    resolveRealtimeVoiceAgentConsultTools: host.resolveRealtimeVoiceAgentConsultTools, resolveRealtimeVoiceAgentConsultToolsAllow: host.resolveRealtimeVoiceAgentConsultToolsAllow,
  };
  const { dispatches: consultations, reply } = fakeReply(async ({ replyOptions }) => {
    assert.equal(instance.getStore()?.active, true, "consultation must enter the current plugin instance");
    replyOptions.onAgentRunStart();
    await delay(0);
    assert.equal(instance.getStore()?.active, true, "consultation must retain the instance across awaits");
    return "Synthetic result";
  });
  const lookups: string[] = [];
  let parentId = "parent-id";
  const runtime = { channel: { reply }, agent: { session: {
    resolveStorePath: () => "/synthetic/store",
    getSessionEntry: (args: { sessionKey: string }) => {
      assert.equal(instance.getStore()?.active, true, "parent reads require the current plugin instance");
      lookups.push(args.sessionKey);
      return args.sessionKey === "agent:agent:conversation" ? { sessionId: parentId } : undefined;
    },
  } } } as unknown as PluginRuntime;
  const consulted = config(true); consulted.consultation = { timeoutMs: 15000, summary: true };
  consulted.realtime.consultPolicy = "substantive";
  consulted.realtime.consultThinkingLevel = "minimal";
  consulted.realtime.consultFastMode = true;
  // Each call in its own session: this test checks isolation and parent seeding.
  consulted.sessionScope = "per-call";
  const hostCfg = { session: { mainKey: "conversation" } };
  const dependencies = { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } };
  const prepared = await prepareOpenClawCallVoice(consulted, hostCfg, runtime, dependencies);
  const session = await runInBackgroundContext(() => prepared.open(callbacks, voiceContext));
  const invoke = async (event: unknown) => {
    await bridgeArgs.onToolCall(event);
    await delay(0);
  };
  assert.deepEqual(bridgeArgs.tools.map((tool: { name: string }) => tool.name), ["openclaw_agent_consult"]);
  assert.match(bridgeArgs.instructions, /Call openclaw_agent_consult before answering/);
  assert.match(bridgeArgs.instructions, /Consultation is read-only/);
  const event = { callId: "tool1", name: "openclaw_agent_consult", args: { question: "Read synthetic data" } };
  // A provider callback may inherit an already released request context.
  await instance.run({ active: false }, () => invoke(event));
  await invoke(event);
  for (let i = 0; i < 100 && !spoken.length; i++) await delay(10);
  assert.equal(consultations.length, 1); assert.equal(results.length, 1);
  assert.match(spoken[0], /Synthetic result/); assert.doesNotMatch(spoken[0], /synthetic tool progress/);
  const first = consultations[0];
  assert.equal(first.ctx.Body, "Read synthetic data"); assert.equal(first.ctx.SenderId, "synthetic-peer");
  assert.equal(first.replyOptions.thinkingLevelOverride, "minimal"); assert.equal(first.replyOptions.fastModeOverride, true);
  assert.deepEqual(first.replyOptions.toolsAllow, READ_ONLY_TOOLS);
  assert.equal(first.ctx.ParentSessionKey, undefined);
  assert.match(first.ctx.BodyForAgent, /non-owner WeChat caller/);
  assert.match(first.ctx.SessionKey, /^agent:agent:wechat-call:[0-9a-f-]{36}$/); assert.equal(typeof bridgeArgs.onTranscript, "function");
  await bridgeArgs.onToolCall({ ...event, callId: "tool2", name: "exec" }); assert.equal(consultations.length, 1);
  assert.deepEqual(await bridgeArgs.runAgentConsult({ prompt: "Check a synthetic fact" }), { text: "Synthetic result" });
  assert.equal(consultations[1].ctx.SessionKey, first.ctx.SessionKey);
  await session.close(); await bridgeArgs.onToolCall({ ...event, callId: "late" }); assert.equal(consultations.length, 2); assert.equal(closed, 1);

  const parentCases = [
    { key: "agent:agent:main", expectedSessionId: "parent-id", forked: true },
    { key: "agent:agent:main", expectedSessionId: "replaced-parent", forked: false },
    { key: "agent:agent:missing", forked: false },
    { key: "agent:main:main", forked: false },
    { key: "global", forked: false },
    { key: "agent:agent:dashboard:incognito-private", forked: false },
  ];
  for (const { forked, ...parentSession } of parentCases) {
    const next = await runInBackgroundContext(() => prepared.open(callbacks, { ...voiceContext, parentSession }));
    await invoke(event);
    const request = consultations.at(-1)!;
    assert.equal(request.ctx.ParentSessionKey, forked ? "agent:agent:conversation" : undefined);
    assert.deepEqual(request.replyOptions.toolsAllow, READ_ONLY_TOOLS);
    await next.close();
  }
  assert.deepEqual(lookups.filter(key => !key.includes(":wechat-call:")), ["agent:agent:conversation", "agent:agent:conversation", "agent:agent:conversation", "agent:agent:missing"]);
  assert.equal(new Set(consultations.map(request => request.ctx.SessionKey)).size, parentCases.length + 1);
  const afterReset = await runInBackgroundContext(() => prepared.open(callbacks, { ...voiceContext,
    parentSession: { key: "agent:agent:main", expectedSessionId: "parent-id" } }));
  parentId = "new-parent-id";
  await invoke(event);
  assert.equal(consultations.at(-1)!.ctx.ParentSessionKey, undefined);
  await afterReset.close();
  const invalidInbound = config(true);
  invalidInbound.consultation.inboundSessionKey = "agent:main:main";
  await assert.rejects(prepareOpenClawCallVoice(invalidInbound, hostCfg, runtime, dependencies), /Invalid calling consultation session/);
  invalidInbound.consultation.inboundSessionKey = "agent:agent:main";
  await assert.rejects(prepareOpenClawCallVoice(invalidInbound, { session: { scope: "global" } }, runtime, dependencies), /Invalid calling consultation session/);

  const always = config(true);
  always.realtime.consultPolicy = "always";
  const alwaysPrepared = await prepareOpenClawCallVoice(always, hostCfg, runtime, dependencies);
  spoken.length = 0;
  let clears = 0;
  const alwaysSession = await runInBackgroundContext(() => alwaysPrepared.open({ ...callbacks, clear: () => { clears++; } }, voiceContext));
  assert.match(bridgeArgs.instructions, /before every substantive answer/);
  instance.run({ active: false }, () => bridgeArgs.onTranscript("user", "Check the status of the synthetic deployment", true));
  for (let i = 0; i < 100 && !spoken.length; i++) await delay(10);
  assert.equal(spoken.length, 1);
  assert.match(spoken[0], /Internal OpenClaw voice playback result/);
  assert.match(spoken[0], /Synthetic result/);
  assert.equal(clears, 2);
  const fallback = consultations.at(-1)!;
  assert.equal(fallback.ctx.Body, "Check the status of the synthetic deployment");
  assert.equal(fallback.replyOptions.thinkingLevelOverride, undefined);
  assert.deepEqual(fallback.replyOptions.toolsAllow, READ_ONLY_TOOLS);
  await alwaysSession.close();
});
test("unverified providers and a bridge contradicting its capabilities fail without connecting or falling back", async () => {
  let connects = 0; let closes = 0;
  const caps = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true };
  const sdk = { createRealtimeVoiceBridgeSession: () => ({ bridge: { outputAudioMode: "continuous" }, connect: async () => { connects++; }, close() { closes++; }, sendAudio() {}, setMediaTimestamp() {} }),
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "other", createBridge() {} }, providerConfig: {}, capabilities: caps }),
    resolveRealtimeVoiceAgentConsultTools: () => [], resolveRealtimeVoiceAgentConsultToolsAllow: () => [] };
  const audioSdk = { createStreamingPcmResampler() {} };
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: sdk, audioSdk }), /provider unsupported/);
  const prepared = await prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: caps }) }, audioSdk });
  await assert.rejects(prepared.open(callbacks, { senderId: "peer", signal: new AbortController().signal }), /provider unavailable/);
  assert.equal(connects, 0); assert.equal(closes, 1);
});

test("owner consultation is scoped to the connected WeChat peer and drops privileges on revocation", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  const ownerPeer = "synthetic-owner";
  const otherPeer = "synthetic-other";
  const parentKey = "agent:agent:owner-chat";
  const settings = (owners: string[], toolPolicy: "owner" | "safe-read-only" | "none" = "owner") => ({
    commands: { ownerAllowFrom: owners },
    channels: { "agent-wechat": {
      native: { mode: "external", accountId: "native-account", endpoint: "http://127.0.0.1:1",
        token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" },
      calls: { voice: { enabled: true, agentId: "agent", toolPolicy, allowConsultSessionPersistence: true, realtime: { provider: "openai" } } },
    } },
  });
  const exact = settings([`agent-wechat:${ownerPeer}`]);
  let live: ReturnType<typeof settings> | undefined = settings([
    `signal:${ownerPeer}`, `wechat:${ownerPeer}`, `agent-wechat:${ownerPeer.toUpperCase()}`, ownerPeer, "agent-wechat:*", "*",
  ]);
  let deliveryChannel = "agent-wechat";
  let release!: () => void;
  const hold = new Promise<void>(resolve => { release = resolve; });
  const { dispatches: requests, reply } = fakeReply(async ({ ctx, replyOptions }) => {
    if (ctx.Body === "simulate registry failure") {
      throw new Error("synthetic-private-value", { cause: new Error("Plugin registry scope is no longer available") });
    }
    if (ctx.Body === "simulate workspace failure") throw Object.assign(new Error("/synthetic/private/workspace"), { code: "EACCES" });
    replyOptions.onAgentRunStart();
    if (ctx.Body === "simulate run failure") throw Error("synthetic-private-value");
    if (ctx.Body === "simulate deferred run") return { text: "synthetic unfinished reply", deferred: true };
    if (ctx.Body === "hold owner result") await hold;
    return replyOptions.toolsAllow ? "synthetic-read-only-result" : "synthetic-privileged-result";
  });
  const runtime = { config: { current: () => live }, channel: { reply }, agent: { session: {
    resolveStorePath: () => "/synthetic/store",
    getSessionEntry: ({ sessionKey }: { sessionKey: string }) => sessionKey === parentKey ? {
      sessionId: "trusted-parent",
      delivery: { kind: "external", context: { channel: deliveryChannel, to: `wechat:${ownerPeer}`, accountId: "default" } },
    } : undefined,
  } } } as unknown as PluginRuntime;
  const consulted = resolveWeChatCallsConfig(exact.channels["agent-wechat"].calls)!.voice!;
  let bridgeArgs: any;
  const toolPolicies: string[] = [];
  const results: unknown[] = [];
  const spoken: string[] = [];
  const trace: string[] = [];
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(),
      sendAudio() {}, setMediaTimestamp() {}, close() {},
      submitToolResult(_id: string, result: unknown) { results.push(result); },
      sendUserMessage(text: string) { spoken.push(text); },
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {},
      capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
    resolveRealtimeVoiceAgentConsultTools(policy: string) {
      toolPolicies.push(policy);
      return host.resolveRealtimeVoiceAgentConsultTools(policy);
    },
  };
  const dependencies = { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } };
  await assert.rejects(prepareOpenClawCallVoice(consulted, exact, { ...runtime, channel: undefined } as unknown as PluginRuntime, dependencies),
    /Calling consultation unavailable/);
  const prepared = await prepareOpenClawCallVoice(consulted, exact, runtime, dependencies);
  const open = (senderId: string, connectedPeer = () => senderId, signal = new AbortController().signal) => prepared.open(callbacks, {
    senderId, connectedPeer, nativeAccountId: "native-account", channelAccountId: "default", logAudioTrace: line => trace.push(line),
    parentSession: { key: parentKey, expectedSessionId: "trusted-parent" }, signal,
  });
  const consult = async (callId: string, question = "Read a synthetic value") => {
    await bridgeArgs.onToolCall({
      name: host.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId, args: { question, senderIsOwner: true },
    });
    await delay(0);
  };
  const assertReadOnly = (peer: string) => {
    const request = requests.at(-1)!;
    assert.equal(request.ctx.SenderId, peer);
    assert.deepEqual(request.replyOptions.toolsAllow, host.resolveRealtimeVoiceAgentConsultToolsAllow("safe-read-only"));
    assert.equal(request.ctx.ParentSessionKey, undefined);
    assert.match(request.ctx.BodyForAgent, /non-owner WeChat caller/);
  };

  const wrongProvider = await open(ownerPeer);
  assert.match(bridgeArgs.instructions, /Consultation is read-only/);
  assert.equal(toolPolicies.at(-1), "safe-read-only");
  await consult("wrong-provider");
  assertReadOnly(ownerPeer);
  live = exact;
  await consult("never-promote");
  assertReadOnly(ownerPeer);
  await wrongProvider.close();

  const nonOwner = await open(otherPeer);
  await consult("owner-requester-is-not-the-peer");
  assertReadOnly(otherPeer);
  await nonOwner.close();

  deliveryChannel = "signal";
  const foreignRoute = await open(ownerPeer);
  await consult("no-foreign-route");
  assert.equal(requests.at(-1)!.replyOptions.toolsAllow, undefined);
  assert.equal(requests.at(-1)!.ctx.ParentSessionKey, undefined);
  await foreignRoute.close();

  deliveryChannel = "agent-wechat";
  let connected = otherPeer;
  const owner = await open(ownerPeer, () => connected);
  assert.match(bridgeArgs.instructions, /Normal agent tools may be available/);
  assert.equal(toolPolicies.at(-1), "owner");
  const beforeAttach = requests.length;
  await consult("preflight-peer-mismatch");
  assert.equal(requests.length, beforeAttach);
  assert.deepEqual(results.at(-1), { error: "Calling consultation not admitted" });
  assert.equal(trace.length, beforeAttach * 2);
  connected = ownerPeer;
  await consult("cached-owner-result");
  const privileged = requests.at(-1)!;
  assert.equal(privileged.ctx.SenderId, ownerPeer);
  assert.equal(privileged.replyOptions.toolsAllow, undefined);
  assert.match(privileged.ctx.BodyForAgent, /verified WeChat owner/);
  assert.equal(privileged.ctx.ParentSessionKey, parentKey);
  assert.equal(privileged.cfg, live);
  assert.equal(privileged.ctx.Provider, "agent-wechat");
  assert.equal(privileged.ctx.Surface, "agent-wechat");
  assert.match(privileged.ctx.SessionKey, /^agent:agent:wechat-call:/);
  assert.equal(privileged.ctx.OriginatingTo, undefined);
  assert.equal(privileged.ctx.CommandInterpretationSuppressed, true);
  assert.equal(privileged.ctx.SuppressMessageReceivedHooks, true);
  assert.equal(privileged.replyOptions.thinkingLevelOverride, undefined);
  assert.doesNotMatch(spoken.at(-1)!, /synthetic tool progress/);
  assert.match(spoken.at(-1)!, /synthetic-privileged-result/);
  const spokenBeforePending = spoken.length;
  await consult("pending-owner-result", "hold owner result");
  assert.equal((results.at(-1) as { status: string }).status, "working");
  assert.equal(spoken.length, spokenBeforePending);
  assert.equal(requests.at(-1)!.replyOptions.toolsAllow, undefined);
  live = settings([]);
  release();
  await delay(0);
  assert.equal(spoken.length, spokenBeforePending);
  assert.deepEqual(trace.slice(-2), [
    "[wechat-call-audio] plugin consult mode=owner stage=started",
    "[wechat-call-audio] plugin consult mode=owner stage=failed category=authority-changed",
  ]);
  live = undefined;
  await consult("unavailable-snapshot");
  assertReadOnly(ownerPeer);
  live = exact;
  await consult("cached-owner-result");
  assertReadOnly(ownerPeer);
  assert.notEqual(requests.at(-1)!.ctx.SessionKey, privileged.ctx.SessionKey);
  assert.match(spoken.at(-1)!, /synthetic-read-only-result/);
  assert.deepEqual(trace.slice(-2), [
    "[wechat-call-audio] plugin consult mode=safe-read-only stage=started",
    "[wechat-call-audio] plugin consult mode=safe-read-only stage=ok",
  ]);
  for (const [question, category] of [
    ["simulate registry failure", "inactive-registry"],
    ["simulate workspace failure", "filesystem"],
    ["simulate run failure", "agent-run"],
  ]) {
    await consult(category, question);
    assert.match(spoken.at(-1)!, /couldn't get an answer/);
    assert.equal(trace.at(-1), `[wechat-call-audio] plugin consult mode=safe-read-only stage=failed category=${category}`);
  }
  assert.ok(trace.every(line => /^\[wechat-call-audio\] plugin consult mode=(owner|safe-read-only) stage=(started|ok|failed category=(authority-changed|inactive-registry|filesystem|agent-run))$/.test(line)));
  live = settings([`agent-wechat:${ownerPeer}`], "none");
  const beforeDisabled = requests.length;
  await consult("live-opt-out");
  assert.equal(requests.length, beforeDisabled);
  await owner.close();

  live = exact;
  const normalCall = new AbortController();
  const normalOwner = await open(ownerPeer, () => ownerPeer, normalCall.signal);
  await consult("normal-owner-session");
  const normal = requests.at(-1)!;
  assert.equal(normal.replyOptions.toolsAllow, undefined);
  assert.match(spoken.at(-1)!, /synthetic-privileged-result/);
  await consult("deferred-owner-session", "simulate deferred run");
  assert.match(spoken.at(-1)!, /couldn't get an answer/);
  normalCall.abort();
  await normalOwner.close();
  assert.equal(normal.replyOptions.abortSignal.aborted, true);

  const normalNonOwner = await open(otherPeer);
  await consult("restricted-session");
  assertReadOnly(otherPeer);
  assert.match(spoken.at(-1)!, /synthetic-read-only-result/);
  await normalNonOwner.close();
});

test("per-contact calls share one session per contact, seed it once, and summarize into it without tools", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  let bridgeArgs: any;
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {},
      close() {}, submitToolResult() {}, sendUserMessage() {},
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
  };
  const sessions = new Set<string>(["agent:agent:conversation"]);
  const { dispatches, reply } = fakeReply(async ({ ctx, replyOptions }) => { replyOptions.onAgentRunStart(); sessions.add(ctx.SessionKey); return "Synthetic answer"; });
  const runtime = { channel: { reply }, agent: { session: { resolveStorePath: () => "/synthetic/store",
    getSessionEntry: ({ sessionKey }: { sessionKey: string }) => sessions.has(sessionKey) ? { sessionId: `${sessionKey}-id` } : undefined } } } as unknown as PluginRuntime;
  const hostCfg = { session: { mainKey: "conversation" } };
  const dependencies = { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } };
  const prepared = await prepareOpenClawCallVoice(config(true), hostCfg, runtime, dependencies);
  const heard: Array<[string, string]> = [];
  const context = { senderId: "Synthetic-Peer", signal: new AbortController().signal, parentSession: { key: "agent:agent:main" },
    recentContext: "Recent context with this caller: synthetic earlier call." };
  let consulted = 0;
  const consult = async () => { await bridgeArgs.onToolCall({ callId: `tool-${dispatches.length}`, name: "openclaw_agent_consult", args: { question: "Read synthetic data" } }); for (let i = 0; i < 100 && dispatches.length === consulted; i++) await delay(5); };

  const first = await prepared.open({ ...callbacks, transcript: (role, text) => { heard.push([role, text]); } }, context);
  assert.match(bridgeArgs.instructions, /synthetic earlier call/);
  assert.deepEqual(first.binding!(), { sessionKey: "agent:agent:wechat-call:synthetic-peer:read-only", owner: false });
  bridgeArgs.onTranscript("user", "Hello there", false); bridgeArgs.onTranscript("user", "Hello there", true);
  bridgeArgs.onTranscript("assistant", "Hi!", true); bridgeArgs.onTranscript("user", "   ", true);
  assert.deepEqual(heard, [["user", "Hello there"], ["assistant", "Hi!"]]);
  await consult(); consulted = dispatches.length;
  assert.equal(dispatches[0].ctx.SessionKey, "agent:agent:wechat-call:synthetic-peer:read-only");
  assert.equal(dispatches[0].ctx.ParentSessionKey, "agent:agent:conversation");
  await first.close();

  // The next call with this contact continues the same session; it is not seeded again.
  const second = await prepared.open(callbacks, context);
  await consult(); consulted = dispatches.length;
  assert.equal(dispatches[1].ctx.SessionKey, dispatches[0].ctx.SessionKey);
  assert.equal(dispatches[1].ctx.ParentSessionKey, undefined);
  await second.close();

  const summary = await prepared.summarize({ binding: first.binding!(), senderId: "Synthetic-Peer", direction: "incoming", durationText: "1 min 5 s",
    transcript: "[00:01] Caller: Hello there", signal: new AbortController().signal });
  assert.equal(summary, "Synthetic answer");
  const request = dispatches.at(-1)!;
  assert.equal(request.ctx.SessionKey, "agent:agent:wechat-call:synthetic-peer:read-only");
  assert.deepEqual(request.replyOptions.toolsAllow, []);
  assert.match(request.ctx.BodyForAgent, /This WeChat voice call has ended[\s\S]*not the account owner[\s\S]*Call transcript:\nThey called you; the call lasted 1 min 5 s\.\n\[00:01\] Caller: Hello there$/);

  // Summaries can be turned off; per-call scope gives each call its own session.
  const quiet = config(true); quiet.consultation.summary = false; quiet.sessionScope = "per-call";
  const isolated = await prepareOpenClawCallVoice(quiet, hostCfg, runtime, dependencies);
  const before = dispatches.length;
  assert.equal(await isolated.summarize({ binding: first.binding!(), senderId: "Synthetic-Peer", direction: "incoming", durationText: "1 s", transcript: "x", signal: new AbortController().signal }), undefined);
  assert.equal(dispatches.length, before);
  const [a, b] = [await isolated.open(callbacks, context), await isolated.open(callbacks, context)];
  assert.match(a.binding!().sessionKey, /^agent:agent:wechat-call:[0-9a-f-]{36}$/); assert.notEqual(a.binding!().sessionKey, b.binding!().sessionKey);
  await a.close(); await b.close();
});
