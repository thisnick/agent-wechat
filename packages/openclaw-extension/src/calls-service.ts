import { randomUUID } from "node:crypto";
import { join } from "node:path";
import type { OpenClawConfig, PluginRuntime } from "openclaw/plugin-sdk/core";
import { isNativeTombstone, NativeCallError, type NativeMessage, type CallAudioFormat, type CallEvent, type CallOffer, type CallPendingOffer, type CallView, type CallConferenceInvitation, type NativeCallMediaClient, type NativeCallsClient, type NativeCallPcmObserver, type NativeCallWebSocketFactory } from "@agent-wechat/shared";
import { ensureNativeHost } from "./native-host.ts";
import { resolveWeChatAccount, type ResolvedWeChatAccount } from "./types.ts";
import type { ResolvedWeChatVoiceCallsConfig } from "./calls-config.ts";
import { prepareOpenClawCallVoice, type CallBackgroundRunner, type CallBinding, type CallVoiceSession, type PreparedCallVoice } from "./calls-voice.ts";
import { CallPcmOutput } from "./calls-audio.ts";
import { CallPcmMixer } from "./calls-mixer.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { CallAudioTrace } from "./calls-audio-trace.ts";
import { callerChatSessionKey, resolveDialingSession, type CallParentSession } from "./calls-session.ts";
import { CallSummaryStore, CallTranscript, formatCallTranscript, formatConsultChatContext, formatDuration, formatRecentCallContext } from "./calls-memory.ts";

export type CallMediaPort = Pick<NativeCallMediaClient, "frames" | "tracks" | "sendPcm" | "stopAudio" | "close">;
export type CallControlPort = Pick<NativeCallsClient, "connect" | "events" | "ready" | "dial" | "answer" | "end" | "clearAudio" | "close"> & Partial<Pick<NativeCallsClient, "startConference" | "joinConference" | "invite">> & { attachMedia(callId: string, observePcm?: NativeCallPcmObserver): Promise<CallMediaPort> };
export type CallServiceDependencies = {
  client?: CallControlPort;
  prepareVoice?: typeof prepareOpenClawCallVoice;
  host?: typeof ensureNativeHost;
  socketFactory?: NativeCallWebSocketFactory;
  logAudioTrace?: (line: string) => void;
  /** Opens a fresh control client, e.g. to resume after calling stopped (tests). */
  createClient?: () => CallControlPort;
  /** Always-on warnings, independent of audio tracing. */
  warn?: (line: string) => void;
  info?: (line: string) => void;
  runInBackgroundContext?: CallBackgroundRunner;
  /** The plugin state directory, where call summaries are kept. */
  stateDir?: string;
  summaries?: CallSummaryStore;
};
type Owned = { key: string; peer: string; groupId?: string; direction: "incoming" | "outgoing"; transcript: CallTranscript; connectedAt?: number; binding?: CallBinding; summarized?: boolean;
  inputTimer?: ReturnType<typeof setInterval>; invitationResults?: Extract<CallEvent, { type: "call.invitation_result" }>;
  callId?: string; parentSession?: CallParentSession; call?: CallView; claimed: boolean; submitted: boolean; abort: AbortController; retired: boolean; attaching: boolean; endStarted: boolean;
  ready: boolean; incomingReadinessAttempted: boolean; claiming: boolean; candidateRevision?: number; offered?: CallOffer; voice?: CallVoiceSession; media?: CallMediaPort; output?: CallPcmOutput; trace?: CallAudioTrace; timer?: ReturnType<typeof setTimeout>; endTimer?: ReturnType<typeof setTimeout>; voiceClosing?: Promise<void> };
type CallSystemEvents = { enqueueSystemEvent?: (text: string, options: { sessionKey: string; contextKey?: string }) => unknown;
  requestHeartbeatNow?: (options: { sessionKey?: string; agentId?: string; reason?: string; source?: string }) => void };
const TERMINAL_WAIT_MS = 30_000;
const RECENT_CONTEXT_WAIT_MS = 2_000;
const MAX_RECOVERY_ATTEMPTS = 30;
type CallAccess = { signal?: AbortSignal; assertCurrent: () => void; parentSession?: CallParentSession };
/** A fixed error code for logs; other error text can contain private details. */
const codeOf = (error: unknown): string => error instanceof NativeCallError ? `:${error.code}` : "";
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
  private capabilities: Extract<CallEvent, { type: "hello" }>["capabilities"] = {};
  private invitations: CallConferenceInvitation[] = [];
  private active?: Owned;
  private stopped = false;
  private busy = false;
  private unconfirmedCallId?: string;
  private seen = new Set<string>();
  private loop?: Promise<void>;
  private state = "disabled";
  private readonly config?: ResolvedWeChatVoiceCallsConfig;
  private readonly logAudioTrace?: (line: string) => void;
  private callingStopped = false;
  private recovery?: ReturnType<typeof setTimeout>;
  private recoveryAttempts = 0;
  private account: ResolvedWeChatAccount;
  private cfg: unknown;
  private runtime: PluginRuntime;
  private dependencies: CallServiceDependencies;
  private readonly summaries?: CallSummaryStore;
  /** Aborts work that outlives a call, such as its summary, when the service stops. */
  private readonly lifetime = new AbortController();
  constructor(account: ResolvedWeChatAccount, cfg: unknown, runtime: PluginRuntime, dependencies: CallServiceDependencies = {}) {
    this.account = account; this.cfg = cfg; this.runtime = runtime; this.dependencies = dependencies; this.config = account.calls?.voice;
    this.summaries = dependencies.summaries ?? (dependencies.stateDir ? new CallSummaryStore(join(dependencies.stateDir, "agent-wechat", "call-summaries.json")) : undefined);
    if (process.env.WX_CALL_AUDIO_TRACE === "1") this.logAudioTrace = dependencies.logAudioTrace ?? (line => console.info(line));
    if (!this.config && account.calls?.video.enabled) this.state = "unavailable";
  }
  status(): { state: string; owned: boolean; callId?: string; groupId?: string; participants?: CallView["participants"]; conferenceInvitations?: CallConferenceInvitation[]; invitationResults?: Extract<CallEvent, { type: "call.invitation_result" }>["results"] } {
    const active = this.active;
    return { state: this.state, owned: active?.claimed === true, ...(active?.claimed && active.callId ? { callId: active.callId } : {}),
      ...(Object.keys(this.config?.conferences.groups ?? {}).length ? { conferenceInvitations: this.invitations.filter(item => !!this.groupPolicy(item.groupId)).map(item => ({ ...item })) } : {}),
      ...(active?.groupId ? { groupId: active.groupId, participants: structuredClone(active.call?.participants ?? []),
        ...(active.invitationResults ? { invitationResults: structuredClone(active.invitationResults.results) } : {}) } : {}) };
  }
  async start(): Promise<void> {
    if (!this.config || !this.account.enabled || this.stopped || this.state !== "disabled") return;
    this.state = "starting";
    try {
      this.prepared = await bounded((this.dependencies.prepareVoice ?? prepareOpenClawCallVoice)(this.config, this.cfg, this.runtime), 10000);
      if (this.stopped) return;
      this.client = await this.openClient();
      if (this.stopped) { this.client.close(); return; }
      const hello = await this.client.connect();
      if (this.stopped) { this.client.close(); return; }
      if ((!hello.capabilities.directAudio && !hello.capabilities.conferenceAudio) || hello.audioFormat.channels !== 1 || hello.audioFormat.frameMs !== 20) throw Error("Native voice unavailable");
      this.capabilities = hello.capabilities;
      this.format = hello.audioFormat; this.busy = hello.availability.busy;
      // Observing pending metadata grants no ACK permission. Only an allowed,
      // provider-ready exact offer registers incoming readiness below.
      this.state = this.busy ? "busy" : "available";
      this.loop = this.consume();
      try { this.logAudioTrace?.(`[wechat-call-audio] plugin service-ready state=${this.state}`); } catch { /* Diagnostics cannot disable calling. */ }
    } catch { this.state = this.stopped ? "stopped" : "unavailable"; if (this.stopped) this.client?.close(); else this.stopCalling("start-failed"); }
  }
  private warn(line: string): void {
    try { this.dependencies.warn?.(`[wechat-call] ${line}`); } catch { /* Diagnostics cannot affect calling. */ }
  }
  private info(line: string): void {
    try { this.dependencies.info?.(`[wechat-call] ${line}`); } catch { /* Diagnostics cannot affect calling. */ }
  }
  private async openClient(): Promise<CallControlPort> {
    if (this.dependencies.createClient) return this.dependencies.createClient();
    if (this.dependencies.client) return this.dependencies.client;
    const native = this.account.native;
    if (!native) throw Error("Native calling configuration unavailable");
    const host = await (this.dependencies.host ?? ensureNativeHost)(native);
    return host.client.calls(native.accountId, this.dependencies.socketFactory ?? await nativeCallSocketFactory());
  }
  /** Closing the control socket ends event delivery; calling resumes on a new
   * connection once the daemon reports that no call is in progress. */
  private stopCalling(reason: string): void {
    this.callingStopped = true;
    this.warn(`calling stopped: state=${this.state} reason=${reason}; it resumes once the daemon reports no call in progress`);
    this.client?.close();
    this.scheduleRecovery();
  }
  private scheduleRecovery(): void {
    // A single injected client cannot reconnect (tests without createClient).
    // Only a prepared service can resume; a failed start is not a lost call.
    if (this.stopped || this.recovery || !this.prepared || (this.dependencies.client && !this.dependencies.createClient)) return;
    if (this.recoveryAttempts >= MAX_RECOVERY_ATTEMPTS) {
      this.warn(`calling still stopped after ${MAX_RECOVERY_ATTEMPTS} reconnect attempts; reload the agent-wechat channel or restart the gateway`);
      return;
    }
    const delay = Math.min(60_000, 5_000 * 2 ** this.recoveryAttempts);
    this.recovery = setTimeout(() => { this.recovery = undefined; void this.recover(); }, delay);
    this.recovery.unref?.();
  }
  /** Resume only when the daemon, the source of truth, has no call in progress:
   * an uncertain outcome is settled there, never by redialing or guessing. */
  private async recover(): Promise<void> {
    if (this.stopped) return;
    if (this.active) { this.scheduleRecovery(); return; }
    this.recoveryAttempts++;
    let client: CallControlPort | undefined;
    try {
      client = await this.openClient();
      const hello = await client.connect();
      if (this.stopped || this.active) { client.close(); if (!this.stopped) this.scheduleRecovery(); return; }
      if ((!hello.capabilities.directAudio && !hello.capabilities.conferenceAudio) || hello.audioFormat.channels !== 1 || hello.audioFormat.frameMs !== 20
        || hello.availability.busy || hello.currentCall) { client.close(); this.scheduleRecovery(); return; }
      this.client = client; this.format = hello.audioFormat; this.busy = false; this.capabilities = hello.capabilities;
      this.callingStopped = false; this.recoveryAttempts = 0; this.state = "available";
      this.loop = this.consume();
      this.warn("calling resumed: the daemon reports no call in progress");
    } catch { client?.close(); this.scheduleRecovery(); }
  }
  private current(owned: Owned): boolean { return !this.stopped && this.active === owned && !owned.retired && !owned.abort.signal.aborted; }
  private async consume(): Promise<void> {
    try {
      for await (const event of this.client!.events()) {
        if (this.stopped) break;
        if (event.type === "hello") {
          if (event.currentOffer) this.offer(event.currentOffer);
          else if (event.currentPendingOffer) this.pending(event.currentPendingOffer);
          this.invitations = event.conferenceInvitations; this.autoJoinConference();
        } else if (event.type === "call.pending") {
          const active = this.active;
          if (event.offer) this.pending(event.offer);
          // The daemon sends availability and any promoted offer before the
          // pending snapshot. An idle daemon with neither offer withdrew it.
          else if (!this.busy && active?.callId && !active.offered && !active.submitted) {
            this.state = "available";
            await this.retire(active);
          }
        } else if (event.type === "call.offered") this.offer(event.offer);
        else if (event.type === "call.state") {
          if (event.call.state === "ended" && this.unconfirmedCallId === event.call.callId && !this.active) {
            this.unconfirmedCallId = undefined;
            this.state = this.busy ? "busy" : "available";
          }
          const active = this.active;
          if (!active || (active.callId && active.callId !== event.call.callId)) continue;
          if (!active.callId && !event.call.ownedByThisClient) continue;
          if (event.call.kind !== (active.groupId ? "conference" : "direct") || event.call.callType !== "voice"
            || (active.groupId !== undefined && event.call.groupId !== active.groupId)) { this.state = "needs_review"; await this.retire(active); this.stopCalling("unsupported-call"); continue; }
          if (!event.call.ownedByThisClient) {
            // An offered call may be won by another connection during preflight.
            if (active.callId === event.call.callId && event.call.state !== "offered") { await this.retire(active); this.state = event.call.state === "ended" ? "available" : "busy"; }
            continue;
          }
          active.callId = event.call.callId; active.call = event.call;
          if (event.call.state === "connected") active.connectedAt ??= Date.now();
          if (["ending", "ended", "uncertain"].includes(event.call.state)) {
            active.output?.close(); active.media?.stopAudio(); this.stopVoice(active);
            if (event.call.state === "ending") { active.endStarted = true; this.armTerminalWait(active); }
            else {
              this.state = event.call.state === "uncertain" ? "needs_review" : "available";
              this.unconfirmedCallId = event.call.state === "uncertain" ? event.call.callId : undefined;
              await this.retire(active);
            }
          } else if (event.call.state === "connected" && active.claimed) void this.attach(active);
        } else if (event.type === "conference.invitations") {
          this.invitations = event.invitations; this.autoJoinConference();
        } else if (event.type === "participant.updated") {
          const call = this.active?.call;
          if (call?.callId === event.callId && event.revision >= call.revision) {
            call.revision = event.revision;
            call.participants = [...(call.participants ?? []).filter(member => member.participantId !== event.participant.participantId), event.participant];
          }
        } else if (event.type === "call.invitation_result") {
          if (this.active?.callId === event.callId) this.active.invitationResults = event;
        } else if (event.type === "calls.availability") {
          this.busy = event.availability.busy;
          if (this.active?.claimed && !event.availability.ownedByThisClient) await this.retire(this.active);
          if (event.availability.busy && !this.active && this.state !== "needs_review") this.state = "busy";
          if (!event.availability.busy) { if (this.active?.claimed) await this.retire(this.active); if (this.state !== "needs_review") this.state = "available"; this.autoJoinConference(); }
        } else if (event.type === "call.error" && ["outcome_unknown", "forbidden", "closed"].includes(event.code)) {
          this.state = "needs_review"; if (this.active) await this.retire(this.active); this.stopCalling(`call-error-${event.code}`); break;
        }
      }
      if (!this.stopped && !this.callingStopped) {
        // The daemon closed the control connection; no further offers arrive.
        this.state = "unavailable"; if (this.active) await this.retire(this.active); this.stopCalling("events-ended");
      }
    } catch (error) { if (!this.stopped) { this.state = "needs_review"; if (this.active) await this.retire(this.active); this.stopCalling(`events-failed${codeOf(error)}`); } }
  }
  private offer(offer: CallOffer): void {
    if (!this.allowedIncoming(offer)) return;
    const existing = this.active;
    if (existing?.callId === offer.callId && existing.peer === offer.callerId && !existing.claimed) {
      existing.offered = offer; existing.candidateRevision = offer.revision;
      if (existing.ready && existing.voice) void this.claim(existing, offer);
      return;
    }
    if (this.state === "needs_review" && !this.seen.has(offer.callId)) { this.seen.add(offer.callId); this.warn("incoming call ignored: calling stopped after an uncertain outcome (state=needs_review)"); }
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
    if (this.state === "needs_review" && !this.seen.has(offer.callId)) { this.seen.add(offer.callId); this.warn("incoming call ignored: calling stopped after an uncertain outcome (state=needs_review)"); }
    if (this.active || this.seen.has(offer.callId) || this.state === "needs_review") return;
    void this.prepareIncoming(this.incoming(offer));
  }
  private incoming(offer: Pick<CallOffer, "callId" | "callerId" | "revision">): Owned {
    if (this.seen.size >= 128) this.seen.delete(this.seen.values().next().value!);
    this.seen.add(offer.callId);
    const active = this.reserve(offer.callerId, "incoming", offer.callId);
    const parent = this.config!.consultation.inboundSessionKey;
    if (parent) active.parentSession = { key: parent };
    active.candidateRevision = offer.revision;
    active.timer = setTimeout(() => { if (this.current(active) && !active.claimed) void this.fail(active, "unanswered-timeout"); }, 20000);
    active.timer.unref?.(); return active;
  }
  private async prepareIncoming(active: Owned): Promise<void> {
    const started = performance.now();
    let stage: "voice" | "ready" = "voice";
    try {
      await this.openVoice(active); if (!this.current(active)) return;
      stage = "ready";
      active.incomingReadinessAttempted = true;
      await this.client!.ready(randomUUID(), true, this.format!, { kind: "offer", callId: active.callId!, expectedRevision: active.candidateRevision! });
      if (!this.current(active)) return;
      active.ready = true;
      if (active.offered) void this.claim(active, active.offered, started);
    } catch (error) { await this.claimFailed(active, error, stage, started); }
  }
  private reserve(peer: string, direction: Owned["direction"], callId?: string, groupId?: string): Owned {
    const active: Owned = { key: randomUUID(), peer, groupId, direction, transcript: new CallTranscript(), callId, claimed: false, submitted: false, ready: false, incomingReadinessAttempted: false, claiming: false, abort: new AbortController(), retired: false, attaching: false, endStarted: false };
    this.active = active; this.state = "preparing"; return active;
  }
  /** What a call starts with: a recap for the voice model (earlier call summaries and the
   * latest chat messages) and the latest chat messages for the agent's first consultation.
   * One history read serves both; it never delays a call long. */
  private async callContext(peer: string): Promise<{ recentContext?: string; consultContext?: string }> {
    const voice = this.config!.realtime.recentContext;
    const consulting = this.config!.toolPolicy !== "none" ? this.config!.consultation.recentMessages : 0;
    const limit = Math.max(voice.enabled ? voice.maxMessages : 0, consulting);
    const native = this.account.native;
    if (!voice.enabled && !limit) return {};
    const [summaries, messages] = await Promise.all([
      voice.enabled ? this.summaries?.recent(this.account.accountId, peer).catch(() => []) ?? [] : [],
      native && limit ? (this.dependencies.host ?? ensureNativeHost)(native)
        .then(host => host.client.messages(native.accountId, peer, { limit }))
        .then(page => page.items.filter((item): item is NativeMessage => !isNativeTombstone(item) && item.threadId === peer))
        .catch(() => [] as NativeMessage[]) : [],
    ]);
    return {
      recentContext: voice.enabled ? formatRecentCallContext(summaries, messages.slice(0, voice.maxMessages)) : undefined,
      consultContext: consulting ? formatConsultChatContext(messages.slice(0, consulting)) : undefined,
    };
  }
  private async openVoice(active: Owned): Promise<void> {
    const runInBackgroundContext = this.dependencies.runInBackgroundContext;
    const { recentContext, consultContext } = await bounded(this.callContext(active.peer), RECENT_CONTEXT_WAIT_MS)
      .catch((): { recentContext?: string; consultContext?: string } => ({}));
    if (!this.current(active)) throw Error("Calling preflight superseded");
    const open = () => this.prepared!.open({ audio: (audio, metadata) => { if (this.current(active)) active.output?.enqueue(audio, metadata); },
      clear: () => { if (this.current(active)) void active.output?.clear(); },
      playbackState: () => this.current(active) ? active.output?.playbackState() ?? [] : [],
      failed: reason => { if (this.current(active)) void this.fail(active, reason ?? "provider"); },
      // Closing can publish the last transcript; it counts until the summary starts.
      transcript: (role, text, final, snapshot) => { if (!active.summarized) active.transcript.add(role, text, final, snapshot); } },
    { senderId: active.peer, signal: active.abort.signal,
      parentSession: active.groupId ? (this.groupPolicy(active.groupId)?.group.sessionKey ? { key: this.groupPolicy(active.groupId)!.group.sessionKey! } : undefined) : active.parentSession,
      ...(active.groupId ? { conference: { groupId: active.groupId } } : {}), recentContext, consultContext,
      channelAccountId: this.account.accountId, nativeAccountId: this.account.native?.accountId, logAudioTrace: this.logAudioTrace, runInBackgroundContext,
      connectedPeer: () => this.current(active) && (!active.groupId || this.groupPolicy(active.groupId)) && active.claimed && active.call?.state === "connected" && active.call.callId === active.callId
        && active.output && !active.endStarted ? active.peer : undefined });
    const opening = runInBackgroundContext ? runInBackgroundContext(open) : open();
    // If timeout wins, a late provider must still be closed rather than adopted.
    void opening.then(voice => { if (!this.current(active)) void Promise.resolve(voice.close()).catch(() => {}); }, () => {});
    active.voice = await bounded(opening, 10000);
    if (!this.current(active)) throw Error("Calling preflight superseded");
  }
  private async claim(active: Owned, offer: CallOffer, started = performance.now()): Promise<void> {
    if (!this.current(active) || active.claiming || active.claimed || !active.ready || !active.voice) return;
    active.claiming = true;
    try {
      active.submitted = true;
      const result = await this.client!.answer(randomUUID(), offer.callId, offer.revision);
      if (!this.current(active)) return;
      if (result.callId !== offer.callId || !result.ownerToken) throw Error("Calling claim not admitted");
      active.claimed = true; this.armDuration(active); this.state = "busy";
      if (active.call?.state === "connected") void this.attach(active);
    } catch (error) { await this.claimFailed(active, error, "answer", started); }
  }
  private async claimFailed(active: Owned, error: unknown, stage: "voice" | "ready" | "answer", started: number): Promise<void> {
    if (this.current(active)) {
        const knownLoss = error instanceof NativeCallError && ["already_claimed", "offer_expired", "stale", "busy", "unsupported"].includes(error.code) && !error.outcomeUnknown;
        this.state = knownLoss ? (this.busy ? "busy" : "available") : active.submitted || active.incomingReadinessAttempted ? "needs_review" : "unavailable";
        // An unanswered call otherwise leaves no trace. Only fixed values reach
        // logs: provider errors can contain private details.
        try {
          this.warn(`incoming call not answered: stage=${stage} ms=${Math.round(performance.now() - started)}`
            + ` code=${error instanceof NativeCallError ? error.code : "none"} state=${this.state}`);
        } catch { /* Diagnostics cannot affect calling. */ }
        await this.retire(active);
        if (!knownLoss) this.stopCalling("preflight-failed");
    }
  }
  private groupPolicy(groupId: string) {
    try {
      const cfg = this.runtime.config?.current?.() ?? this.cfg;
      const account = resolveWeChatAccount(cfg as Record<string, unknown>, this.account.accountId);
      const voice = account?.calls?.voice;
      if (!account?.enabled || !voice || voice.agentId !== this.config?.agentId || !account.native || !this.account.native
        || nativeConfigurationKey(account.native) !== nativeConfigurationKey(this.account.native)) return;
      const group = Object.hasOwn(voice.conferences.groups, groupId) ? voice.conferences.groups[groupId] : undefined;
      return group ? { voice, group } : undefined;
    } catch { return; }
  }
  private autoJoinConference(): void {
    if (this.stopped || this.state !== "available" || this.active || this.busy || !this.capabilities.conferenceJoin || !this.capabilities.conferenceAudio || !this.client?.joinConference) return;
    const invitation = this.invitations.find(item => this.groupPolicy(item.groupId)?.group.autoJoin
      && !this.seen.has(`conference:${item.invitationId}:${item.revision}`));
    if (!invitation) return;
    if (this.seen.size >= 128) this.seen.delete(this.seen.values().next().value!);
    this.seen.add(`conference:${invitation.invitationId}:${invitation.revision}`);
    void this.conference(invitation.groupId, { assertCurrent: () => {
      if (!this.groupPolicy(invitation.groupId)?.group.autoJoin) throw Error("Conference auto-join permission changed");
    } }, invitation).catch(() => this.warn("conference invitation was not joined; no automatic resubmission"));
  }
  async startConference(groupId: string, invitees: string[], access: CallAccess): Promise<{ callId: string }> {
    return this.conference(groupId, access, undefined, invitees);
  }
  async joinConference(groupId: string, access: CallAccess): Promise<{ callId: string }> {
    const matches = this.invitations.filter(item => item.groupId === groupId);
    if (matches.length !== 1) throw Error("No unambiguous current conference invitation; join never creates a room");
    return this.conference(groupId, access, matches[0]);
  }
  private allowedInvitees(groupId: string, contacts: string[]): void {
    const policy = this.groupPolicy(groupId);
    if (!policy || policy.voice.outboundPolicy !== "allowlist" || !contacts.length || contacts.length > 100
      || new Set(contacts).size !== contacts.length || contacts.some(id => !policy.voice.outboundAllowTo.includes(id) || id.endsWith("@chatroom"))) throw Error("Conference invitees not permitted");
  }
  private async conference(groupId: string, access: CallAccess, invitation?: CallConferenceInvitation, invitees?: string[]): Promise<{ callId: string }> {
    const assertAllowed = () => {
      access.signal?.throwIfAborted(); access.assertCurrent();
      if (!this.groupPolicy(groupId) || !this.capabilities.conferenceAudio
        || (invitation ? !this.capabilities.conferenceJoin || !this.client?.joinConference : !this.capabilities.conferenceCreate || !this.client?.startConference)) throw Error("Conference operation unavailable or not permitted");
      if (invitation && !this.invitations.some(item => item.invitationId === invitation.invitationId && item.revision === invitation.revision && item.groupId === groupId)) throw Error("Conference invitation changed; no join submitted");
      if (!invitation) this.allowedInvitees(groupId, invitees ?? []);
    };
    assertAllowed();
    if (this.stopped || this.state !== "available" || this.active || this.busy || !this.prepared || !this.client) throw Error("Calling service is not available");
    const active = this.reserve(groupId, invitation ? "incoming" : "outgoing", undefined, groupId);
    active.parentSession = access.parentSession;
    const aborted = () => { if (this.current(active) && !active.claimed) void this.fail(active, "conference-cancelled"); };
    access.signal?.addEventListener("abort", aborted, { once: true });
    try {
      await this.openVoice(active); assertAllowed();
      if (!this.current(active)) throw Error("Calling preflight superseded");
      await this.client.ready(randomUUID(), true, this.format!, { kind: "outgoing" });
      assertAllowed(); if (!this.current(active)) throw Error("Calling preflight superseded");
      active.submitted = true;
      const result = invitation ? await this.client.joinConference!(randomUUID(), invitation.invitationId, invitation.revision)
        : await this.client.startConference!(randomUUID(), groupId, [...invitees!]);
      if (!this.current(active) || !result.callId || !result.ownerToken) throw Error("Conference outcome not admitted");
      active.callId = result.callId; active.claimed = true; this.state = "busy"; this.armDuration(active);
      if (active.call?.state === "connected") void this.attach(active);
      return { callId: result.callId };
    } catch (error) {
      const unknown = active.submitted && (!(error instanceof NativeCallError) || error.outcomeUnknown);
      if (this.current(active)) {
        this.state = unknown ? "needs_review" : "available";
        await this.retire(active); if (unknown) this.stopCalling("conference-outcome-unknown");
      }
      throw Error(unknown ? "Conference outcome needs review; no automatic retry" : "Conference was not admitted");
    } finally { access.signal?.removeEventListener("abort", aborted); }
  }
  async invite(contacts: string[], access: CallAccess): Promise<{ requestId: string; status: "accepted" }> {
    access.signal?.throwIfAborted(); access.assertCurrent();
    const active = this.active;
    if (!active?.claimed || !active.groupId || !active.callId || active.call?.state !== "connected" || !this.client?.invite || !this.capabilities.conferenceInvite) throw Error("No owned conference can invite contacts");
    this.allowedInvitees(active.groupId, contacts);
    const requestId = randomUUID();
    access.signal?.throwIfAborted(); access.assertCurrent(); this.allowedInvitees(active.groupId, contacts);
    await this.client.invite(requestId, active.callId, [...contacts]);
    return { requestId, status: "accepted" };
  }
  async dial(peer: string, access: CallAccess): Promise<{ callId: string }> {
    access.signal?.throwIfAborted(); access.assertCurrent();
    if (!this.config || this.config.outboundPolicy !== "allowlist" || !this.config.outboundAllowTo.includes(peer) || peer.endsWith("@chatroom")) throw Error("Calling destination not permitted");
    if (!this.capabilities.directAudio) throw Error("Direct voice unavailable");
    if (this.stopped || this.state !== "available" || this.active || this.busy || !this.prepared || !this.client) throw Error("Calling service is not available");
    const active = this.reserve(peer, "outgoing");
    active.parentSession = access.parentSession;
    const aborted = () => { if (this.current(active) && !active.claimed) void this.fail(active, "dial-cancelled"); };
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
      if (this.current(active)) { this.state = unknown ? "needs_review" : active.submitted ? "available" : "unavailable"; await this.retire(active); if (unknown || !active.submitted) this.stopCalling("dial-failed"); }
      throw Error(unknown ? "Call outcome needs review; no automatic redial" : "Call was not admitted");
    } finally { access.signal?.removeEventListener("abort", aborted); }
  }
  private armDuration(active: Owned): void {
    clearTimeout(active.timer);
    active.timer = setTimeout(() => {
      if (!this.current(active)) return;
      this.warn(`ending call: it reached the ${this.config!.maxDurationSeconds}-second limit (maxDurationSeconds)`);
      void this.end().catch(() => {});
    }, this.config!.maxDurationSeconds * 1000);
    active.timer.unref?.();
  }
  private async attach(active: Owned): Promise<void> {
    if (!this.current(active) || !active.claimed || active.attaching || active.media || !active.callId || active.endStarted) return;
    active.attaching = true;
    try {
      const trace = this.logAudioTrace ? new CallAudioTrace(this.logAudioTrace, {
        daemonHz: this.format!.sampleRate, realtimeHz: this.prepared!.sampleRate,
      }) : undefined;
      active.trace = trace;
      const media = await this.client!.attachMedia(active.callId, trace?.observePcm);
      if (this.stopped || this.active !== active || active.retired) { media.close(); return; }
      active.media = media;
      if (active.endStarted) { media.stopAudio(); this.armTerminalWait(active); return; }
      if (!this.current(active)) { media.close(); return; }
      const tracks = media.tracks();
      if ((!active.groupId && tracks.receiveTracks.length !== 1) || JSON.stringify(tracks.sendTrack.format) !== JSON.stringify(this.format)
        || tracks.receiveTracks.some(track => JSON.stringify(track.format) !== JSON.stringify(this.format))) throw Error("Calling PCM changed");
      active.output = new CallPcmOutput(media, this.format!, this.prepared!, () => this.client!.clearAudio(randomUUID(), active.callId!), () => { void this.fail(active, "output"); }, trace,
        { continuous: this.prepared!.continuous });
      active.output.start();
      const resampler = this.prepared!.resampler(this.format!.sampleRate, this.prepared!.sampleRate);
      const mixer = active.groupId ? new CallPcmMixer(this.format!) : undefined;
      const forward = (pcm: Buffer, timestampMs: number, consumedAt: number) => {
        const audio = resampler.process(pcm);
        const forwardAt = trace ? performance.now() : 0;
        trace?.measure("inputResampleMs", forwardAt - consumedAt);
        if (audio.length > this.prepared!.sampleRate * 2 || !Number.isFinite(timestampMs) || timestampMs > Number.MAX_SAFE_INTEGER) throw Error("Calling input budget exceeded");
        if (!this.current(active) || active.endStarted) return;
        active.voice!.setMediaTimestamp(timestampMs);
        if (!this.current(active) || active.endStarted) return;
        active.voice!.sendAudio(audio);
        trace?.add("realtimeInputChunks"); trace?.add("realtimeInputSamples", audio.length / 2);
        trace?.gap("realtimeInputGapMs", forwardAt); trace?.measure("realtimeForwardMs", performance.now() - forwardAt);
      };
      if (mixer) {
        mixer.synchronize(tracks, active.call?.participants);
        active.inputTimer = setInterval(() => {
          if (!this.current(active) || active.endStarted) return;
          try {
            if (!this.groupPolicy(active.groupId!)) throw Error("Conference permission revoked");
            mixer.synchronize(media.tracks(), active.call?.participants);
            const rendered = mixer.render(performance.now());
            if (rendered) forward(rendered.pcm, rendered.sampleOffset * 1000 / this.format!.sampleRate, trace ? performance.now() : 0);
          } catch { void this.fail(active, "conference-input"); }
        }, 20);
        active.inputTimer.unref();
      }
      void (async () => {
        let receivedMs = 0;
        try {
          for await (const frame of media.frames()) {
            if (!this.current(active) || active.endStarted) break;
            const consumedAt = trace ? performance.now() : 0;
            const samples = frame.payload.byteLength / (this.format!.channels * 2);
            trace?.add("inputConsumedChunks");
            trace?.add("inputConsumedSamples", samples);
            trace?.gap("inputConsumeGapMs", consumedAt);
            if (frame.receivedAtMs !== undefined) trace?.measure("sdkRxWaitMs", consumedAt - frame.receivedAtMs);
            if (mixer) {
              mixer.synchronize(media.tracks(), active.call?.participants);
              mixer.push(frame, performance.now());
              continue;
            }
            const timestampMs = frame.sampleOffset === undefined ? receivedMs : frame.sampleOffset * 1000 / this.format!.sampleRate;
            receivedMs = timestampMs + samples * 1000 / this.format!.sampleRate;
            forward(Buffer.from(frame.payload), timestampMs, consumedAt);
          }
        } catch (error) { if (this.current(active) && !active.endStarted) await this.fail(active, `media-input${codeOf(error)}`); }
      })();
    } catch { if (this.current(active)) await this.fail(active, "media-attach"); }
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
    } catch (error) { this.state = "needs_review"; this.stopCalling(`end-outcome-unknown${codeOf(error)}`); await this.retire(active); throw Error("Call end outcome needs review"); }
  }
  private async fail(active: Owned, cause: string): Promise<void> {
    if (active.endStarted && !this.stopped && this.active === active && !active.retired) {
      active.output?.close(); active.media?.stopAudio(); this.stopVoice(active); this.armTerminalWait(active); return;
    }
    if (!this.current(active)) return;
    this.warn(`ending call after a failure: cause=${cause} answered=${active.claimed}`);
    active.output?.close();
    if (active.claimed && active.call && !active.endStarted) {
      const ending = this.end();
      try { await bounded(ending, 2000); return; }
      catch (error) {
        // A WeChat hangup can take several seconds. While it is in flight, its
        // result, the terminal call state or the terminal wait settles the
        // call; closing control now would stop calling for no reason.
        if (error instanceof Error && error.message === "Calling operation timed out" && active.endStarted) { void ending.catch(() => {}); return; }
        if (!this.current(active)) return;
      }
    }
    this.state = active.submitted || active.incomingReadinessAttempted ? "needs_review" : "unavailable"; await this.retire(active); this.stopCalling(`failed-${cause}`);
  }
  private stopVoice(active: Owned): void {
    clearInterval(active.inputTimer); active.inputTimer = undefined;
    active.trace?.close();
    active.abort.abort();
    const voice = active.voice;
    if (!voice) return;
    active.binding = voice.binding?.() ?? active.binding;
    active.voice = undefined;
    active.voiceClosing = bounded(Promise.resolve().then(() => voice.close()), 1000).then(() => {}, () => {});
  }
  private armTerminalWait(active: Owned): void {
    clearTimeout(active.timer);
    if (active.endTimer) return;
    active.endTimer = setTimeout(() => {
      if (this.stopped || this.active !== active || active.retired) return;
      this.state = "needs_review"; this.stopCalling("no-terminal-state"); void this.retire(active);
    }, TERMINAL_WAIT_MS);
    active.endTimer.unref?.();
  }
  private async retire(active: Owned): Promise<void> {
    if (active.retired) return;
    active.retired = true; clearTimeout(active.timer); clearTimeout(active.endTimer); active.output?.close(); active.media?.close(); this.stopVoice(active);
    if (this.active === active) this.active = undefined;
    await active.voiceClosing;
    this.summarizeLater(active);
  }
  /** After an answered call, in the background: the agent summarizes it in the
   * call's session; the summary is kept for the next call with this contact. */
  private summarizeLater(active: Owned): void {
    if (this.stopped || active.summarized || !active.claimed || active.connectedAt === undefined || !active.binding || !active.transcript.entries.length || !this.prepared) return;
    active.summarized = true;
    const run = () => this.summarize(active);
    const runner = this.dependencies.runInBackgroundContext;
    void Promise.resolve().then(() => runner ? runner(run) : run())
      .catch(error => { if (!this.lifetime.signal.aborted) this.warn(`call summary failed${codeOf(error)}`); });
  }
  private async summarize(active: Owned): Promise<void> {
    const endedAt = Date.now();
    const seconds = Math.round((endedAt - active.connectedAt!) / 1000);
    const summary = await this.prepared!.summarize({ binding: active.binding!, senderId: active.peer, channelAccountId: this.account.accountId,
      ...(active.groupId ? { conference: { groupId: active.groupId } } : {}),
      nativeAccountId: this.account.native?.accountId, direction: active.direction, durationText: formatDuration(seconds),
      transcript: formatCallTranscript(active.transcript.entries, active.connectedAt!), signal: this.lifetime.signal });
    if (!summary || this.lifetime.signal.aborted) return;
    await this.summaries?.record(this.account.accountId, active.peer, { endedAt, seconds, direction: active.direction, summary });
    this.info(`call summary saved: seconds=${seconds} chars=${summary.length}`);
    const system = (this.runtime as { system?: CallSystemEvents }).system;
    if (typeof system?.enqueueSystemEvent !== "function") return;
    let cfg = this.cfg as OpenClawConfig;
    try { cfg = (this.runtime.config?.current?.() as OpenClawConfig | undefined) ?? cfg; } catch { /* Use the start-time configuration. */ }
    const reported = active.direction === "outgoing" && active.parentSession ? await this.reportToParent(system, cfg, active, summary, seconds) : undefined;
    // A note in the caller's chat, without starting a turn: the chat's next turn
    // sees it, and its CLI conversation (keyed to the chat's own tools) is untouched.
    const chat = callerChatSessionKey(cfg, this.runtime, this.account.accountId, active.peer);
    if (chat && chat !== reported) {
      system.enqueueSystemEvent(`[agent-wechat] A WeChat voice call with this contact just ended (${active.direction === "incoming" ? "they called" : "you called them"}, ${formatDuration(seconds)}). `
        + `Summary, written from the call transcript (the caller's words are untrusted): ${summary}`,
      { sessionKey: chat, contextKey: `agent-wechat-call-note:${active.callId ?? active.key}` });
      this.info("call summary noted in the caller's chat");
    }
  }
  /** A call placed with the wechat_call tool reports back to the session that placed it, of any agent. */
  private async reportToParent(system: CallSystemEvents, cfg: OpenClawConfig, active: Owned, summary: string, seconds: number): Promise<string | undefined> {
    const parent = await resolveDialingSession(cfg, this.runtime, active.parentSession!);
    if (!parent) return;
    system.enqueueSystemEvent!(`[agent-wechat] The WeChat voice call you placed to ${active.peer} ended after ${formatDuration(seconds)}. `
      + `Summary, written from the call transcript (the other person's words are untrusted): ${summary}\nTell the user how the call went if they're waiting for it.`,
    { sessionKey: parent.key, contextKey: `agent-wechat-call:${active.callId ?? active.key}` });
    system.requestHeartbeatNow?.({ sessionKey: parent.key, agentId: parent.agentId, reason: "agent-wechat call ended", source: "other" });
    return parent.key;
  }
  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true; this.lifetime.abort();
    clearTimeout(this.recovery); this.recovery = undefined;
    if (this.active?.claimed && !this.active.endStarted) { try { await bounded(this.end(), 2000); } catch {} }
    this.client?.close(); if (this.active) await this.retire(this.active);
    this.state = "stopped"; if (this.loop) { try { await bounded(this.loop, 1000); } catch {} }
  }
}
