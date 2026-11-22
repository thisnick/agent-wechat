import type {
  CallAudioFormat,
  CallTrack,
  CallVideoCapabilities,
  CallVideoTrack,
  NativeCallMediaClient,
  NativeCallVideoFrame,
} from "@agent-wechat/shared";
import {
  EMPTY_CALL_DEVICES,
  type CallAudioLevels,
  type CallDevice,
  type CallDeviceSelection,
  type CallDevicesSnapshot,
  type CallMediaAttachOptions,
  type CallMediaBackend,
  type CallMediaHandle,
  type LocalPreviewSurface,
} from "./media-backend.ts";

const WORKLET_PROCESSOR_NAME = "agentwechat-call-audio";

/**
 * Self-contained AudioWorkletProcessor handling both:
 * 1) Capture: hardware sampleRate -> target PCM16LE mono frames (default 16 kHz / 20 ms = 320 samples)
 * 2) Playout: per-track 16 kHz PCM16LE jitter queues -> mixed & resampled to hardware sampleRate
 */
const AUDIO_WORKLET_SOURCE = `
class AgentWechatCallAudioProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const opts = (options && options.processorOptions) || {};
    this.targetRate = opts.targetRate || 16000;
    this.frameSamples = opts.frameSamples || 320;
    this.muted = !!opts.muted;
    this.speakerMuted = !!opts.speakerMuted;

    // Capture resampler state (hardware sampleRate -> targetRate)
    this.captureStep = sampleRate / this.targetRate;
    this.capturePhase = 0;
    this.capturePrev = 0;
    this.captureOut = new Int16Array(this.frameSamples);
    this.captureOutLen = 0;
    this.captureSumSq = 0;
    this.captureCount = 0;

    // Playout per-track jitter queues (targetRate -> hardware sampleRate)
    this.playStep = this.targetRate / sampleRate;
    this.tracks = new Map();
    this.speakerSumSq = 0;
    this.speakerCount = 0;
    this.meterCounter = 0;

    this.port.onmessage = (event) => {
      const msg = event.data;
      if (!msg || typeof msg !== "object") return;
      if (msg.type === "mute") {
        this.muted = !!msg.muted;
      } else if (msg.type === "speakerMuted") {
        this.speakerMuted = !!msg.muted;
      } else if (msg.type === "clearTrack") {
        this.tracks.delete(msg.trackId);
      } else if (msg.type === "clearAll") {
        this.tracks.clear();
      } else if (msg.type === "pcm" && msg.buffer) {
        const samples = new Int16Array(msg.buffer);
        let track = this.tracks.get(msg.trackId);
        if (!track) {
          track = {
            chunks: [],
            totalSamples: 0,
            pos: 0,
            primed: false,
            lastSample: 0,
            sumSq: 0,
            count: 0,
          };
          this.tracks.set(msg.trackId, track);
        }
        // Bound latency to ~240 ms (12 x 20ms frames at 16 kHz = 3840 samples).
        const maxBuffered = this.frameSamples * 12;
        while (track.totalSamples + samples.length > maxBuffered && track.chunks.length > 0) {
          const dropped = track.chunks.shift();
          track.totalSamples -= dropped.length;
          track.pos = 0;
        }
        track.chunks.push(samples);
        track.totalSamples += samples.length;
        // Pre-buffer 3 frames (60 ms) after silence/start to absorb jitter.
        if (!track.primed && track.totalSamples >= this.frameSamples * 3) {
          track.primed = true;
        }
      }
    };
  }

  sampleTrack(track) {
    if (!track.primed || track.chunks.length === 0) return 0;
    const head = track.chunks[0];
    const idx = Math.floor(track.pos);
    const frac = track.pos - idx;
    const s0 = idx < head.length ? head[idx] / 32768 : track.lastSample;
    let s1 = s0;
    if (idx + 1 < head.length) {
      s1 = head[idx + 1] / 32768;
    } else if (track.chunks.length > 1 && track.chunks[1].length > 0) {
      s1 = track.chunks[1][0] / 32768;
    }
    const val = s0 + (s1 - s0) * frac;
    track.lastSample = s0;
    track.pos += this.playStep;
    while (track.chunks.length > 0 && track.pos >= track.chunks[0].length) {
      const done = track.chunks.shift();
      track.pos -= done.length;
      track.totalSamples -= done.length;
    }
    if (track.chunks.length === 0) {
      track.pos = 0;
      track.primed = false;
    }
    track.sumSq += val * val;
    track.count += 1;
    return val;
  }

  process(inputs, outputs) {
    const input = inputs[0] && inputs[0][0];
    if (input && input.length > 0) {
      const n = input.length;
      while (this.capturePhase < n) {
        const idx = Math.floor(this.capturePhase);
        const frac = this.capturePhase - idx;
        const prev = idx === 0 ? this.capturePrev : input[idx - 1];
        const curr = input[idx];
        const sample = this.muted ? 0 : (prev + (curr - prev) * frac);
        const clamped = Math.max(-1, Math.min(1, sample));
        this.captureOut[this.captureOutLen++] = clamped < 0 ? Math.round(clamped * 32768) : Math.round(clamped * 32767);
        this.captureSumSq += clamped * clamped;
        this.captureCount += 1;
        if (this.captureOutLen >= this.frameSamples) {
          const out = this.captureOut;
          this.captureOut = new Int16Array(this.frameSamples);
          this.captureOutLen = 0;
          this.port.postMessage({ type: "capture", buffer: out.buffer }, [out.buffer]);
        }
        this.capturePhase += this.captureStep;
      }
      this.capturePhase -= n;
      this.capturePrev = input[n - 1];
    }

    const outChannels = outputs[0];
    if (outChannels && outChannels.length > 0) {
      const frames = outChannels[0].length;
      for (let i = 0; i < frames; i++) {
        let sum = 0;
        for (const track of this.tracks.values()) {
          sum += this.sampleTrack(track);
        }
        // C1 continuous soft-clip knee at +/-0.9 to avoid step distortion on multi-speaker peaks
        const abs = Math.abs(sum);
        const mixed = abs > 0.9 ? Math.sign(sum) * (0.9 + 0.1 * Math.tanh((abs - 0.9) / 0.1)) : sum;
        const value = this.speakerMuted ? 0 : mixed;
        this.speakerSumSq += value * value;
        this.speakerCount += 1;
        for (let ch = 0; ch < outChannels.length; ch++) {
          outChannels[ch][i] = value;
        }
      }
    }

    this.meterCounter += 1;
    if (this.meterCounter >= 30) {
      this.meterCounter = 0;
      const micRms = this.captureCount > 0 ? Math.sqrt(this.captureSumSq / this.captureCount) : 0;
      const spkRms = this.speakerCount > 0 ? Math.sqrt(this.speakerSumSq / this.speakerCount) : 0;
      const trackLevels = {};
      for (const [id, track] of this.tracks.entries()) {
        if (track.count > 0) {
          trackLevels[id] = Math.min(1, Math.sqrt(track.sumSq / track.count) * 2.5);
          track.sumSq = 0;
          track.count = 0;
        }
      }
      this.captureSumSq = 0;
      this.captureCount = 0;
      this.speakerSumSq = 0;
      this.speakerCount = 0;
      this.port.postMessage({
        type: "levels",
        micLevel: this.muted ? 0 : Math.min(1, micRms * 2.8),
        speakerLevel: this.speakerMuted ? 0 : Math.min(1, spkRms * 2.5),
        trackLevels,
      });
    }
    return true;
  }
}
registerProcessor("${WORKLET_PROCESSOR_NAME}", AgentWechatCallAudioProcessor);
`;

type AudioContextWithSink = AudioContext & {
  setSinkId?: (sinkId: string) => Promise<void>;
};

async function probeWebCodecsH264(): Promise<{ encode: boolean; decode: boolean }> {
  let encode = false;
  let decode = false;
  if (typeof VideoEncoder === "function" && typeof VideoEncoder.isConfigSupported === "function") {
    try {
      const support = await VideoEncoder.isConfigSupported({
        codec: "avc1.42001e",
        width: 640,
        height: 480,
        bitrate: 600_000,
        framerate: 15,
        avc: { format: "annexb" },
      } as VideoEncoderConfig);
      encode = support.supported === true;
    } catch {
      encode = false;
    }
  }
  if (typeof VideoDecoder === "function" && typeof VideoDecoder.isConfigSupported === "function") {
    try {
      const support = await VideoDecoder.isConfigSupported({
        codec: "avc1.42001e",
      });
      decode = support.supported === true;
    } catch {
      decode = false;
    }
  }
  return { encode, decode };
}

function stopStream(stream: MediaStream | null | undefined): void {
  if (!stream) return;
  for (const track of stream.getTracks()) {
    try {
      track.stop();
    } catch {}
  }
}

export class BrowserCallMediaBackend implements CallMediaBackend {
  readonly kind = "browser" as const;
  private h264Support: { encode: boolean; decode: boolean } = { encode: false, decode: false };
  private probePromise: Promise<void> | null = null;

  constructor() {
    this.probePromise = probeWebCodecsH264().then((result) => {
      this.h264Support = result;
    });
  }

  videoCapabilities(): CallVideoCapabilities {
    const sendCodecs: CallVideoCapabilities["sendCodecs"] = this.h264Support.encode
      ? ["h264", "jpeg"]
      : ["jpeg"];
    const receiveCodecs: CallVideoCapabilities["receiveCodecs"] = this.h264Support.decode
      ? ["h264", "jpeg"]
      : ["jpeg"];
    return {
      sendCodecs,
      receiveCodecs,
      maxWidth: 640,
      maxHeight: 480,
      maxFps: 15,
    };
  }

  async ensureProbed(): Promise<void> {
    if (this.probePromise) await this.probePromise;
  }

  async listDevices(): Promise<CallDevicesSnapshot> {
    if (typeof navigator === "undefined" || !navigator.mediaDevices?.enumerateDevices) {
      return { ...EMPTY_CALL_DEVICES };
    }
    try {
      const raw = await navigator.mediaDevices.enumerateDevices();
      const mapList = (kind: CallDevice["kind"]): CallDevice[] =>
        raw
          .filter((item) => item.kind === kind && item.deviceId !== "")
          .map((item, idx) => ({
            id: item.deviceId,
            label:
              item.label ||
              (kind === "audioinput"
                ? `Microphone ${idx + 1}`
                : kind === "audiooutput"
                  ? `Speaker ${idx + 1}`
                  : `Camera ${idx + 1}`),
            kind,
            isDefault: item.deviceId === "default" || idx === 0,
          }));
      const hasLabels = raw.some((item) => Boolean(item.label));
      const supportsOutputSelection =
        typeof AudioContext !== "undefined" &&
        typeof (AudioContext.prototype as unknown as AudioContextWithSink).setSinkId === "function";
      return {
        audioInputs: mapList("audioinput"),
        audioOutputs: mapList("audiooutput"),
        videoInputs: mapList("videoinput"),
        permissionState: hasLabels ? "granted" : "unknown",
        supportsOutputSelection,
      };
    } catch {
      return { ...EMPTY_CALL_DEVICES, permissionState: "denied" };
    }
  }

  async startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface> {
    if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
      throw new Error("camera_unavailable");
    }
    const stream = await navigator.mediaDevices.getUserMedia({
      video: cameraId
        ? { deviceId: { exact: cameraId }, width: { ideal: 640 }, height: { ideal: 480 }, frameRate: { ideal: 15 } }
        : { width: { ideal: 640 }, height: { ideal: 480 }, frameRate: { ideal: 15 } },
      audio: false,
    });
    return {
      stream,
      stop: () => stopStream(stream),
    };
  }

  async attach(options: CallMediaAttachOptions): Promise<CallMediaHandle> {
    await this.ensureProbed();
    const session = new BrowserCallMediaSession(options);
    await session.start();
    return session;
  }
}

class BrowserCallMediaSession implements CallMediaHandle {
  private readonly options: CallMediaAttachOptions;
  private readonly media: NativeCallMediaClient;
  private readonly audioFormat: CallAudioFormat;
  private stopped = false;
  private muted: boolean;
  private speakerMuted: boolean;
  private cameraEnabled: boolean;
  private devices: CallDeviceSelection;

  // Audio state
  private audioCtx: AudioContextWithSink | null = null;
  private workletNode: AudioWorkletNode | null = null;
  private micStream: MediaStream | null = null;
  private micSource: MediaStreamAudioSourceNode | null = null;
  private audioSendGeneration = 1;

  // Local video capture + encode state
  private cameraStream: MediaStream | null = null;
  private videoElement: HTMLVideoElement | null = null;
  private captureTimer: ReturnType<typeof setTimeout> | null = null;
  private videoSendTrack: CallVideoTrack | null = null;
  private videoEncoder: VideoEncoder | null = null;
  private encoderDims: { w: number; h: number } | null = null;
  private encoderRotated = false;
  private cameraGen = 0;
  private forceNextKeyframe = true;
  private captureBusy = false;

  // Remote tracks & decoders (audio and video receive IDs tracked separately)
  private readonly audioReceiveTrackIds = new Set<number>();
  private readonly videoReceiveTrackParticipants = new Map<number, string | undefined>();
  private readonly decoders = new Map<number, { decoder: VideoDecoder; awaitingKeyframe: boolean; codec: string }>();
  private readonly lastKeyframeRequestAt = new Map<number, number>();
  private readonly decodingJpegTracks = new Set<number>();

  constructor(options: CallMediaAttachOptions) {
    this.options = options;
    this.media = options.media;
    this.audioFormat = options.audioFormat;
    this.muted = options.initialMuted;
    this.speakerMuted = options.speakerMuted;
    this.cameraEnabled = options.callType === "video" && options.initialCameraEnabled;
    this.devices = { ...options.devices };
  }

  async start(): Promise<void> {
    try {
      const at = this.media.tracks();
      this.audioSendGeneration = at.sendTrack.generation;
      for (const track of at.receiveTracks) {
        this.audioReceiveTrackIds.add(track.id);
      }
      const vt = this.media.videoTracks();
      this.videoSendTrack = vt.sendTrack;
      for (const track of vt.receiveTracks) {
        this.videoReceiveTrackParticipants.set(track.id, track.participantId);
      }
    } catch {
      // Media tracks may update shortly after attach.
    }

    await this.startAudioEngine();
    this.startAudioReceiveLoop();
    this.startVideoReceiveLoop();
    this.startKeyframeRequestLoop();

    if (this.cameraEnabled) {
      await this.startCameraCapture();
    }
  }

  private async startAudioEngine(): Promise<void> {
    const AudioCtx =
      typeof globalThis !== "undefined"
        ? (globalThis.AudioContext ??
          (globalThis as unknown as { webkitAudioContext?: typeof AudioContext })
            .webkitAudioContext)
        : undefined;
    if (!AudioCtx) return;

    const ctx = new AudioCtx() as AudioContextWithSink;
    this.audioCtx = ctx;
    if (this.devices.outputDeviceId && typeof ctx.setSinkId === "function") {
      try {
        await ctx.setSinkId(this.devices.outputDeviceId);
      } catch {
        // Output device selection is optional; keep OS default on rejection.
      }
    }

    const frameSamples = Math.round((this.audioFormat.sampleRate * this.audioFormat.frameMs) / 1000);
    if (ctx.audioWorklet && typeof Blob !== "undefined" && typeof URL.createObjectURL === "function") {
      const blob = new Blob([AUDIO_WORKLET_SOURCE], { type: "application/javascript" });
      const url = URL.createObjectURL(blob);
      try {
        await ctx.audioWorklet.addModule(url);
      } finally {
        URL.revokeObjectURL(url);
      }
      if (this.stopped) return;
      const node = new AudioWorkletNode(ctx, WORKLET_PROCESSOR_NAME, {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        outputChannelCount: [1],
        processorOptions: {
          targetRate: this.audioFormat.sampleRate,
          frameSamples,
          muted: this.muted,
          speakerMuted: this.speakerMuted,
        },
      });
      node.port.onmessage = (event) => {
        if (this.stopped) return;
        const msg = event.data;
        if (!msg || typeof msg !== "object") return;
        if (msg.type === "capture" && msg.buffer instanceof ArrayBuffer) {
          try {
            this.media.sendPcm(new Int16Array(msg.buffer), this.audioSendGeneration);
          } catch {
            // Backpressure or ending state; ignore dropped frames.
          }
        } else if (msg.type === "levels") {
          const levels: CallAudioLevels = {
            micLevel: typeof msg.micLevel === "number" ? msg.micLevel : 0,
            speakerLevel: typeof msg.speakerLevel === "number" ? msg.speakerLevel : 0,
            trackLevels:
              msg.trackLevels && typeof msg.trackLevels === "object" ? msg.trackLevels : {},
          };
          this.options.onLevels(levels);
        }
      };
      node.connect(ctx.destination);
      this.workletNode = node;
    }

    if (ctx.state === "suspended") {
      void ctx.resume().catch(() => {});
    }
    await this.openMicrophone();
  }

  private async openMicrophone(): Promise<void> {
    if (this.stopped || !this.audioCtx || !this.workletNode) return;
    if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
      this.options.onError("microphone_unavailable");
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: {
          ...(this.devices.inputDeviceId ? { deviceId: { exact: this.devices.inputDeviceId } } : {}),
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
          channelCount: 1,
        },
        video: false,
      });
      if (this.stopped) {
        stopStream(stream);
        return;
      }
      this.micSource?.disconnect();
      stopStream(this.micStream);
      this.micStream = stream;
      this.micSource = this.audioCtx.createMediaStreamSource(stream);
      this.micSource.connect(this.workletNode);
    } catch {
      this.options.onError("microphone_unavailable");
    }
  }

  private startAudioReceiveLoop(): void {
    void (async () => {
      try {
        for await (const frame of this.media.frames()) {
          if (this.stopped) break;
          if (this.workletNode) {
            const copy = new Uint8Array(frame.payload.byteLength);
            copy.set(frame.payload);
            this.workletNode.port.postMessage(
              { type: "pcm", trackId: frame.trackId, buffer: copy.buffer },
              [copy.buffer],
            );
          }
        }
      } catch {
        // Media stream closed; controller receives call.state/disconnect.
      }
    })();
  }

  private startKeyframeRequestLoop(): void {
    void (async () => {
      try {
        for await (const _req of this.media.keyframeRequests()) {
          if (this.stopped) break;
          this.forceNextKeyframe = true;
        }
      } catch {
        // Media stream closed.
      }
    })();
  }

  private startVideoReceiveLoop(): void {
    void (async () => {
      try {
        for await (const frame of this.media.videoFrames()) {
          if (this.stopped) break;
          this.handleIncomingVideoFrame(frame);
        }
      } catch {
        // Media stream closed.
      }
    })();
  }

  private handleIncomingVideoFrame(frame: NativeCallVideoFrame): void {
    if (!this.videoReceiveTrackParticipants.has(frame.trackId)) {
      this.videoReceiveTrackParticipants.set(frame.trackId, undefined);
    }
    const participantId = this.videoReceiveTrackParticipants.get(frame.trackId);
    if (frame.codec === "jpeg") {
      if (typeof createImageBitmap === "function" && typeof Blob !== "undefined") {
        // Drop intermediate frames if a previous JPEG decode on this track is still in flight.
        if (this.decodingJpegTracks.has(frame.trackId)) return;
        this.decodingJpegTracks.add(frame.trackId);
        const bytes = new Uint8Array(frame.payload.byteLength);
        bytes.set(frame.payload);
        const blob = new Blob([bytes], { type: "image/jpeg" });
        void createImageBitmap(blob).then(
          (bitmap) => {
            this.decodingJpegTracks.delete(frame.trackId);
            if (this.stopped || !this.videoReceiveTrackParticipants.has(frame.trackId)) {
              bitmap.close();
              return;
            }
            this.options.onRemoteFrame({
              trackId: frame.trackId,
              participantId: this.videoReceiveTrackParticipants.get(frame.trackId),
              rotationDegrees: frame.rotationDegrees,
              bitmap,
              width: bitmap.width,
              height: bitmap.height,
            });
          },
          () => {
            this.decodingJpegTracks.delete(frame.trackId);
            if (!this.stopped && this.videoReceiveTrackParticipants.has(frame.trackId)) {
              this.options.onRemoteFrame({
                trackId: frame.trackId,
                participantId: this.videoReceiveTrackParticipants.get(frame.trackId),
                rotationDegrees: frame.rotationDegrees,
                jpegBytes: bytes,
              });
            }
          },
        );
      } else {
        this.options.onRemoteFrame({
          trackId: frame.trackId,
          participantId,
          rotationDegrees: frame.rotationDegrees,
          jpegBytes: frame.payload,
        });
      }
      return;
    }

    // H.264 / H.265 Annex-B via WebCodecs VideoDecoder
    if (typeof VideoDecoder !== "function" || typeof EncodedVideoChunk !== "function") {
      return;
    }
    const codecString = frame.codec === "h264" ? "avc1.42001e" : "hvc1.1.6.L93.B0";
    let entry = this.decoders.get(frame.trackId);
    if (!entry || entry.codec !== codecString || entry.decoder.state === "closed") {
      if (entry) {
        try {
          entry.decoder.close();
        } catch {}
      }
      const trackId = frame.trackId;
      const rotationRef = { value: frame.rotationDegrees };
      const decoder = new VideoDecoder({
        output: (videoFrame) => {
          if (this.stopped || !this.videoReceiveTrackParticipants.has(trackId)) {
            videoFrame.close();
            return;
          }
          void createImageBitmap(videoFrame)
            .then((bitmap) => {
              videoFrame.close();
              if (this.stopped || !this.videoReceiveTrackParticipants.has(trackId)) {
                bitmap.close();
                return;
              }
              this.options.onRemoteFrame({
                trackId,
                participantId: this.videoReceiveTrackParticipants.get(trackId),
                rotationDegrees: rotationRef.value,
                bitmap,
                width: bitmap.width,
                height: bitmap.height,
              });
            })
            .catch(() => videoFrame.close());
        },
        error: () => {
          const current = this.decoders.get(trackId);
          if (current) current.awaitingKeyframe = true;
          this.requestKeyframeRateLimited(trackId);
        },
      });
      try {
        decoder.configure({ codec: codecString, optimizeForLatency: true });
      } catch {
        return;
      }
      entry = { decoder, awaitingKeyframe: true, codec: codecString };
      (entry as { rotationRef?: { value: NativeCallVideoFrame["rotationDegrees"] } }).rotationRef =
        rotationRef;
      this.decoders.set(frame.trackId, entry);
    }
    const rotationHolder = (
      entry as { rotationRef?: { value: NativeCallVideoFrame["rotationDegrees"] } }
    ).rotationRef;
    if (rotationHolder) rotationHolder.value = frame.rotationDegrees;

    if (entry.awaitingKeyframe && !frame.keyframe) {
      this.requestKeyframeRateLimited(frame.trackId);
      return;
    }
    if (frame.keyframe) entry.awaitingKeyframe = false;
    try {
      const chunk = new EncodedVideoChunk({
        type: frame.keyframe ? "key" : "delta",
        timestamp: frame.timestampMicros,
        data: frame.payload,
      });
      entry.decoder.decode(chunk);
    } catch {
      entry.awaitingKeyframe = true;
      this.requestKeyframeRateLimited(frame.trackId);
    }
  }

  private requestKeyframeRateLimited(trackId: number): void {
    const now = Date.now();
    const last = this.lastKeyframeRequestAt.get(trackId) ?? 0;
    if (now - last < 2000) return;
    this.lastKeyframeRequestAt.set(trackId, now);
    try {
      this.media.requestKeyframe(trackId);
    } catch {}
  }

  private ensureH264Encoder(
    width: number,
    height: number,
    fps: number,
    bitrateKbps: number,
    rotated90Ccw: boolean,
  ): void {
    if (typeof VideoEncoder !== "function" || typeof VideoFrame !== "function") return;
    if (
      this.videoEncoder &&
      this.videoEncoder.state === "configured" &&
      this.encoderDims?.w === width &&
      this.encoderDims?.h === height &&
      this.encoderRotated === rotated90Ccw
    ) {
      return;
    }
    if (this.videoEncoder) {
      try {
        this.videoEncoder.close();
      } catch {}
      this.videoEncoder = null;
      this.encoderDims = null;
      this.encoderRotated = false;
    }
    try {
      const encoder = new VideoEncoder({
        output: (chunk) => {
          if (this.stopped || !this.cameraEnabled || !this.videoSendTrack) return;
          const bytes = new Uint8Array(chunk.byteLength);
          chunk.copyTo(bytes);
          try {
            this.media.sendVideo(bytes, this.videoSendTrack.generation, {
              codec: "h264",
              keyframe: chunk.type === "key",
              timestampMicros: chunk.timestamp,
              rotationDegrees: rotated90Ccw ? 90 : 0,
            });
          } catch {}
        },
        error: () => {
          try {
            this.videoEncoder?.close();
          } catch {}
          this.videoEncoder = null;
          this.encoderDims = null;
          this.encoderRotated = false;
        },
      });
      encoder.configure({
        codec: "avc1.42001e",
        width,
        height,
        bitrate: bitrateKbps * 1000,
        framerate: fps,
        avc: { format: "annexb" },
      } as VideoEncoderConfig);
      this.videoEncoder = encoder;
      this.encoderDims = { w: width, h: height };
      this.encoderRotated = rotated90Ccw;
      this.forceNextKeyframe = true;
    } catch {
      this.videoEncoder = null;
      this.encoderDims = null;
      this.encoderRotated = false;
    }
  }

  private async startCameraCapture(): Promise<void> {
    if (this.stopped) return;
    if (typeof navigator === "undefined" || !navigator.mediaDevices?.getUserMedia) {
      this.options.onError("camera_unavailable");
      return;
    }
    this.stopCameraCapture();
    const gen = ++this.cameraGen;
    const format = this.videoSendTrack?.format;
    const targetW = Math.min(format?.maxWidth ?? 640, 640);
    const targetH = Math.min(format?.maxHeight ?? 480, 480);
    const targetFps = Math.max(5, Math.min(format?.maxFps ?? 15, 15));

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        video: {
          ...(this.devices.cameraDeviceId ? { deviceId: { exact: this.devices.cameraDeviceId } } : {}),
          width: { ideal: targetW },
          height: { ideal: targetH },
          frameRate: { ideal: targetFps },
        },
        audio: false,
      });
    } catch {
      if (gen === this.cameraGen && !this.stopped) {
        this.options.onError("camera_unavailable");
      }
      return;
    }
    if (this.stopped || !this.cameraEnabled || gen !== this.cameraGen) {
      stopStream(stream);
      return;
    }
    this.cameraStream = stream;
    this.options.onLocalStream(stream);

    if (typeof document === "undefined") return;
    const video = document.createElement("video");
    video.muted = true;
    video.playsInline = true;
    video.srcObject = stream;
    this.videoElement = video;
    try {
      await video.play();
    } catch {
      // Headless or paused tab; wait for frames when ready.
    }
    if (this.stopped || !this.cameraEnabled || gen !== this.cameraGen) {
      if (gen !== this.cameraGen) {
        video.pause();
        video.srcObject = null;
        stopStream(stream);
      } else {
        this.stopCameraCapture();
      }
      return;
    }

    const canvas = document.createElement("canvas");
    const ctx2d = canvas.getContext("2d");
    const intervalMs = Math.round(1000 / targetFps);
    this.forceNextKeyframe = true;

    const tick = () => {
      if (this.stopped || !this.cameraEnabled || gen !== this.cameraGen || !this.videoElement) return;
      const activeTrack = this.videoSendTrack;
      if (
        activeTrack &&
        this.videoElement.readyState >= 2 &&
        this.videoElement.videoWidth > 0 &&
        !this.captureBusy
      ) {
        const vw = this.videoElement.videoWidth;
        const vh = this.videoElement.videoHeight;
        const maxW = Math.min(activeTrack.format.maxWidth, 640);
        const maxH = Math.min(activeTrack.format.maxHeight, 480);
        const scale = Math.min(1, maxW / vw, maxH / vh);
        const w = Math.max(16, Math.round((vw * scale) / 2) * 2);
        const h = Math.max(16, Math.round((vh * scale) / 2) * 2);
        if (activeTrack.format.codec === "h264" && typeof VideoEncoder === "function" && typeof VideoFrame === "function") {
          // Pre-rotate H.264 frames 90° CCW to match WeChat mobile/conference wire orientation
          // (+90° CW display rotation), enabling zero-transcode pass-through on H.264 wire calls.
          const rotated = ctx2d !== null;
          const encW = rotated ? h : w;
          const encH = rotated ? w : h;
          this.ensureH264Encoder(
            encW,
            encH,
            Math.max(5, Math.min(activeTrack.format.maxFps, 15)),
            activeTrack.format.bitrateKbps ?? 600,
            rotated,
          );
          if (this.videoEncoder && this.videoEncoder.state === "configured" && this.videoEncoder.encodeQueueSize <= 2) {
            let vf: VideoFrame | null = null;
            try {
              if (ctx2d) {
                if (canvas.width !== encW || canvas.height !== encH) {
                  canvas.width = encW;
                  canvas.height = encH;
                }
                ctx2d.save();
                ctx2d.setTransform(1, 0, 0, 1, 0, 0);
                ctx2d.translate(0, encH);
                ctx2d.rotate(-Math.PI / 2);
                ctx2d.drawImage(this.videoElement, 0, 0, w, h);
                ctx2d.restore();
                vf = new VideoFrame(canvas, {
                  timestamp: Math.round(performance.now() * 1000),
                });
              } else {
                vf = new VideoFrame(this.videoElement, {
                  timestamp: Math.round(performance.now() * 1000),
                });
              }
              const keyFrame = this.forceNextKeyframe;
              this.forceNextKeyframe = false;
              this.videoEncoder.encode(vf, { keyFrame });
            } catch {} finally {
              vf?.close();
            }
          }
        } else if (ctx2d && activeTrack.format.codec === "jpeg") {
          if (canvas.width !== w || canvas.height !== h) {
            canvas.width = w;
            canvas.height = h;
          }
          ctx2d.setTransform(1, 0, 0, 1, 0, 0);
          ctx2d.drawImage(this.videoElement, 0, 0, w, h);
          this.captureBusy = true;
          const sendGeneration = activeTrack.generation;
          canvas.toBlob(
            (blob) => {
              this.captureBusy = false;
              if (!blob || this.stopped || !this.cameraEnabled || gen !== this.cameraGen) return;
              void blob.arrayBuffer().then((buf) => {
                if (this.stopped || !this.cameraEnabled || gen !== this.cameraGen) return;
                try {
                  this.media.sendVideo(new Uint8Array(buf), sendGeneration, {
                    codec: "jpeg",
                    keyframe: true,
                    timestampMicros: Math.round(performance.now() * 1000),
                  });
                } catch {}
              });
            },
            "image/jpeg",
            0.72,
          );
        }
      }
      this.captureTimer = setTimeout(tick, intervalMs);
    };
    this.captureTimer = setTimeout(tick, 50);
  }

  private stopCameraCapture(): void {
    this.cameraGen += 1;
    if (this.captureTimer) {
      clearTimeout(this.captureTimer);
      this.captureTimer = null;
    }
    this.captureBusy = false;
    if (this.videoEncoder) {
      try {
        this.videoEncoder.close();
      } catch {}
      this.videoEncoder = null;
      this.encoderDims = null;
      this.encoderRotated = false;
    }
    if (this.videoElement) {
      this.videoElement.pause();
      this.videoElement.srcObject = null;
      this.videoElement = null;
    }
    if (this.cameraStream) {
      stopStream(this.cameraStream);
      this.cameraStream = null;
      this.options.onLocalStream(null);
    }
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    this.workletNode?.port.postMessage({ type: "mute", muted });
  }

  setSpeakerMuted(muted: boolean): void {
    this.speakerMuted = muted;
    this.workletNode?.port.postMessage({ type: "speakerMuted", muted });
  }

  async setCameraEnabled(enabled: boolean): Promise<void> {
    this.cameraEnabled = enabled;
    if (enabled) {
      await this.startCameraCapture();
    } else {
      this.stopCameraCapture();
    }
  }

  async switchDevices(selection: CallDeviceSelection): Promise<void> {
    const inputChanged = selection.inputDeviceId !== this.devices.inputDeviceId;
    const outputChanged = selection.outputDeviceId !== this.devices.outputDeviceId;
    const cameraChanged = selection.cameraDeviceId !== this.devices.cameraDeviceId;
    this.devices = { ...this.devices, ...selection };
    if (outputChanged && this.audioCtx && typeof this.audioCtx.setSinkId === "function") {
      try {
        await this.audioCtx.setSinkId(this.devices.outputDeviceId ?? "");
      } catch {
        this.options.onError("output_device_failed");
      }
    }
    if (inputChanged) {
      await this.openMicrophone();
    }
    if (cameraChanged && this.cameraEnabled) {
      await this.startCameraCapture();
    }
  }

  updateTracks(sendTracks: CallTrack[], receiveTracks: CallTrack[]): void {
    const nextAudioSend = sendTracks.find((t) => t.media === "audio");
    if (nextAudioSend) {
      this.audioSendGeneration = nextAudioSend.generation;
    }
    const nextVideoSend =
      sendTracks.find((t): t is CallVideoTrack => t.media === "video") ?? null;
    const codecChanged =
      nextVideoSend?.format.codec !== this.videoSendTrack?.format.codec;
    this.videoSendTrack = nextVideoSend;
    if (codecChanged && this.cameraEnabled && nextVideoSend) {
      void this.startCameraCapture();
    }

    const activeAudioReceiveIds = new Set<number>();
    const activeVideoReceiveIds = new Set<number>();
    for (const track of receiveTracks) {
      if (track.media === "audio") {
        activeAudioReceiveIds.add(track.id);
        this.audioReceiveTrackIds.add(track.id);
      } else if (track.media === "video") {
        activeVideoReceiveIds.add(track.id);
        this.videoReceiveTrackParticipants.set(track.id, track.participantId);
      }
    }
    for (const id of [...this.audioReceiveTrackIds]) {
      if (!activeAudioReceiveIds.has(id)) {
        this.audioReceiveTrackIds.delete(id);
        this.workletNode?.port.postMessage({ type: "clearTrack", trackId: id });
      }
    }
    for (const id of [...this.videoReceiveTrackParticipants.keys()]) {
      if (!activeVideoReceiveIds.has(id)) {
        this.videoReceiveTrackParticipants.delete(id);
        const dec = this.decoders.get(id);
        if (dec) {
          try {
            dec.decoder.close();
          } catch {}
          this.decoders.delete(id);
        }
        this.options.onTrackCleared(id);
      }
    }
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.stopCameraCapture();
    for (const dec of this.decoders.values()) {
      try {
        dec.decoder.close();
      } catch {}
    }
    this.decoders.clear();
    this.audioReceiveTrackIds.clear();
    this.videoReceiveTrackParticipants.clear();
    this.decodingJpegTracks.clear();
    try {
      this.micSource?.disconnect();
    } catch {}
    this.micSource = null;
    stopStream(this.micStream);
    this.micStream = null;
    if (this.workletNode) {
      try {
        this.workletNode.port.postMessage({ type: "clearAll" });
        this.workletNode.disconnect();
      } catch {}
      this.workletNode = null;
    }
    if (this.audioCtx) {
      void this.audioCtx.close().catch(() => {});
      this.audioCtx = null;
    }
  }
}
