import { randomUUID } from "node:crypto";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import type { ResolvedWeChatCallsConfig } from "./calls-config.ts";

export type StreamingPcmResampler = { process(chunk: Buffer): Buffer; flush(): Buffer };
export type VoiceCallbacks = {
  audio(audio: Buffer, metadata?: { itemId: string }): void;
  clear(): void;
  failed(): void;
};
export type CallVoiceSession = { sendAudio(audio: Buffer): void; setMediaTimestamp(ms: number): void; close(): void | Promise<void> };
export type PreparedCallVoice = {
  sampleRate: number;
  resampler(inputRate: number, outputRate: number): StreamingPcmResampler;
  open(callbacks: VoiceCallbacks, context: { senderId: string; signal: AbortSignal }): Promise<CallVoiceSession>;
};
type HostSession = CallVoiceSession & { connect(): Promise<void>; submitToolResult(id: string, result: unknown): void | Promise<void>; bridge: { outputAudioMode?: string; isConnected(): boolean } };
type ProviderCapabilities = { inputAudioFormats?: Array<{ encoding: string; sampleRateHz: number; channels: number }>; outputAudioFormats?: Array<{ encoding: string; sampleRateHz: number; channels: number }>; supportsBargeIn?: boolean; supportsToolCalls?: boolean };
const PCM = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
const fits = (formats: ProviderCapabilities["inputAudioFormats"]) => formats?.some(format => format.encoding === PCM.encoding && format.sampleRateHz === PCM.sampleRateHz && format.channels === 1) === true;

/** 2026.10.7 publishes these subpaths as JavaScript without declarations. Check
 * its exported entry points at runtime; never import private hashed bundles. */
export async function prepareOpenClawCallVoice(config: ResolvedWeChatCallsConfig, cfg: unknown, runtime: PluginRuntime,
  dependencies?: { voiceSdk: Record<string, any>; audioSdk: Record<string, any> }): Promise<PreparedCallVoice> {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audioSdk] = dependencies ? [dependencies.voiceSdk, dependencies.audioSdk] : await Promise.all([import(voicePath), import(audioPath)]);
  for (const name of ["createRealtimeVoiceBridgeSession", "resolveConfiguredRealtimeVoiceProvider", "resolveRealtimeVoiceAgentConsultTools", "resolveRealtimeVoiceAgentConsultToolsAllow"]) {
    if (typeof sdk[name] !== "function") throw Error("Calling realtime SDK unavailable");
  }
  if (typeof audioSdk.createStreamingPcmResampler !== "function") throw Error("Calling streaming resampler unavailable");
  if (config.toolPolicy !== "none" && (typeof sdk.consultRealtimeVoiceAgent !== "function" || !runtime.agent?.runEmbeddedAgent || !config.allowConsultSessionPersistence)) throw Error("Calling consultation unavailable");
  const selected = sdk.resolveConfiguredRealtimeVoiceProvider({ cfg, configuredProviderId: config.provider, agentId: config.agentId, surface: "bridge",
    providerConfigOverrides: { ...(config.model ? { model: config.model } : {}), ...(config.voice ? { voice: config.voice } : {}) }, useProviderDefaultModel: true, autoRespondToAudio: true }) as { provider: unknown; providerConfig: unknown; capabilities?: ProviderCapabilities };
  const capabilities = selected.capabilities;
  // This pinned response bridge supplies item IDs for fencing interrupted output.
  // Continuous and other provider contracts need their own reviewed adapter.
  if ((selected.provider as { id?: string })?.id !== "openai") throw Error("Calling provider unsupported");
  if (!capabilities || !fits(capabilities.inputAudioFormats) || !fits(capabilities.outputAudioFormats) || capabilities.supportsBargeIn !== true
    || typeof (selected.provider as { createBridge?: unknown })?.createBridge !== "function"
    || (config.toolPolicy !== "none" && capabilities.supportsToolCalls !== true)) throw Error("Calling provider capabilities unavailable");
  return {
    sampleRate: PCM.sampleRateHz,
    resampler: (inputRate, outputRate) => audioSdk.createStreamingPcmResampler(inputRate, outputRate) as StreamingPcmResampler,
    async open(callbacks, context) {
      let session: HostSession | undefined;
      let closed = false;
      let ready = false;
      let resolveReady!: () => void;
      let rejectReady!: (error: Error) => void;
      const admission = new Promise<void>((resolve, reject) => { resolveReady = resolve; rejectReady = reject; });
      // Observe immediately: an early provider callback may reject before connect.
      void admission.catch(() => {});
      const consults = new Map<string, Promise<unknown>>();
      const signal = context.signal;
      const fail = () => { if (!closed) { rejectReady(Error("Calling provider failed")); callbacks.failed(); } };
      const abort = () => { closed = true; rejectReady(Error("Calling provider stopped")); void Promise.resolve(session?.close()).catch(() => {}); };
      signal.throwIfAborted(); signal.addEventListener("abort", abort, { once: true });
      const sessionKey = `agent:${config.agentId}:wechat-call:${randomUUID()}`;
      let consulting = false;
      const consult = async (question: string, requestSignal?: AbortSignal): Promise<{ text: string }> => {
        if (closed || signal.aborted || !ready || config.toolPolicy === "none" || consulting || question.length > 4000 || !question.trim()) throw Error("Calling consultation not admitted");
        consulting = true;
        try {
          return await sdk.consultRealtimeVoiceAgent({ cfg, agentRuntime: runtime.agent, agentId: config.agentId, sessionKey,
            contextMode: "isolated", senderId: context.senderId, senderIsOwner: false, messageProvider: "agent-wechat",
            toolsAllow: sdk.resolveRealtimeVoiceAgentConsultToolsAllow(config.toolPolicy), transcript: [], args: { question },
            surface: "a WeChat voice call", userLabel: "Caller", assistantLabel: "Assistant", runIdPrefix: "wechat-call", lane: "wechat-call",
            timeoutMs: 30000, abortSignal: requestSignal ? AbortSignal.any([signal, requestSignal]) : signal,
            logger: { info() {}, warn() {}, error() {} } }) as { text: string };
        } finally { consulting = false; }
      };
      try {
        session = sdk.createRealtimeVoiceBridgeSession({ cfg, agentId: config.agentId, provider: selected.provider, providerConfig: selected.providerConfig,
          capabilities, audioFormat: PCM, autoRespondToAudio: true, interruptResponseOnInputAudio: true,
          instructions: "Speak concisely with the caller. Calling permission does not grant account-owner authority. Do not claim to perform actions unless a permitted tool confirms them.",
          tools: sdk.resolveRealtimeVoiceAgentConsultTools(config.toolPolicy), triggerGreetingOnReady: false, markStrategy: "ignore",
          audioSink: { isOpen: () => ready && !closed && !signal.aborted, sendAudio: callbacks.audio, clearAudio: callbacks.clear },
          onReady: () => { if (!closed && !signal.aborted) { ready = true; resolveReady(); } }, onClose: fail, onError: fail,
          ...(config.toolPolicy !== "none" ? { runAgentConsult: (request: { prompt: string; signal?: AbortSignal }) => consult(request.prompt, request.signal) } : {}),
          onToolCall: async (event: { name: string; callId: string; args: unknown }) => {
            if (closed || signal.aborted || !session) return;
            if (typeof event.callId !== "string" || !event.callId || event.callId.length > 200) throw Error("Invalid call consultation identity");
            if (event.name !== sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME || config.toolPolicy === "none") {
              await session.submitToolResult(event.callId, { error: "Tool is not permitted for this call" }); return;
            }
            if (consults.size >= 32 && !consults.has(event.callId)) throw Error("Calling consultation limit reached");
            let result = consults.get(event.callId);
            if (!result) {
              const question = event.args && typeof event.args === "object" ? (event.args as { question?: unknown }).question : undefined;
              result = (typeof question === "string" ? consult(question) : Promise.reject(Error("Question required")))
                .then(value => ({ text: value.text.slice(0, 8000) }), () => ({ error: "Call consultation failed" }));
              consults.set(event.callId, result);
            }
            const answer = await result;
            if (!closed && !signal.aborted) await session.submitToolResult(event.callId, answer);
          },
        }) as HostSession;
        if (!session || typeof session.connect !== "function" || typeof session.sendAudio !== "function" || typeof session.close !== "function"
          || typeof session.setMediaTimestamp !== "function" || session.bridge?.outputAudioMode === "continuous") throw Error("Calling bridge contract unavailable");
        signal.throwIfAborted(); if (closed) throw Error("Calling provider stopped");
        await session.connect(); await admission; signal.throwIfAborted();
        if (!session.bridge.isConnected()) throw Error("Calling provider not connected");
        return { sendAudio: audio => { if (!closed && !signal.aborted) session!.sendAudio(audio); }, setMediaTimestamp: ms => { if (!closed && !signal.aborted) session!.setMediaTimestamp(ms); },
          close: async () => { if (closed) return; closed = true; ready = false; signal.removeEventListener("abort", abort); consults.clear(); await session!.close(); } };
      } catch {
        closed = true; signal.removeEventListener("abort", abort); void Promise.resolve(session?.close()).catch(() => {}); throw Error("Calling provider unavailable");
      }
    },
  };
}
