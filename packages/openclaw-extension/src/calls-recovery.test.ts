import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { prepareOpenClawCallVoice } from "./calls-voice.ts";
import { resolveWeChatCallsConfig } from "./calls-config.ts";

async function until(predicate: () => boolean) { for (let i = 0; i < 250; i++) { if (predicate()) return; await delay(5); } assert.fail("Condition did not become true"); }

test("recoverable provider errors preserve audio; continuity reset cancels old consultations and fences their results", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice", audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audioSdk] = await Promise.all([import(voicePath), import(audioPath)]);
  let request: any; let connected = false, failures = 0, clears = 0, input = 0;
  let hold: Promise<void> | undefined; let release!: () => void;
  const dispatches: any[] = [], spoken: string[] = [];
  const runtime = { channel: { reply: { finalizeInboundContext: (ctx: unknown) => ctx,
    dispatchReplyWithBufferedBlockDispatcher: async (params: any) => {
      dispatches.push(params); params.replyOptions.onAgentRunStart(); await hold;
      await params.dispatcherOptions.deliver({ text: "Synthetic agent result" }, { kind: "final" }); params.replyOptions.onAgentRunTerminalOutcome("completed");
    },
  } }, agent: { session: { resolveStorePath: () => "/synthetic/store", getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  const provider = { id: "openai", createBridge(params: any) {
    request = params; return { outputAudioMode: "response", connect: async () => { connected = true; params.onReady(); }, isConnected: () => connected,
      sendAudio: () => { input++; }, setMediaTimestamp() {}, acknowledgeMark() {}, submitToolResult() {}, sendUserMessage: (text: string) => spoken.push(text), close: () => { connected = false; } };
  } };
  const pcm = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
  const config = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "main", realtime: { provider: "openai" }, toolPolicy: "safe-read-only", allowConsultSessionPersistence: true } })!.voice!;
  const prepared = await prepareOpenClawCallVoice(config, {}, runtime, { voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider, providerConfig: {}, capabilities: {
    inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true,
  } }) }, audioSdk });
  const session = await prepared.open({ audio() {}, clear() { clears++; }, failed() { failures++; } }, { senderId: "peer", signal: new AbortController().signal });
  request.onError(Error("Recoverable diagnostic")); session.sendAudio(Buffer.alloc(960));
  assert.equal(input, 1); assert.equal(failures, 0);
  hold = new Promise(resolve => { release = resolve; });
  request.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: "old", args: { question: "Old request" } });
  await until(() => dispatches.length === 1);
  request.onEvent({ direction: "client", type: "session.continuity.reset" });
  assert.equal(dispatches[0].replyOptions.abortSignal.aborted, true); assert.equal(clears, 1);
  request.onReady(); release(); hold = undefined; await delay(40);
  assert.equal(spoken.length, 0, "old results and error replies cannot enter the replacement provider session");
  request.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: "new", args: { question: "Fresh request" } });
  await until(() => spoken.length === 1); assert.equal(dispatches.length, 2);
  request.onClose("error"); assert.equal(failures, 1);
  await session.close();
});
