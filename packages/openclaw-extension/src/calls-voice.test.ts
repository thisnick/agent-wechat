import test from "node:test";
import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
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

test("provider Workers receive cloneable audio format while owned config and callbacks retain identity", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audioSdk] = await Promise.all([import(voicePath), import(audioPath)]);
  let wrappedFormat: unknown, wrappedConfig: unknown, wrappedAudio: unknown;
  let request: any, input = 0, output = 0, closed = 0;
  const provider = { id: "openai", createBridge(value: any) {
    request = value;
    assert.notEqual(value.audioFormat, wrappedFormat);
    assert.equal(value.cfg, wrappedConfig, "Do not unwrap or copy the host's configuration view");
    assert.equal(value.onAudio, wrappedAudio, "Keep the SDK's owned callback");
    let connected = false;
    return { outputAudioMode: "continuous", connect: async () => {
      assert.deepEqual(structuredClone({ audioFormat: value.audioFormat }), { audioFormat: pcm });
      connected = true; value.onReady();
    }, isConnected: () => connected, sendAudio() { input++; }, sendUserMessage() {}, setMediaTimestamp() {},
    acknowledgeMark() {}, submitToolResult() {}, close() { closed++; connected = false; } };
  } };
  const prepared = await prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, {
    voiceSdk: { ...sdk,
      resolveConfiguredRealtimeVoiceProvider: () => ({ provider, providerConfig: {}, capabilities: {
        inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: false, handlesInputAudioBargeIn: true, handlesAgentConsult: true,
      } }),
      createRealtimeVoiceBridgeSession(params: any) {
        const adapted = params.provider;
        return sdk.createRealtimeVoiceBridgeSession({ ...params, provider: { ...adapted, createBridge(value: any) {
          wrappedFormat = new Proxy(value.audioFormat, {});
          wrappedConfig = new Proxy(value.cfg, {});
          wrappedAudio = value.onAudio;
          assert.throws(() => structuredClone(wrappedFormat), { name: "DataCloneError" });
          return adapted.createBridge({ ...value, cfg: wrappedConfig, audioFormat: wrappedFormat });
        } } });
      },
    }, audioSdk,
  });
  const session = await prepared.open({ audio() { output++; }, clear() {}, failed() { assert.fail("Unexpected provider failure"); } },
    { senderId: "synthetic-peer", signal: new AbortController().signal });
  session.sendAudio(Buffer.alloc(960)); request.onAudio(Buffer.alloc(960));
  assert.equal(input, 1); assert.equal(output, 1);
  await session.close(); assert.equal(closed, 1);
});

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
    sendUserMessage: (text: string) => spoken.push(text),
    sendAudio: (value: Buffer) => input.push(value), setMediaTimestamp() {}, handleBargeIn(options: unknown) {
      assert.deepEqual(options, { force: true, audioPlaybackActive: true });
      assert.deepEqual(request.getPlaybackState(), [{ itemId: "item", audioEndMs: 240 }]);
      request.onClearAudio("barge-in");
    }, acknowledgeMark() {}, submitToolResult() {},
    close: () => { closed++; connected = false; },
  }; } };
  const spoken: string[] = [];
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
  const session = await prepared.open({ audio: (_value, metadata) => { heard++; assert.equal(metadata?.itemId, "item"); }, clear: reason => { clears++; assert.equal(reason, "output-budget"); }, playbackState: () => [{ itemId: "item", audioEndMs: 240 }], failed() { assert.fail("Unexpected provider failure"); } }, { senderId: "synthetic-peer", signal: new AbortController().signal });
  assert.equal(providerRequest.instructions, bridgeParams.instructions);
  assert.equal(providerRequest.agentId, "agent");
  assert.match(bridgeParams.instructions, /Synthetic selected/);
  assert.match(bridgeParams.instructions, /Dry and precise/);
  assert.match(bridgeParams.instructions, /Use short sentences/);
  assert.match(bridgeParams.instructions, /does not grant account-owner authority/);
  assert.match(bridgeParams.instructions, /conversation-only/);
  assert.match(bridgeParams.instructions, /unless a tool or consultation result in this call supplied it/);
  assert.doesNotMatch(bridgeParams.instructions, /Synthetic default|openclaw_agent_consult|synthetic-openai-secret|small talk|Consult behavior/);
  session.sendAudio(Buffer.alloc(960)); assert.equal(input.length, 1);
  providerRequest.onAudio(Buffer.alloc(960), { itemId: "item" }); session.interruptOutput(); assert.equal(heard, 1); assert.equal(clears, 1);
  assert.deepEqual(providerRequest.tools, []); assert.equal(providerRequest.runAgentConsult, undefined); assert.equal(typeof bridgeParams.onTranscript, "function");
  providerRequest.onTranscript("user", "Synthetic discarded transcript", true);
  await session.close(); providerRequest.onAudio(Buffer.alloc(960), { itemId: "late" }); assert.equal(heard, 1); assert.equal(closed, 1);
  const generated = Buffer.alloc(3200); for (let i = 0; i < 1600; i++) generated.writeInt16LE(Math.round(4000 * Math.sin(i * 2 * Math.PI * 440 / 16000)), i * 2);
  const whole = prepared.resampler(16000, 24000); const expected = Buffer.concat([whole.process(generated), whole.flush()]);
  const stream = prepared.resampler(16000, 24000); const pieces: Buffer[] = [];
  for (let i = 0; i < generated.length; i += 83) pieces.push(stream.process(generated.subarray(i, i + 83)));
  pieces.push(stream.flush()); assert.deepEqual(Buffer.concat(pieces), expected); assert.equal(expected.length, 4800);
});

test("a delayed consultation acknowledges quietly through the public SDK and delivers one result without blocking input", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audioSdk] = await Promise.all([import(voicePath), import(audioPath)]);
  let bridgeArgs: any, connected = false, inputs = 0, automaticReplies = 0;
  const spoken: string[] = [], acknowledgments: Array<{ id: string; result: any; options: any }> = [];
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const { dispatches, reply } = fakeReply(async ({ replyOptions }) => {
    replyOptions.onAgentRunStart(); await held; return "Verified lookup result";
  });
  const provider = { id: "openai", createBridge(request: any) { return {
    outputAudioMode: "response", supportsToolResultSuppression: true,
    connect: async () => { connected = true; request.onReady(); }, isConnected: () => connected,
    sendUserMessage: (text: string) => spoken.push(text), sendAudio: () => { inputs++; },
    setMediaTimestamp() {}, handleBargeIn() {}, acknowledgeMark() {},
    submitToolResult(id: string, result: unknown, options: unknown) {
      acknowledgments.push({ id, result, options });
      if (!(options as any)?.suppressResponse) automaticReplies++;
    },
    close: () => { connected = false; },
  }; } };
  const runtime = { channel: { reply } } as unknown as PluginRuntime;
  const prepared = await prepareOpenClawCallVoice(config(true), {}, runtime, {
    voiceSdk: { ...sdk, createRealtimeVoiceBridgeSession(params: any) { bridgeArgs = params; return sdk.createRealtimeVoiceBridgeSession(params); },
      resolveConfiguredRealtimeVoiceProvider: () => ({ provider, providerConfig: {}, capabilities: {
        inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true,
      } }) }, audioSdk,
  });
  const session = await prepared.open(callbacks, { senderId: "peer", signal: new AbortController().signal });
  try {
    assert.match(bridgeArgs.instructions, /acknowledge it once/);
    assert.match(bridgeArgs.instructions, /stay quiet unless the caller asks for its status/);
    // auto: the host adds nothing, so the call scopes what the voice model answers itself.
    assert.match(bridgeArgs.instructions, /Answer greetings, small talk and general-knowledge questions yourself/);
    assert.match(bridgeArgs.instructions, /Use the consultation tool when a request needs current information/);
    assert.doesNotMatch(bridgeArgs.instructions, /only when verified|Consult behavior/);
    const event = { callId: "lookup", name: "openclaw_agent_consult", args: { question: "Check a fact" } };
    await bridgeArgs.onToolCall(event);
    for (let i = 0; i < 100 && !dispatches.length; i++) await delay(5);
    assert.equal(dispatches.length, 1);
    assert.equal(acknowledgments.length, 1);
    assert.equal(acknowledgments[0].result.status, "working");
    assert.deepEqual(acknowledgments[0].options, { suppressResponse: true });
    assert.equal(automaticReplies, 0); assert.equal(spoken.length, 0);
    session.sendAudio(Buffer.alloc(960)); session.sendAudio(Buffer.alloc(960));
    bridgeArgs.onTranscript("user", "Are you still checking?", true);
    await bridgeArgs.onToolCall(event);
    assert.equal(inputs, 2); assert.equal(dispatches.length, 1);
    assert.equal(acknowledgments.length, 1); assert.equal(spoken.length, 0);
    release();
    for (let i = 0; i < 100 && !spoken.length; i++) await delay(5);
    assert.equal(spoken.length, 1); assert.match(spoken[0], /Verified lookup result/);
    assert.doesNotMatch(spoken[0], /synthetic tool progress/);
    assert.equal(automaticReplies, 0);
  } finally { release(); await session.close(); }
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
    sendUserMessage() {}, sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close: () => { connected = false; },
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
  assert.match(request.instructions, /Answer greetings, small talk and general-knowledge questions yourself/);
  assert.match(request.instructions, /Delegate to the OpenClaw agent when a request needs current information/);
  assert.doesNotMatch(request.instructions, /openclaw_agent_consult|consultation tool|Consult behavior/);
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

  // substantive reaches a delegating model, worded without the consult tool it does not have.
  const substantive = config(true); substantive.realtime.consultPolicy = "substantive";
  const strict = await (await prepareOpenClawCallVoice(substantive, {}, runtime, dependencies)).open({ ...callbacks, failed }, { senderId: "synthetic-peer", signal });
  assert.match(request.instructions, /Consult behavior:/);
  assert.match(request.instructions, /Delegate to the OpenClaw agent before answering requests that need facts/);
  assert.doesNotMatch(request.instructions, /openclaw_agent_consult|small talk/);
  await strict.close();

  const always = config(true); always.realtime.consultPolicy = "always";
  await assert.rejects(prepareOpenClawCallVoice(always, {}, runtime, dependencies), /capabilities unavailable/);
  outputAudioMode = "response";
  await assert.rejects(prepared.open({ ...callbacks, failed }, { senderId: "synthetic-peer", signal }), /provider unavailable/);
});

test("fast context is off by default and validates its settings", () => {
  const voice = { enabled: true, agentId: "agent", realtime: { provider: "openai" } };
  assert.deepEqual(resolveWeChatCallsConfig({ voice })!.voice!.realtime.fastContext,
    { enabled: false, timeoutMs: 800, maxResults: 3, sources: ["memory", "sessions"], fallbackToConsult: false });
  const custom = resolveWeChatCallsConfig({ voice: { ...voice, realtime: { provider: "openai",
    fastContext: { enabled: true, timeoutMs: 1500, maxResults: 5, sources: ["memory"], fallbackToConsult: true } } } })!.voice!;
  assert.deepEqual(custom.realtime.fastContext, { enabled: true, timeoutMs: 1500, maxResults: 5, sources: ["memory"], fallbackToConsult: true });
  for (const fastContext of [{ timeoutMs: 50 }, { timeoutMs: 1.5 }, { maxResults: 0 }, { maxResults: 11 }, { sources: [] }, { sources: ["files"] },
    { sources: ["memory", "memory"] }, { enabled: "yes" }, { fallbackToConsult: 1 }, { unknown: true }, "on"]) {
    assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, realtime: { provider: "openai", fastContext: fastContext as any } } }), /Invalid calling configuration/);
  }
});

test("fast context answers only the owner's direct calls, is interpreted rather than read out, and the next consultation runs the agent", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  const ownerPeer = "synthetic-owner";
  const fastContext = { enabled: true, timeoutMs: 800, maxResults: 3, sources: ["memory", "sessions"], fallbackToConsult: false };
  const cfg = { commands: { ownerAllowFrom: [`agent-wechat:${ownerPeer}`] }, channels: { "agent-wechat": {
    native: { mode: "external", accountId: "native-account", endpoint: "http://127.0.0.1:1", token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" },
    calls: { voice: { enabled: true, agentId: "agent", toolPolicy: "owner", allowConsultSessionPersistence: true, realtime: { provider: "openai", fastContext } } },
  } } };
  const voice = resolveWeChatCallsConfig(cfg.channels["agent-wechat"].calls as any)!.voice!;
  assert.deepEqual(voice.realtime.fastContext, fastContext);
  const { dispatches, reply } = fakeReply(async ({ replyOptions }) => { replyOptions.onAgentRunStart(); return "Synthetic agent result"; });
  const runtime = { config: { current: () => cfg }, channel: { reply }, agent: { session: { resolveStorePath: () => "/synthetic/store", getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  const lookups: any[] = []; let fastResult: unknown;
  const tool = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true };
  const live = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: false, handlesInputAudioBargeIn: true, supportsToolCalls: false, handlesAgentConsult: true };
  const start = async (capabilities: typeof tool | typeof live, senderId: string) => {
    let bridgeArgs: any; const spoken: string[] = []; const traces: string[] = [];
    const sdk = { ...host,
      resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities }),
      resolveRealtimeVoiceFastContextConsult: async (params: any) => { lookups.push(params); params.liveness.assertCurrent(); return fastResult; },
      createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
        bridge: { isConnected: () => true, outputAudioMode: capabilities === live ? "continuous" : "response" }, connect: async () => args.onReady(),
        sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close() {}, submitToolResult() {},
        sendUserMessage(text: string) { spoken.push(text); },
      }; },
    };
    const prepared = await prepareOpenClawCallVoice(voice, cfg, runtime, { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } });
    const session = await prepared.open(callbacks, { senderId, connectedPeer: () => senderId, nativeAccountId: "native-account", channelAccountId: "default",
      logAudioTrace: line => traces.push(line), signal: new AbortController().signal });
    let toolCalls = 0;
    const ask = async (question: string): Promise<string> => {
      if (capabilities === live) return (await bridgeArgs.runAgentConsult({ prompt: question, signal: new AbortController().signal })).text;
      const before = spoken.length;
      await bridgeArgs.onToolCall({ name: host.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: `lookup-${++toolCalls}`, args: { question } });
      for (let i = 0; i < 100 && spoken.length === before; i++) await delay(5);
      return spoken.at(-1)!;
    };
    return { session, ask, traces };
  };

  // A non-owner caller never gets snippets: the agent and its read-only rules answer.
  fastResult = { handled: true, result: { text: "Synthetic private memory snippet" } };
  const guest = await start(tool, "synthetic-guest");
  assert.match(await guest.ask("What is the owner's address?"), /Speak this exact OpenClaw answer[\s\S]*Synthetic agent result/);
  assert.equal(lookups.length, 0); assert.equal(dispatches.length, 1);
  assert.match(dispatches[0].ctx.BodyForAgent, /non-owner WeChat caller/);
  await guest.session.close();

  // The owner's snippets go to the voice model to interpret, with the way back to the agent.
  const owner = await start(tool, ownerPeer);
  const first = await owner.ask("What did we decide?");
  assert.match(first, /^Internal OpenClaw memory lookup result\./);
  assert.match(first, /Do not read it aloud as written/);
  assert.match(first, /Synthetic private memory snippet/);
  assert.match(first, /call the consultation tool again now with the same request/);
  assert.doesNotMatch(first, /Speak this exact/);
  assert.equal(lookups.length, 1); assert.equal(dispatches.length, 1, "a fast-context answer does not run the agent");
  assert.deepEqual(lookups[0].args, { question: "What did we decide?" });
  assert.equal(lookups[0].agentId, "agent");
  assert.deepEqual(lookups[0].config, fastContext);
  // Asking again (here, an action the snippets cannot perform) runs the agent with owner tools.
  assert.match(await owner.ask("Hang up the call"), /Speak this exact OpenClaw answer[\s\S]*Synthetic agent result/);
  assert.equal(lookups.length, 1, "the consultation after a fast-context answer skips the lookup");
  assert.equal(dispatches.length, 2); assert.equal(dispatches[1].replyOptions.toolsAllow, undefined);
  assert.equal(lookups[0].sessionKey, dispatches[1].ctx.SessionKey, "searches as the owner's own call session");
  // The bypass is used once; a later question searches again, and a miss with no fallback is interpreted too.
  fastResult = { handled: true, result: { text: "No relevant OpenClaw memory was found quickly for the live caller." } };
  assert.match(await owner.ask("Anything else on file?"), /^Internal OpenClaw memory lookup result\.[\s\S]*No relevant OpenClaw memory/);
  assert.equal(lookups.length, 2); assert.equal(dispatches.length, 2);
  // The miss above also counts as a fast-context answer, so the next consultation runs the agent.
  fastResult = { handled: false };
  assert.match(await owner.ask("Read synthetic data"), /Synthetic agent result/);
  assert.equal(lookups.length, 2); assert.equal(dispatches.length, 3);
  // With fallbackToConsult, the SDK reports a miss as unhandled and the agent runs.
  assert.match(await owner.ask("Check synthetic status"), /Synthetic agent result/);
  assert.equal(lookups.length, 3); assert.equal(dispatches.length, 4);
  assert.deepEqual(owner.traces.map(line => line.match(/stage=(fast-context-\w+)/)?.[1]).filter(Boolean),
    ["fast-context-answered", "fast-context-bypassed", "fast-context-answered", "fast-context-bypassed", "fast-context-fallback"]);
  assert.equal(owner.traces.some(line => /synthetic|decide|snippet/i.test(line)), false, "traces carry no question or snippet text");
  await owner.session.close();

  // GPT-Live gets plain text to put in its own words, worded as delegation.
  fastResult = { handled: true, result: { text: "Synthetic private memory snippet" } };
  const liveOwner = await start(live, ownerPeer);
  const delegated = await liveOwner.ask("What did we decide?");
  assert.match(delegated, /^Synthetic private memory snippet/);
  assert.match(delegated, /delegate the same request again now/);
  assert.equal(await liveOwner.ask("Hang up the call"), "Synthetic agent result");
  assert.equal(lookups.length, 4); assert.equal(dispatches.length, 5);
  await liveOwner.session.close();
});

test("shared host voice profile supplies identity, user grounding and persona without agent instructions", async t => {
  const dir = await mkdtemp(join(tmpdir(), "wechat-call-voice-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const workspace = join(dir, "workspace");
  await mkdir(workspace);
  const identity = join(workspace, "IDENTITY.md");
  await Promise.all([
    writeFile(identity, "- Name: Synthetic Main Voice"),
    writeFile(join(workspace, "SOUL.md"), "Speak with quiet curiosity.\n" + "x".repeat(8000)),
    writeFile(join(workspace, "USER.md"), "The configured user is Synthetic Owner. Address them as Synthetic Nick."),
    writeFile(join(workspace, "AGENTS.md"), "private-workspace-sentinel"),
    writeFile(join(workspace, "MEMORY.md"), "private-memory-sentinel"),
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
        sendUserMessage() {}, sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close() {} };
    },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {},
      capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }),
    resolveRealtimeVoiceAgentConsultTools: () => [],
    resolveRealtimeVoiceAgentConsultToolsAllow: () => [],
  };
  // Legacy main resolves the host default workspace without an agents.list entry.
  const cfg = { plugins: { slots: { memory: "none" } }, agents: { defaults: { workspace } } };
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
  assert.match(profile, /### IDENTITY\.md\n- Name: Synthetic Main Voice/);
  assert.match(profile, /### SOUL\.md\nSpeak with quiet curiosity/);
  assert.match(profile, /### USER\.md\nThe configured user is Synthetic Owner/);
  assert.match(profile, /many sessions at once/);
  assert.match(profile, /truncat/i);
  assert.ok(profile.length < 8500);
  assert.doesNotMatch(profile, /AGENTS\.md|MEMORY\.md|private-/);
  await writeFile(join(workspace, "USER.md"), "Updated user grounding");
  assert.match(await prompt(prepared), /Updated user grounding/);
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
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {},
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
  const consulted = config(true); consulted.consultation = { timeoutMs: 15000, summary: true, recentMessages: 10 };
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
  assert.doesNotMatch(bridgeArgs.instructions, /small talk/);
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
      sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close() {},
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
  let bridgeArgs: any; let spoken = 0;
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {},
      close() {}, submitToolResult() {}, sendUserMessage() { spoken++; },
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
  const heard: Array<[string, string, boolean, boolean]> = [];
  const context = { senderId: "Synthetic-Peer", signal: new AbortController().signal, parentSession: { key: "agent:agent:main" },
    recentContext: "Recent context with this caller: synthetic earlier call.", consultContext: "Latest messages of your WeChat chat: synthetic chat." };
  // Each consultation finishes (its answer is spoken) before the next starts.
  const consult = async () => { const said = spoken; await bridgeArgs.onToolCall({ callId: `tool-${dispatches.length}`, name: "openclaw_agent_consult", args: { question: "Read synthetic data" } }); for (let i = 0; i < 100 && spoken === said; i++) await delay(5); };

  const first = await prepared.open({ ...callbacks, transcript: (role, text, final, snapshot) => { heard.push([role, text, final, snapshot]); } }, context);
  assert.match(bridgeArgs.instructions, /synthetic earlier call/);
  assert.deepEqual(first.binding!(), { sessionKey: "agent:agent:wechat-call:synthetic-peer:read-only", owner: false });
  bridgeArgs.onTranscript("user", "Hello", false); bridgeArgs.onTranscript("user", "Hello there", false, { textMode: "snapshot" });
  bridgeArgs.onTranscript("user", "Hello there", true); bridgeArgs.onTranscript("system", "ignored", true);
  assert.deepEqual(heard, [["user", "Hello", false, false], ["user", "Hello there", false, true], ["user", "Hello there", true, false]]);
  await consult();
  assert.equal(dispatches[0].ctx.SessionKey, "agent:agent:wechat-call:synthetic-peer:read-only");
  assert.equal(dispatches[0].ctx.ParentSessionKey, "agent:agent:conversation");
  // The chat context goes with the call's first consultation only.
  assert.match(dispatches[0].ctx.BodyForAgent, /read-only consultation[\s\S]*synthetic chat\.\n\nCaller question:/);
  await consult();
  assert.doesNotMatch(dispatches[1].ctx.BodyForAgent, /synthetic chat/);
  await first.close();

  // The next call with this contact continues the same session; it is not seeded again.
  const second = await prepared.open(callbacks, context);
  await consult();
  assert.equal(dispatches[2].ctx.SessionKey, dispatches[0].ctx.SessionKey);
  assert.equal(dispatches[2].ctx.ParentSessionKey, undefined);
  assert.match(dispatches[2].ctx.BodyForAgent, /synthetic chat/, "each call gets the chat context again");
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
