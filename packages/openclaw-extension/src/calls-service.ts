import { randomUUID } from "node:crypto";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { NativeCallError, type CallAudioFormat, type CallEvent, type CallOffer, type CallPendingOffer, type CallView, type NativeCallMediaClient, type NativeCallsClient, type NativeCallWebSocketFactory } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import type { ResolvedWeChatAccount } from "./types.ts";
import type { ResolvedWeChatCallsConfig } from "./calls-config.ts";
import { prepareOpenClawCallVoice, type CallVoiceSession, type PreparedCallVoice } from "./calls-voice.ts";
import { CallPcmOutput } from "./calls-audio.ts";

export type CallMediaPort = Pick<NativeCallMediaClient, "frames" | "tracks" | "sendPcm" | "stopAudio" | "close">;
export type CallControlPort = Pick<NativeCallsClient, "connect" | "events" | "ready" | "dial" | "answer" | "end" | "clearAudio" | "close"> & { attachMedia(callId: string): Promise<CallMediaPort> };
export type CallServiceDependencies = {
  client?: CallControlPort;
  prepareVoice?: typeof prepareOpenClawCallVoice;
  host?: typeof ensureNativeHost;
  socketFactory?: NativeCallWebSocketFactory;
};
type Owned = { key: string; peer: string; callId?: string; call?: CallView; claimed: boolean; submitted: boolean; abort: AbortController; retired: boolean; attaching: boolean; endStarted: boolean;
  ready: boolean; incomingReadinessAttempted: boolean; claiming: boolean; candidateRevision?: number; offered?: CallOffer; voice?: CallVoiceSession; media?: CallMediaPort; output?: CallPcmOutput; timer?: ReturnType<typeof setTimeout>; endTimer?: ReturnType<typeof setTimeout>; voiceClosing?: Promise<void> };
const TERMINAL_WAIT_MS = 30_000;
const bounded = async <T>(promise: Promise<T>, ms: number): Promise<T> => {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try { return await Promise.race([promise, new Promise<never>((_, reject) => { timer = setTimeout(() => reject(Error("Calling operation timed out")), ms); })]); }
  finally { clearTimeout(timer); }
};
export async function nativeCallSocketFactory(): Promise<NativeCallWebSocketFactory> {
  const wsPath: string = "ws";
  const module = await import(wsPath);
  const WebSocket = module.default;
  if (typeof WebSocket !== "function") throw Error("Calling WebSocket transport unavailable");
  return (url, options) => new WebSocket(url, { ...options, followRedirects: false, perMessageDeflate: false });
}

/** An ordinary daemon client. Connecting grants neither a call claim nor chat
 * authority, and a failed/uncertain command is never retried as a new call. */
export class NativeCallService {
  private client?: CallControlPort;
  private prepared?: PreparedCallVoice;
  private format?: CallAudioFormat;
  private active?: Owned;
  private stopped = false;
  private busy = false;
  private seen = new Set<string>();
  private loop?: Promise<void>;
  private state = "disabled";
  private readonly config?: ResolvedWeChatCallsConfig;
  private account: ResolvedWeChatAccount;
  private cfg: unknown;
  private runtime: PluginRuntime;
  private dependencies: CallServiceDependencies;
  constructor(account: ResolvedWeChatAccount, cfg: unknown, runtime: PluginRuntime, dependencies: CallServiceDependencies = {}) {
    this.account = account; this.cfg = cfg; this.runtime = runtime; this.dependencies = dependencies; this.config = account.calls;
  }
  status(): { state: string; owned: boolean; callId?: string } {
    return { state: this.state, owned: this.active?.claimed === true, ...(this.active?.claimed && this.active.callId ? { callId: this.active.callId } : {}) };
  }
  async start(): Promise<void> {
    if (!this.config || !this.account.enabled || this.stopped || this.state !== "disabled") return;
    this.state = "starting";
    try {
      this.prepared = await bounded((this.dependencies.prepareVoice ?? prepareOpenClawCallVoice)(this.config, this.cfg, this.runtime), 10000);
      if (this.stopped) return;
      if (this.dependencies.client) this.client = this.dependencies.client;
      else {
        const native = this.account.native;
        if (!native) throw Error("Native calling configuration unavailable");
        const host = await (this.dependencies.host ?? ensureNativeHost)(native);
        this.client = host.client.calls(native.accountId, this.dependencies.socketFactory ?? await nativeCallSocketFactory());
      }
      if (this.stopped) { this.client.close(); return; }
      const hello = await this.client.connect();
      if (this.stopped) { this.client.close(); return; }
      if (!hello.capabilities.directAudio || hello.audioFormat.channels !== 1 || hello.audioFormat.frameMs !== 20) throw Error("Native direct voice unavailable");
      this.format = hello.audioFormat; this.busy = hello.availability.busy;
      // Observing pending metadata grants no ACK permission. Only an allowed,
      // provider-ready exact offer registers incoming readiness below.
      this.state = this.busy ? "busy" : "available";
      this.loop = this.consume();
    } catch { this.state = this.stopped ? "stopped" : "unavailable"; this.client?.close(); }
  }
  private current(owned: Owned): boolean { return !this.stopped && this.active === owned && !owned.retired && !owned.abort.signal.aborted; }
  private async consume(): Promise<void> {
    try {
      for await (const event of this.client!.events()) {
        if (this.stopped) break;
        if (event.type === "hello") {
          if (event.currentOffer) this.offer(event.currentOffer);
          else if (event.currentPendingOffer) this.pending(event.currentPendingOffer);
        } else if (event.type === "call.pending" && event.offer) this.pending(event.offer);
        else if (event.type === "call.offered") this.offer(event.offer);
        else if (event.type === "call.state") {
          const active = this.active;
          if (!active || (active.callId && active.callId !== event.call.callId)) continue;
          if (event.call.kind !== "direct") { this.state = "needs_review"; await this.retire(active); this.client?.close(); continue; }
          if (!event.call.ownedByThisClient) {
            // An offered call may be won by another connection during preflight.
            if (active.callId === event.call.callId && event.call.state !== "offered") { await this.retire(active); this.state = event.call.state === "ended" ? "available" : "busy"; }
            continue;
          }
          active.callId = event.call.callId; active.call = event.call;
          if (["ending", "ended", "uncertain"].includes(event.call.state)) {
            active.output?.close(); active.media?.stopAudio(); this.stopVoice(active);
            if (event.call.state === "ending") { active.endStarted = true; this.armTerminalWait(active); }
            else {
              this.state = event.call.state === "uncertain" ? "needs_review" : "available";
              await this.retire(active);
            }
          } else if (event.call.state === "connected" && active.claimed) void this.attach(active);
        } else if (event.type === "calls.availability") {
          this.busy = event.availability.busy;
          if (this.active?.claimed && !event.availability.ownedByThisClient) await this.retire(this.active);
          if (event.availability.busy && !this.active && this.state !== "needs_review") this.state = "busy";
          if (!event.availability.busy) { if (this.active?.claimed) await this.retire(this.active); if (this.state !== "needs_review") this.state = "available"; }
        } else if (event.type === "call.error" && ["outcome_unknown", "forbidden", "closed"].includes(event.code)) {
          this.state = "needs_review"; if (this.active) await this.retire(this.active); this.client?.close(); break;
        }
      }
    } catch { if (!this.stopped) { this.state = "needs_review"; if (this.active) await this.retire(this.active); this.client?.close(); } }
  }
  private offer(offer: CallOffer): void {
    if (!this.allowedIncoming(offer)) return;
    const existing = this.active;
    if (existing?.callId === offer.callId && existing.peer === offer.callerId && !existing.claimed) {
      existing.offered = offer; existing.candidateRevision = offer.revision;
      if (existing.ready && existing.voice) void this.claim(existing, offer);
      return;
    }
    if (existing || this.seen.has(offer.callId) || this.state === "needs_review") return;
    const active = this.incoming(offer);
    active.offered = offer;
    void this.prepareIncoming(active);
  }
  private allowedIncoming(offer: Pick<CallOffer, "kind" | "callType" | "callerId">): boolean {
    return !!this.config && this.config.inboundPolicy === "allowlist" && this.config.inboundAllowFrom.includes(offer.callerId)
      && offer.kind === "direct" && offer.callType === "voice";
  }
  private pending(offer: CallPendingOffer): void {
    if (!this.allowedIncoming(offer)) return;
    if (this.active?.callId === offer.callId && this.active.peer === offer.callerId && !this.active.submitted) {
      this.active.candidateRevision = offer.revision; return;
    }
    if (this.active || this.seen.has(offer.callId) || this.state === "needs_review") return;
    void this.prepareIncoming(this.incoming(offer));
  }
  private incoming(offer: Pick<CallOffer, "callId" | "callerId" | "revision">): Owned {
    if (this.seen.size >= 128) this.seen.delete(this.seen.values().next().value!);
    this.seen.add(offer.callId);
    const active = this.reserve(offer.callerId, offer.callId);
    active.candidateRevision = offer.revision;
    active.timer = setTimeout(() => { if (this.current(active) && !active.claimed) void this.fail(active); }, 20000);
    active.timer.unref?.(); return active;
  }
  private async prepareIncoming(active: Owned): Promise<void> {
    try {
      await this.openVoice(active); if (!this.current(active)) return;
      active.incomingReadinessAttempted = true;
      await this.client!.ready(randomUUID(), true, this.format!, { kind: "offer", callId: active.callId!, expectedRevision: active.candidateRevision! });
      if (!this.current(active)) return;
      active.ready = true;
      if (active.offered) void this.claim(active, active.offered);
    } catch (error) { await this.claimFailed(active, error); }
  }
  private reserve(peer: string, callId?: string): Owned {
    const active: Owned = { key: randomUUID(), peer, callId, claimed: false, submitted: false, ready: false, incomingReadinessAttempted: false, claiming: false, abort: new AbortController(), retired: false, attaching: false, endStarted: false };
    this.active = active; this.state = "preparing"; return active;
  }
  private async openVoice(active: Owned): Promise<void> {
    const opening = this.prepared!.open({ audio: (audio, metadata) => { if (this.current(active)) active.output?.enqueue(audio, metadata); },
      clear: () => { if (this.current(active)) void active.output?.clear(); }, failed: () => { if (this.current(active)) void this.fail(active); } },
    { senderId: active.peer, signal: active.abort.signal });
    // If timeout wins, a late provider must still be closed rather than adopted.
    void opening.then(voice => { if (!this.current(active)) void Promise.resolve(voice.close()).catch(() => {}); }, () => {});
    active.voice = await bounded(opening, 10000);
    if (!this.current(active)) throw Error("Calling preflight superseded");
  }
  private async claim(active: Owned, offer: CallOffer): Promise<void> {
    if (!this.current(active) || active.claiming || active.claimed || !active.ready || !active.voice) return;
    active.claiming = true;
    try {
      active.submitted = true;
      const result = await this.client!.answer(randomUUID(), offer.callId, offer.revision);
      if (!this.current(active)) return;
      if (result.callId !== offer.callId || !result.ownerToken) throw Error("Calling claim not admitted");
      active.claimed = true; this.armDuration(active); this.state = "busy";
      if (active.call?.state === "connected") void this.attach(active);
    } catch (error) { await this.claimFailed(active, error); }
  }
  private async claimFailed(active: Owned, error: unknown): Promise<void> {
    if (this.current(active)) {
        const knownLoss = error instanceof NativeCallError && ["already_claimed", "offer_expired", "stale", "busy", "unsupported"].includes(error.code) && !error.outcomeUnknown;
        this.state = knownLoss ? (this.busy ? "busy" : "available") : active.submitted || active.incomingReadinessAttempted ? "needs_review" : "unavailable";
        await this.retire(active);
        if (!knownLoss) this.client?.close();
    }
  }
  async dial(peer: string, access: { signal?: AbortSignal; assertCurrent: () => void }): Promise<{ callId: string }> {
    access.signal?.throwIfAborted(); access.assertCurrent();
    if (!this.config || this.config.outboundPolicy !== "allowlist" || !this.config.outboundAllowTo.includes(peer) || peer.endsWith("@chatroom")) throw Error("Calling destination not permitted");
    if (this.stopped || this.state !== "available" || this.active || this.busy || !this.prepared || !this.client) throw Error("Calling service is not available");
    const active = this.reserve(peer);
    const aborted = () => { if (this.current(active) && !active.claimed) void this.fail(active); };
    access.signal?.addEventListener("abort", aborted, { once: true });
    try {
      await this.openVoice(active); access.signal?.throwIfAborted(); access.assertCurrent();
      if (!this.current(active)) throw Error("Calling preflight superseded");
      await this.client.ready(randomUUID(), true, this.format!, { kind: "outgoing" });
      access.signal?.throwIfAborted(); access.assertCurrent(); if (!this.current(active)) throw Error("Calling preflight superseded");
      active.submitted = true;
      const result = await this.client.dial(randomUUID(), peer);
      if (!this.current(active) || !result.callId || !result.ownerToken) throw Error("Calling dial outcome not admitted");
      active.callId = result.callId; active.claimed = true; this.state = "busy"; this.armDuration(active);
      if (active.call?.state === "connected") void this.attach(active);
      return { callId: result.callId };
    } catch (error) {
      const unknown = active.submitted && (!(error instanceof NativeCallError) || error.outcomeUnknown);
      if (this.current(active)) { this.state = unknown ? "needs_review" : active.submitted ? "available" : "unavailable"; await this.retire(active); if (unknown || !active.submitted) this.client.close(); }
      throw Error(unknown ? "Call outcome needs review; no automatic redial" : "Call was not admitted");
    } finally { access.signal?.removeEventListener("abort", aborted); }
  }
  private armDuration(active: Owned): void {
    clearTimeout(active.timer);
    active.timer = setTimeout(() => { if (this.current(active)) void this.end().catch(() => {}); }, this.config!.maxDurationSeconds * 1000);
    active.timer.unref?.();
  }
  private async attach(active: Owned): Promise<void> {
    if (!this.current(active) || !active.claimed || active.attaching || active.media || !active.callId || active.endStarted) return;
    active.attaching = true;
    try {
      const media = await this.client!.attachMedia(active.callId);
      if (this.stopped || this.active !== active || active.retired) { media.close(); return; }
      active.media = media;
      if (active.endStarted) { media.stopAudio(); this.armTerminalWait(active); return; }
      if (!this.current(active)) { media.close(); return; }
      const tracks = media.tracks();
      if (JSON.stringify(tracks.sendTrack.format) !== JSON.stringify(this.format) || JSON.stringify(tracks.receiveTrack.format) !== JSON.stringify(this.format)) throw Error("Calling PCM changed");
      active.output = new CallPcmOutput(media, this.format!, this.prepared!, () => this.client!.clearAudio(randomUUID(), active.callId!), () => { void this.fail(active); });
      const resampler = this.prepared!.resampler(this.format!.sampleRate, this.prepared!.sampleRate);
      void (async () => {
        try {
          for await (const frame of media.frames()) {
            if (!this.current(active) || active.endStarted) break;
            const audio = resampler.process(Buffer.from(frame.payload));
            if (audio.length > this.prepared!.sampleRate * 2 || frame.timestampMicros > BigInt(Number.MAX_SAFE_INTEGER)) throw Error("Calling input budget exceeded");
            active.voice!.setMediaTimestamp(Number(frame.timestampMicros) / 1000);
            active.voice!.sendAudio(audio);
          }
        } catch { if (this.current(active) && !active.endStarted) await this.fail(active); }
      })();
    } catch { if (this.current(active)) await this.fail(active); }
    finally { active.attaching = false; }
  }
  async end(): Promise<void> {
    const active = this.active;
    if (!active?.claimed || !active.callId || !active.call || !this.client || active.endStarted) throw Error("No owned call can be ended");
    if (!["dialing", "ringing", "connecting", "connected"].includes(active.call.state)) throw Error("Call cannot be ended in this state");
    active.endStarted = true; active.output?.close(); active.media?.stopAudio(); this.stopVoice(active); this.armTerminalWait(active);
    // Closing media revokes the daemon owner. Acceptance and Ending can precede
    // the upstream terminal reply, so retain its connection until terminal state.
    try {
      await this.client.end(["dialing", "ringing"].includes(active.call.state) ? "call.cancel" : "call.hangup", randomUUID(), active.callId, active.call.revision);
      // Acceptance is not a terminal result. Keep the owner until server state arrives.
    } catch { this.state = "needs_review"; this.client.close(); await this.retire(active); throw Error("Call end outcome needs review"); }
  }
  private async fail(active: Owned): Promise<void> {
    if (active.endStarted && !this.stopped && this.active === active && !active.retired) {
      active.output?.close(); active.media?.stopAudio(); this.stopVoice(active); this.armTerminalWait(active); return;
    }
    if (!this.current(active)) return;
    active.output?.close();
    if (active.claimed && active.call && !active.endStarted) {
      try { await bounded(this.end(), 2000); return; } catch {}
    }
    this.state = active.submitted || active.incomingReadinessAttempted ? "needs_review" : "unavailable"; await this.retire(active); this.client?.close();
  }
  private stopVoice(active: Owned): void {
    active.abort.abort();
    const voice = active.voice;
    if (!voice) return;
    active.voice = undefined;
    active.voiceClosing = bounded(Promise.resolve().then(() => voice.close()), 1000).then(() => {}, () => {});
  }
  private armTerminalWait(active: Owned): void {
    clearTimeout(active.timer);
    if (active.endTimer) return;
    active.endTimer = setTimeout(() => {
      if (this.stopped || this.active !== active || active.retired) return;
      this.state = "needs_review"; this.client?.close(); void this.retire(active);
    }, TERMINAL_WAIT_MS);
    active.endTimer.unref?.();
  }
  private async retire(active: Owned): Promise<void> {
    if (active.retired) return;
    active.retired = true; clearTimeout(active.timer); clearTimeout(active.endTimer); active.output?.close(); active.media?.close(); this.stopVoice(active);
    if (this.active === active) this.active = undefined;
    await active.voiceClosing;
  }
  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    if (this.active?.claimed && !this.active.endStarted) { try { await bounded(this.end(), 2000); } catch {} }
    this.client?.close(); if (this.active) await this.retire(this.active);
    this.state = "stopped"; if (this.loop) { try { await bounded(this.loop, 1000); } catch {} }
  }
}
