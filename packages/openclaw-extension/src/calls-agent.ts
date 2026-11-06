import { randomUUID } from "node:crypto";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import type { ResolvedWeChatVoiceCallsConfig } from "./calls-config.ts";
import { resolveCallTranscriptionApiKey } from "./calls-secrets.ts";

type TranscriptionSession = { connect(): Promise<void>; sendAudio(audio: Buffer): void; close(): void; isConnected(): boolean };
type BridgeRequest = {
  onReady(): void; onClose(reason: string): void; onError(error: Error): void;
  onAudio(audio: Buffer, metadata: { itemId: string }): void; onClearAudio(): void;
  onTranscript?(role: string, text: string, final: boolean, metadata?: { textMode: "snapshot" }): void;
  getPlaybackState?(): Array<{ itemId: string; audioEndMs: number }>;
};
export type CallAgentRequest = { prompt: string; signal: AbortSignal; contextOnly: boolean; context?: string; interruption?: string };
type CallAgentDependencies = { voiceSdk: Record<string, any>; transcriptionSdk?: Record<string, any>; meetingSdk?: Record<string, any> };
const PCM = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
const COALESCE_MS = 200;
const MAX_TURN_CHARS = 16_000;

// The public TTS API has a timeout, but no AbortSignal. Stop waiting on a
// cancelled turn so it cannot delay the next one; still observe its late result.
function waitForSpeech<T>(pending: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise((resolve, reject) => {
    const abort = () => { signal.removeEventListener("abort", abort); reject(signal.reason); };
    pending.then(value => { signal.removeEventListener("abort", abort); resolve(value); }, error => { signal.removeEventListener("abort", abort); reject(error); });
    if (signal.aborted) abort(); else signal.addEventListener("abort", abort, { once: true });
  });
}

/** Adapts the public transcription registry and host TTS to the public voice
 * bridge contract. Call ownership, tool authority and session routing stay in
 * the caller's delegate; this adapter owns audio/turn cancellation only. */
export async function prepareCallAgentProvider(config: ResolvedWeChatVoiceCallsConfig, cfg: unknown, runtime: PluginRuntime, dependencies: CallAgentDependencies) {
  const transcriptionPath: string = "openclaw/plugin-sdk/realtime-transcription";
  const meetingPath: string = "openclaw/plugin-sdk/meeting-runtime";
  const transcriptionSdk = dependencies.transcriptionSdk ?? await import(transcriptionPath);
  const meetingSdk = dependencies.meetingSdk ?? await import(meetingPath);
  const { convertPcmToMulaw8k, createSpeechThresholdGate, readPcm16AudioStats } = dependencies.voiceSdk;
  if (typeof transcriptionSdk.getRealtimeTranscriptionProvider !== "function" || typeof meetingSdk.convertMeetingTtsAudioForBridge !== "function"
    || typeof convertPcmToMulaw8k !== "function" || typeof createSpeechThresholdGate !== "function" || typeof readPcm16AudioStats !== "function"
    || typeof runtime.tts?.textToSpeechTelephony !== "function") throw Error("Calling agent speech SDK unavailable");
  const provider = transcriptionSdk.getRealtimeTranscriptionProvider(config.transcription.provider, cfg);
  if (!provider || typeof provider.createSession !== "function") throw Error("Calling transcription provider unavailable");
  const apiKey = await resolveCallTranscriptionApiKey(config, cfg);
  const rawConfig = { ...(config.transcription.model ? { model: config.transcription.model, modelId: config.transcription.model } : {}),
    ...(config.transcription.language ? { language: config.transcription.language } : {}), ...(apiKey ? { apiKey } : {}) };
  const providerConfig = provider.resolveConfig ? await provider.resolveConfig({ cfg, rawConfig }) : rawConfig;
  if (!(provider.isConfiguredAsync ? await provider.isConfiguredAsync({ cfg, providerConfig }) : provider.isConfigured?.({ cfg, providerConfig }))) throw Error("Calling transcription provider is not configured");
  return {
    capabilities: { inputAudioFormats: [PCM], outputAudioFormats: [PCM], supportsBargeIn: true, handlesAgentConsult: true },
    provider(delegate: (request: CallAgentRequest) => Promise<{ text: string; isCurrent(): boolean }>, inBackground: <T>(run: () => T) => T) {
      return { id: "openclaw-agent", createBridge(request: BridgeRequest) {
        let closed = false, connected = false, generation = 0, active: AbortController | undefined;
        let task: Promise<void> | undefined, pendingSpeech = "", latestContext = "", interruption: string | undefined;
        const pendingContext: string[] = [];
        let coalesce: ReturnType<typeof setTimeout> | undefined, outputTimer: ReturnType<typeof setTimeout> | undefined;
        let output: { pcm: Buffer; offset: number; itemId: string; generation: number; isCurrent(): boolean } | undefined;
        let utteranceOpen = false, closing = false;
        let stt: TranscriptionSession | undefined;
        let lastOutputItem: string | undefined;
        const gate = createSpeechThresholdGate({ rmsThreshold: 650, peakThreshold: 2500, speechFrames: 4, silenceFrames: 10, cooldownMs: 300 });
        const live = (epoch: number) => !closed && connected && generation === epoch;
        const interrupt = () => {
          if (closed || utteranceOpen) return;
          utteranceOpen = true;
          const playing = request.getPlaybackState?.().find(item => item.itemId === (output?.itemId ?? lastOutputItem));
          if (output || playing) {
            const played = playing?.audioEndMs ?? 0;
            interruption = `The previous spoken reply was interrupted. Approximately ${played} ms was played; do not assume its full text in session history was heard.`;
          }
          generation++; active?.abort(); clearTimeout(outputTimer); outputTimer = undefined; output = undefined;
          request.onClearAudio();
        };
        const scheduleOutput = () => {
          if (closed || !output || outputTimer) return;
          outputTimer = setTimeout(() => {
            outputTimer = undefined;
            const current = output;
            if (!current || !live(current.generation)) return;
            try {
              if (!current.isCurrent()) {
                generation++; active?.abort(); output = undefined; request.onClearAudio(); void flush(); return;
              }
              // Playback accounting is supplied by our native paced sink. Feed
              // at most 200 ms ahead, so long TTS responses do not overflow it.
              const played = request.getPlaybackState?.().find(item => item.itemId === current.itemId)?.audioEndMs;
              const sentMs = current.offset / 48;
              if (played === undefined || sentMs - played < 200) {
                const end = Math.min(current.pcm.length, current.offset + 1920); // 40 ms PCM16/24 kHz.
                request.onAudio(current.pcm.subarray(current.offset, end), { itemId: current.itemId }); current.offset = end;
              }
              if (current.offset === current.pcm.length) { output = undefined; void flush(); }
              else scheduleOutput();
            } catch { fail(); }
          }, 20);
          outputTimer.unref?.();
        };
        const fail = () => { if (!closed) { request.onError(Error("Calling agent speech failed")); close(); request.onClose("transport-error"); } };
        const flush = async () => {
          if (closed || !connected || task || output || coalesce) return;
          const contextOnly = !pendingSpeech;
          const prompt = pendingSpeech || pendingContext.shift();
          if (!prompt) return;
          pendingSpeech = "";
          const epoch = generation; const controller = new AbortController(); active = controller;
          task = inBackground(async () => {
            try {
              const note = interruption; interruption = undefined;
              const result = await delegate({ prompt, signal: controller.signal, contextOnly, context: latestContext || undefined, interruption: note });
              if (!live(epoch) || controller.signal.aborted || !result.isCurrent() || !result.text.trim()) return;
              const speech = await waitForSpeech(runtime.tts.textToSpeechTelephony({ text: result.text, cfg: cfg as any, timeoutMs: config.consultation.timeoutMs }), controller.signal);
              if (!live(epoch) || controller.signal.aborted || !result.isCurrent()) return;
              if (!speech.success || !speech.audioBuffer || !speech.sampleRate) throw Error("Calling TTS unavailable");
              const pcm = meetingSdk.convertMeetingTtsAudioForBridge(speech.audioBuffer, speech.sampleRate, "pcm16-24khz", speech.outputFormat, "WeChat");
              if (!Buffer.isBuffer(pcm) || !pcm.length || pcm.length % 2 || pcm.length > 24_000 * 2 * 180) throw Error("Calling TTS audio budget exceeded");
              request.onTranscript?.("assistant", result.text, true, { textMode: "snapshot" });
              lastOutputItem = `agent-${randomUUID()}`;
              output = { pcm, offset: 0, itemId: lastOutputItem, generation: epoch, isCurrent: result.isCurrent }; scheduleOutput();
            } catch {
              // Cancellation or failed consultation never retries an action.
              // An unsuccessful turn leaves input open for the next request.
              if (live(epoch) && !controller.signal.aborted) request.onError(Error("Calling agent turn unavailable"));
            }
          }).finally(() => { task = undefined; if (active === controller) active = undefined; void flush(); });
        };
        const close = () => { if (closed) return; closed = true; connected = false; generation++; active?.abort(); clearTimeout(coalesce); clearTimeout(outputTimer); pendingSpeech = ""; pendingContext.length = 0; output = undefined;
          closing = true; try { stt?.close(); } catch { /* Teardown remains fenced. */ } finally { closing = false; } };
        stt = provider.createSession({ cfg, providerConfig,
          onSpeechStart: interrupt,
          onPartial: (text: string) => { if (!closed && text.trim()) interrupt(); },
          onTranscript: (text: string) => {
            if ((closed && !closing) || !text.trim()) return;
            if (closing) { request.onTranscript?.("user", text, true, { textMode: "snapshot" }); return; }
            interrupt(); utteranceOpen = false;
            request.onTranscript?.("user", text, true, { textMode: "snapshot" });
            pendingSpeech = `${pendingSpeech}${pendingSpeech ? "\n" : ""}${text}`.slice(0, MAX_TURN_CHARS);
            clearTimeout(coalesce); coalesce = setTimeout(() => { coalesce = undefined; void flush(); }, COALESCE_MS); coalesce.unref?.();
          },
          onError: () => fail(),
        });
        if (closed) stt?.close();
        return { outputAudioMode: "response", pacesInputAudio: false,
          connect: async () => { await stt!.connect(); if (closed) { stt!.close(); return; } if (!stt!.isConnected()) throw Error("Calling transcription not connected"); connected = true; request.onReady(); },
          isConnected: () => !closed && connected && stt!.isConnected(),
          sendAudio: (audio: Buffer) => { if (!closed && connected) { gate.accept(readPcm16AudioStats(audio), { onTrigger: () => { interrupt(); return true; } }); stt!.sendAudio(convertPcmToMulaw8k(audio, 24000)); } },
          setMediaTimestamp() {}, acknowledgeMark() {}, submitToolResult() { throw Error("Agent voice bridge does not expose provider tools"); },
          sendUserMessage: (text: string) => { if (!closed && text.trim()) { latestContext = text.slice(0, MAX_TURN_CHARS); if (pendingContext.length >= 4) pendingContext.shift(); pendingContext.push(latestContext); void flush(); } },
          triggerGreeting: (text = "Briefly greet the caller.") => { if (!closed) { if (pendingContext.length >= 4) pendingContext.shift(); pendingContext.push(text.slice(0, MAX_TURN_CHARS)); void flush(); } },
          close,
        };
      } };
    },
  };
}
